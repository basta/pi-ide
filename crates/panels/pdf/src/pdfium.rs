//! pdfium engine (Linux and other non-macOS platforms): a native library pide
//! downloads on request. Runs on the worker thread (`worker.rs`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use pdfium_render::prelude::*;

use crate::worker::{Bitmap, Hit, PageSize, Rect, Request, library_dir};

pub fn library_path() -> PathBuf {
    Pdfium::pdfium_platform_library_name_at_path(&library_dir())
}

pub(crate) fn serve(rx: Receiver<Request>, ready: Sender<Result<(), String>>) {
    let bindings = match Pdfium::bind_to_library(library_path()) {
        Ok(b) => b,
        Err(e) => {
            let _ = ready.send(Err(format!("can't load pdfium: {e}")));
            return;
        }
    };
    let pdfium = Pdfium::new(bindings);
    let _ = ready.send(Ok(()));
    let mut docs: HashMap<u64, PdfDocument<'_>> = HashMap::new();
    while let Ok(req) = rx.recv() {
        match req {
            Request::Open { id, path, reply } => {
                let r = pdfium
                    .load_pdf_from_file(&path, None)
                    .map_err(|e| e.to_string())
                    .map(|doc| {
                        let sizes = doc
                            .pages()
                            .iter()
                            .map(|p| PageSize {
                                width: p.width().value,
                                height: p.height().value,
                            })
                            .collect();
                        docs.insert(id, doc);
                        sizes
                    });
                let _ = reply.send(r);
            }
            Request::Render {
                id,
                page,
                width_px,
                reply,
            } => {
                let _ = reply.send(render(&docs, id, page, width_px));
            }
            Request::Search {
                id,
                text,
                match_case,
                reply,
            } => {
                let _ = reply.send(search(&docs, id, &text, match_case));
            }
            Request::Close { id } => {
                docs.remove(&id);
            }
        }
    }
}

fn render(
    docs: &HashMap<u64, PdfDocument<'_>>,
    id: u64,
    page: usize,
    width_px: u32,
) -> Result<Bitmap, String> {
    let doc = docs.get(&id).ok_or("document closed")?;
    let p = doc
        .pages()
        .get(page as PdfPageIndex)
        .map_err(|e| e.to_string())?;
    let config = PdfRenderConfig::new()
        .set_target_width(width_px.max(1) as Pixels)
        .render_form_data(true);
    let bitmap = p.render_with_config(&config).map_err(|e| e.to_string())?;
    let rgba = bitmap.as_image().map_err(|e| e.to_string())?.into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut bgra = rgba.into_raw();
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Ok(Bitmap {
        width,
        height,
        bgra,
    })
}

fn search(
    docs: &HashMap<u64, PdfDocument<'_>>,
    id: u64,
    text: &str,
    match_case: bool,
) -> Result<Vec<Hit>, String> {
    let doc = docs.get(&id).ok_or("document closed")?;
    let options = PdfSearchOptions::new().match_case(match_case);
    let mut hits = Vec::new();
    for (i, page) in doc.pages().iter().enumerate() {
        let height = page.height().value;
        let Ok(page_text) = page.text() else { continue };
        let Ok(search) = page_text.search(text, &options) else {
            continue;
        };
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

