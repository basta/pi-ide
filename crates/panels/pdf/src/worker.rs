//! The pdfium worker: one thread that owns the library and the open documents
//! (pdfium objects can't cross threads). The UI sends requests and awaits the
//! answers on oneshot channels; nothing here touches GPUI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

use pdfium_render::prelude::*;
use tokio::sync::oneshot;

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

enum Request {
    Open { id: u64, path: PathBuf, reply: oneshot::Sender<Result<Vec<PageSize>, String>> },
    Render { id: u64, page: usize, width_px: u32, reply: oneshot::Sender<Result<Bitmap, String>> },
    Search { id: u64, text: String, match_case: bool, reply: oneshot::Sender<Result<Vec<Hit>, String>> },
    Close { id: u64 },
}

/// Where the library lives once installed through pide.
pub fn library_dir() -> PathBuf {
    pide_core::paths::data_dir().join("pdfium/lib")
}

pub fn library_path() -> PathBuf {
    Pdfium::pdfium_platform_library_name_at_path(&library_dir())
}

pub fn installed() -> bool {
    library_path().is_file()
}

/// The pdfium build matching pdfium-render 0.9.5's `pdfium_8125` bindings.
pub const PDFIUM_RELEASE: &str = "chromium/8125";

pub fn download_url() -> Option<String> {
    let asset = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "pdfium-mac-arm64.tgz",
        ("macos", "x86_64") => "pdfium-mac-x64.tgz",
        ("linux", "x86_64") => "pdfium-linux-x64.tgz",
        ("linux", "aarch64") => "pdfium-linux-arm64.tgz",
        _ => return None,
    };
    Some(format!("https://github.com/bblanchon/pdfium-binaries/releases/download/{PDFIUM_RELEASE}/{asset}"))
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
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("pide-pdfium".into())
        .spawn(move || {
            let bindings = match Pdfium::bind_to_library(library_path()) {
                Ok(b) => b,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("can't load pdfium: {e}")));
                    return;
                }
            };
            let pdfium = Pdfium::new(bindings);
            let _ = ready_tx.send(Ok(()));
            let mut docs: HashMap<u64, PdfDocument<'_>> = HashMap::new();
            while let Ok(req) = rx.recv() {
                match req {
                    Request::Open { id, path, reply } => {
                        let r = pdfium.load_pdf_from_file(&path, None).map_err(|e| e.to_string()).map(|doc| {
                            let sizes = doc
                                .pages()
                                .iter()
                                .map(|p| PageSize { width: p.width().value, height: p.height().value })
                                .collect();
                            docs.insert(id, doc);
                            sizes
                        });
                        let _ = reply.send(r);
                    }
                    Request::Render { id, page, width_px, reply } => {
                        let _ = reply.send(render(&docs, id, page, width_px));
                    }
                    Request::Search { id, text, match_case, reply } => {
                        let _ = reply.send(search(&docs, id, &text, match_case));
                    }
                    Request::Close { id } => {
                        docs.remove(&id);
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    ready_rx.recv().map_err(|e| e.to_string())??;
    Ok(Worker { tx })
}

fn render(docs: &HashMap<u64, PdfDocument<'_>>, id: u64, page: usize, width_px: u32) -> Result<Bitmap, String> {
    let doc = docs.get(&id).ok_or("document closed")?;
    let p = doc.pages().get(page as PdfPageIndex).map_err(|e| e.to_string())?;
    let config = PdfRenderConfig::new().set_target_width(width_px.max(1) as Pixels).render_form_data(true);
    let bitmap = p.render_with_config(&config).map_err(|e| e.to_string())?;
    let rgba = bitmap.as_image().map_err(|e| e.to_string())?.into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut bgra = rgba.into_raw();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Ok(Bitmap { width, height, bgra })
}

fn search(docs: &HashMap<u64, PdfDocument<'_>>, id: u64, text: &str, match_case: bool) -> Result<Vec<Hit>, String> {
    let doc = docs.get(&id).ok_or("document closed")?;
    let options = PdfSearchOptions::new().match_case(match_case);
    let mut hits = Vec::new();
    for (i, page) in doc.pages().iter().enumerate() {
        let height = page.height().value;
        let Ok(page_text) = page.text() else { continue };
        let Ok(search) = page_text.search(text, &options) else { continue };
        for segments in search.iter(PdfSearchDirection::SearchForward) {
            let rects = segments
                .iter()
                .map(|s| {
                    let b = s.bounds();
                    Rect {
                        x: b.left().value,
                        y: height - b.top().value,
                        w: b.right().value - b.left().value,
                        h: b.top().value - b.bottom().value,
                    }
                })
                .collect();
            hits.push(Hit { page: i, rects });
            if hits.len() >= 5000 {
                return Ok(hits);
            }
        }
    }
    Ok(hits)
}

impl Worker {
    pub fn open(&self, id: u64, path: &Path) -> oneshot::Receiver<Result<Vec<PageSize>, String>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Request::Open { id, path: path.to_path_buf(), reply });
        rx
    }
    pub fn render(&self, id: u64, page: usize, width_px: u32) -> oneshot::Receiver<Result<Bitmap, String>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Request::Render { id, page, width_px, reply });
        rx
    }
    pub fn search(&self, id: u64, text: &str, match_case: bool) -> oneshot::Receiver<Result<Vec<Hit>, String>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Request::Search { id, text: text.to_string(), match_case, reply });
        rx
    }
    pub fn close(&self, id: u64) {
        let _ = self.tx.send(Request::Close { id });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against the installed pdfium (Install it from a PDF tab first). Not run
    /// by default: it loads the downloaded native library.
    /// `PIDE_TEST_PDF=/path/to/file.pdf cargo test -p pide-pdf -- --ignored`
    #[test]
    #[ignore]
    fn open_render_search_real_pdf() {
        let path = PathBuf::from(std::env::var("PIDE_TEST_PDF").expect("PIDE_TEST_PDF"));
        let w = worker().expect("pdfium");
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
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
