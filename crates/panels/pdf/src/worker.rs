//! The PDF worker: one thread that owns the engine and the open documents
//! (neither pdfium's nor PDFKit's objects can cross threads). The UI sends
//! requests and awaits the answers on oneshot channels; nothing here touches
//! GPUI. The engine is Apple's PDFKit on macOS (`pdfkit.rs`, part of the OS)
//! and pdfium elsewhere (`pdfium.rs`, downloaded on request).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

use tokio::sync::oneshot;

#[cfg(not(target_os = "macos"))]
use crate::pdfium as engine;
#[cfg(target_os = "macos")]
use crate::pdfkit as engine;

/// One page's size in PDF points (1/72 inch).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

/// A rectangle on a page, in points from the page's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One search hit: its page and the boxes covering it (one per line it spans).
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub page: usize,
    pub rects: Vec<Rect>,
}

/// A rendered page, BGRA, `width * height * 4` bytes.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub(crate) enum Request {
    Open {
        id: u64,
        path: PathBuf,
        reply: oneshot::Sender<Result<Vec<PageSize>, String>>,
    },
    Render {
        id: u64,
        page: usize,
        width_px: u32,
        reply: oneshot::Sender<Result<Bitmap, String>>,
    },
    Search {
        id: u64,
        text: String,
        match_case: bool,
        reply: oneshot::Sender<Result<Vec<Hit>, String>>,
    },
    Close {
        id: u64,
    },
}

/// Where pdfium lives once installed through pide (not used on macOS).
pub fn library_dir() -> PathBuf {
    // pdfium-binaries puts the Windows DLL in bin/, the other platforms' library in lib/.
    let sub = if cfg!(windows) {
        "pdfium/bin"
    } else {
        "pdfium/lib"
    };
    pide_core::paths::data_dir().join(sub)
}

/// Whether the engine is there. PDFKit always is.
pub fn installed() -> bool {
    #[cfg(target_os = "macos")]
    return true;
    #[cfg(not(target_os = "macos"))]
    return crate::pdfium::library_path().is_file();
}

/// The pdfium build matching pdfium-render 0.9.5's `pdfium_8125` bindings.
pub const PDFIUM_RELEASE: &str = "chromium/8125";

pub fn download_url() -> Option<String> {
    let asset = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "pdfium-mac-arm64.tgz",
        ("macos", "x86_64") => "pdfium-mac-x64.tgz",
        ("linux", "x86_64") => "pdfium-linux-x64.tgz",
        ("linux", "aarch64") => "pdfium-linux-arm64.tgz",
        ("windows", "x86_64") => "pdfium-win-x64.tgz",
        ("windows", "aarch64") => "pdfium-win-arm64.tgz",
        _ => return None,
    };
    Some(format!(
        "https://github.com/bblanchon/pdfium-binaries/releases/download/{PDFIUM_RELEASE}/{asset}"
    ))
}

#[derive(Clone)]
pub struct Worker {
    tx: Sender<Request>,
}

static WORKER: OnceLock<Result<Worker, String>> = OnceLock::new();

/// The shared worker, started on first use. Errors if pdfium isn't installed or won't load.
pub fn worker() -> Result<Worker, String> {
    if let Some(w) = WORKER.get() {
        return w.clone();
    }
    if !installed() {
        return Err("pdfium isn't installed".into());
    }
    WORKER.get_or_init(start).clone()
}

fn start() -> Result<Worker, String> {
    let (tx, rx) = channel::<Request>();
    let (ready_tx, ready_rx) = channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("pide-pdf".into())
        .spawn(move || engine::serve(rx, ready_tx))
        .map_err(|e| e.to_string())?;
    ready_rx.recv().map_err(|e| e.to_string())??;
    Ok(Worker { tx })
}

impl Worker {
    pub fn open(&self, id: u64, path: &Path) -> oneshot::Receiver<Result<Vec<PageSize>, String>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Request::Open {
            id,
            path: path.to_path_buf(),
            reply,
        });
        rx
    }
    pub fn render(
        &self,
        id: u64,
        page: usize,
        width_px: u32,
    ) -> oneshot::Receiver<Result<Bitmap, String>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Request::Render {
            id,
            page,
            width_px,
            reply,
        });
        rx
    }
    pub fn search(
        &self,
        id: u64,
        text: &str,
        match_case: bool,
    ) -> oneshot::Receiver<Result<Vec<Hit>, String>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Request::Search {
            id,
            text: text.to_string(),
            match_case,
            reply,
        });
        rx
    }
    pub fn close(&self, id: u64) {
        let _ = self.tx.send(Request::Close { id });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against a real PDF (and, off macOS, the installed pdfium). Not run by
    /// default: it needs a file.
    /// `PIDE_TEST_PDF=/path/to/file.pdf cargo test -p pide-pdf -- --ignored`
    #[test]
    #[ignore]
    fn open_render_search_real_pdf() {
        let path = PathBuf::from(std::env::var("PIDE_TEST_PDF").expect("PIDE_TEST_PDF"));
        let w = worker().expect("engine");
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let sizes = rt.block_on(w.open(1, &path)).unwrap().unwrap();
        assert!(!sizes.is_empty());
        let bmp = rt.block_on(w.render(1, 0, 600)).unwrap().unwrap();
        assert_eq!(bmp.width, 600);
        assert_eq!(bmp.bgra.len(), (bmp.width * bmp.height * 4) as usize);
        let hits = rt.block_on(w.search(1, "the", false)).unwrap().unwrap();
        eprintln!("{} pages, {} hits for \"the\"", sizes.len(), hits.len());
        w.close(1);
    }
}
