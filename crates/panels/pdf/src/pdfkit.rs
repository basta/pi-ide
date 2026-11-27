//! PDFKit engine (macOS): Apple's PDF framework, part of the OS, so nothing to
//! install. Runs on the worker thread (`worker.rs`); the documents never leave
//! it. Pages are drawn into a CoreGraphics bitmap (BGRA, white background).

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};

use objc2::AnyThread as _;
use objc2::rc::{Retained, autoreleasepool};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
};
use objc2_foundation::{NSRect, NSString, NSStringCompareOptions, NSURL};
use objc2_pdf_kit::{PDFDisplayBox, PDFDocument, PDFPage};

use crate::worker::{Bitmap, Hit, PageSize, Rect, Request};

/// Most hits a search returns (like the pdfium engine).
const MAX_HITS: usize = 5000;

pub(crate) fn serve(rx: Receiver<Request>, ready: Sender<Result<(), String>>) {
    let _ = ready.send(Ok(()));
    let mut docs: HashMap<u64, Retained<PDFDocument>> = HashMap::new();
    while let Ok(req) = rx.recv() {
        autoreleasepool(|_| match req {
            Request::Open { id, path, reply } => {
                let r = open(&path).map(|doc| {
                    let sizes = sizes(&doc);
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
        });
    }
}

fn open(path: &Path) -> Result<Retained<PDFDocument>, String> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    unsafe { PDFDocument::initWithURL(PDFDocument::alloc(), &url) }
        .ok_or_else(|| format!("can't open {} as a PDF", path.display()))
}

fn page(doc: &PDFDocument, index: usize) -> Result<Retained<PDFPage>, String> {
    unsafe { doc.pageAtIndex(index) }.ok_or_else(|| format!("no page {}", index + 1))
}

/// The page's crop box (what viewers show), in PDF page space.
fn crop(page: &PDFPage) -> NSRect {
    unsafe { page.boundsForBox(PDFDisplayBox::CropBox) }
}

/// Whether the page is turned a quarter, so it shows as wide as it is tall.
fn sideways(page: &PDFPage) -> bool {
    unsafe { page.rotation() }.rem_euclid(180) == 90
}

/// The size a page shows at, rotation applied.
fn shown(page: &PDFPage) -> PageSize {
    let b = crop(page);
    let (w, h) = (b.size.width as f32, b.size.height as f32);
    if sideways(page) {
        PageSize {
            width: h,
            height: w,
        }
    } else {
        PageSize {
            width: w,
            height: h,
        }
    }
}

fn sizes(doc: &PDFDocument) -> Vec<PageSize> {
    (0..unsafe { doc.pageCount() })
        .filter_map(|i| page(doc, i).ok())
        .map(|p| shown(&p))
        .collect()
}

fn render(
    docs: &HashMap<u64, Retained<PDFDocument>>,
    id: u64,
    index: usize,
    width_px: u32,
) -> Result<Bitmap, String> {
    let doc = docs.get(&id).ok_or("document closed")?;
    let p = page(doc, index)?;
    let size = shown(&p);
    let width = width_px.max(1);
    let scale = width as f64 / size.width.max(1.0) as f64;
    let height = ((size.height as f64 * scale).round() as u32).max(1);
    let stride = width as usize * 4;
    let mut bgra = vec![0u8; stride * height as usize];
    // Little-endian 32-bit, alpha first = B, G, R, A in memory, rows top-down.
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    let space = CGColorSpace::new_device_rgb().ok_or("no RGB color space")?;
    let ctx = unsafe {
        CGBitmapContextCreate(
            bgra.as_mut_ptr().cast(),
            width as usize,
            height as usize,
            8,
            stride,
            Some(&space),
            info,
        )
    }
    .ok_or("can't make a bitmap")?;
    let full = CGRect::new(
        CGPoint::new(0.0, 0.0),
        CGSize::new(width as f64, height as f64),
    );
    CGContext::set_rgb_fill_color(Some(&ctx), 1.0, 1.0, 1.0, 1.0);
    CGContext::fill_rect(Some(&ctx), full);
    CGContext::scale_ctm(Some(&ctx), scale, scale);
    // Draws the crop box at the origin, rotation applied, annotations included.
    unsafe { p.drawWithBox_toContext(PDFDisplayBox::CropBox, &ctx) };
    drop(ctx);
    Ok(Bitmap {
        width,
        height,
        bgra,
    })
}

/// A rectangle in PDF page space (origin bottom-left) → points from the shown
/// page's top-left corner, rotation applied.
fn to_shown(page: &PDFPage, r: NSRect) -> Rect {
    let b = crop(page);
    let (x0, y0) = (b.origin.x as f32, b.origin.y as f32);
    let (bw, bh) = (b.size.width as f32, b.size.height as f32);
    let (x, y) = (r.origin.x as f32 - x0, r.origin.y as f32 - y0);
    let (w, h) = (r.size.width as f32, r.size.height as f32);
    match unsafe { page.rotation() }.rem_euclid(360) {
        90 => Rect {
            x: y,
            y: x,
            w: h,
            h: w,
        },
        180 => Rect {
            x: bw - x - w,
            y: y,
            w,
            h,
        },
        270 => Rect {
            x: bh - y - h,
            y: bw - x - w,
            w: h,
            h: w,
        },
        _ => Rect {
            x,
            y: bh - y - h,
            w,
            h,
        },
    }
}

fn search(
    docs: &HashMap<u64, Retained<PDFDocument>>,
    id: u64,
    text: &str,
    match_case: bool,
) -> Result<Vec<Hit>, String> {
    let doc = docs.get(&id).ok_or("document closed")?;
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let options = if match_case {
        NSStringCompareOptions::empty()
    } else {
        NSStringCompareOptions::CaseInsensitiveSearch
    };
    let found = unsafe { doc.findString_withOptions(&NSString::from_str(text), options) };
    let mut hits = Vec::new();
    for sel in found.iter() {
        let pages = unsafe { sel.pages() };
        let Some(p) = pages.iter().next() else {
            continue;
        };
        let index = unsafe { doc.indexForPage(&p) };
        let rects = unsafe { sel.selectionsByLine() }
            .iter()
            .map(|line| to_shown(&p, unsafe { line.boundsForPage(&p) }))
            .filter(|r| r.w > 0.0 && r.h > 0.0)
            .collect::<Vec<_>>();
        if rects.is_empty() {
            continue;
        }
        hits.push(Hit { page: index, rects });
        if hits.len() >= MAX_HITS {
            break;
        }
    }
    hits.sort_by_key(|h| h.page);
    Ok(hits)
}
