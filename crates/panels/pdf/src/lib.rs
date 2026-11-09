//! PDF panel (spec M5): pages rendered by pdfium on a worker thread, zoom, text
//! search with highlights, and a page cache bounded at 256 MB. pdfium is a
//! native library pide installs on request (an Install button, like language
//! servers), into `~/.local/share/pide/pdfium`.

pub mod worker;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelInfo, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{DockPosition, Panel};

use worker::{Hit, PageSize};

pub const PANEL_NAME: &str = "pdf";
/// Rendered pages kept in memory, at most.
const CACHE_BYTES: usize = 256 * 1024 * 1024;
const PAGE_GAP: f32 = 12.;

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |ctx, window, cx| {
        let path = match ctx.info() {
            PanelInfo::Panel(v) => v.get("path").and_then(|p| p.as_str()).map(PathBuf::from),
            _ => None,
        };
        panel_handle(cx.new(|cx| PdfPanel::new(path.unwrap_or_default(), window, cx)))
    });
}

pub fn is_pdf(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

enum Doc {
    NotInstalled,
    Loading,
    Failed(String),
    Ready(Vec<PageSize>),
}

/// Rendered pages, least recently used first out, bounded by bytes.
#[derive(Default)]
struct Cache {
    images: HashMap<(usize, u32), Arc<RenderImage>>,
    order: VecDeque<(usize, u32)>,
    bytes: usize,
}

impl Cache {
    fn get(&mut self, key: (usize, u32)) -> Option<Arc<RenderImage>> {
        let img = self.images.get(&key)?.clone();
        if let Some(i) = self.order.iter().position(|k| *k == key) {
            self.order.remove(i);
        }
        self.order.push_back(key);
        Some(img)
    }

    /// Insert; returns evicted images (to free from the GPU atlas).
    fn insert(
        &mut self,
        key: (usize, u32),
        img: Arc<RenderImage>,
        size: usize,
    ) -> Vec<Arc<RenderImage>> {
        self.images.insert(key, img);
        self.order.push_back(key);
        self.bytes += size;
        let mut evicted = Vec::new();
        while self.bytes > CACHE_BYTES && self.order.len() > 1 {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(i) = self.images.remove(&old) {
                self.bytes = self
                    .bytes
                    .saturating_sub(i.size(0).width.0 as usize * i.size(0).height.0 as usize * 4);
                evicted.push(i);
            }
        }
        evicted
    }
}

struct Install {
    lines: Vec<String>,
    done: Option<Result<(), String>>,
}

pub struct PdfPanel {
    path: PathBuf,
    id: u64,
    doc: Doc,
    /// Logical pixels per PDF point.
    scale: f32,
    fit_width: bool,
    cache: Cache,
    pending: HashSet<(usize, u32)>,
    scroll: ScrollHandle,
    search: Entity<InputState>,
    hits: Vec<Hit>,
    current: usize,
    install: Option<Install>,
    /// Evicted pages, freed from the GPU atlas on the next frame.
    to_drop: Vec<Arc<RenderImage>>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for PdfPanel {}

impl PdfPanel {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search in PDF (Enter)"));
        let subs = vec![
            cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { shift, .. } = ev {
                    if this.hits.is_empty() {
                        this.run_search(cx);
                    } else {
                        this.step(if *shift { -1 } else { 1 }, cx);
                    }
                }
            }),
        ];
        let mut this = Self {
            path,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            doc: Doc::Loading,
            scale: 1.0,
            fit_width: true,
            cache: Cache::default(),
            pending: HashSet::new(),
            scroll: ScrollHandle::new(),
            search,
            hits: vec![],
            current: 0,
            install: None,
            to_drop: vec![],
            focus: cx.focus_handle(),
            _tasks: vec![],
            _subs: subs,
        };
        this.open(cx);
        this
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        if !worker::installed() {
            self.doc = Doc::NotInstalled;
            return;
        }
        let w = match worker::worker() {
            Ok(w) => w,
            Err(e) => {
                self.doc = Doc::Failed(e);
                return;
            }
        };
        self.doc = Doc::Loading;
        let rx = w.open(self.id, &self.path);
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = rx.await.unwrap_or(Err("pdfium stopped".into()));
            let _ = this.update(cx, |this, cx| {
                this.doc = match r {
                    Ok(sizes) if !sizes.is_empty() => Doc::Ready(sizes),
                    Ok(_) => Doc::Failed("This PDF has no pages.".into()),
                    Err(e) => Doc::Failed(e),
                };
                cx.notify();
            });
        }));
    }

    /// Download and unpack pdfium, then open the document. Only on the user's click.
    fn install(&mut self, cx: &mut Context<Self>) {
        let Some(url) = worker::download_url() else {
            self.install = Some(Install {
                lines: vec![],
                done: Some(Err("No pdfium build for this platform.".into())),
            });
            cx.notify();
            return;
        };
        let dir = worker::library_dir()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let tgz = std::env::temp_dir().join("pide-pdfium.tgz");
        let _ = std::fs::create_dir_all(&dir);
        self.install = Some(Install {
            lines: vec![format!("Downloading {url}")],
            done: None,
        });
        let steps = vec![
            vec![
                "curl".to_string(),
                "-fL".into(),
                "-o".into(),
                tgz.display().to_string(),
                url,
            ],
            vec![
                "tar".to_string(),
                "-xzf".into(),
                tgz.display().to_string(),
                "-C".into(),
                dir.display().to_string(),
            ],
        ];
        self._tasks.push(cx.spawn(async move |this, cx| {
            for step in steps {
                let mut run = pide_runtime::process::run_streaming(step, None);
                while let Some(line) = run.lines.recv().await {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(i) = this.install.as_mut() {
                            i.lines.push(line);
                            cx.notify();
                        }
                    });
                }
                let ok = matches!(run.done.await, Ok(Ok(Some(0))));
                if !ok {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(i) = this.install.as_mut() {
                            i.done = Some(Err("Install failed; see the output above.".into()));
                        }
                        cx.notify();
                    });
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if let Some(i) = this.install.as_mut() {
                    i.done = Some(Ok(()));
                }
                this.open(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let text = self.search.read(cx).value().to_string();
        self.hits.clear();
        self.current = 0;
        if text.trim().is_empty() {
            cx.notify();
            return;
        }
        let Ok(w) = worker::worker() else { return };
        let rx = w.search(self.id, &text, false);
        self._tasks.push(cx.spawn(async move |this, cx| {
            let hits = rx.await.ok().and_then(Result::ok).unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.hits = hits;
                this.reveal_current();
                cx.notify();
            });
        }));
    }

    fn step(&mut self, by: i64, cx: &mut Context<Self>) {
        if self.hits.is_empty() {
            return;
        }
        let n = self.hits.len() as i64;
        self.current = ((self.current as i64 + by).rem_euclid(n)) as usize;
        self.reveal_current();
        cx.notify();
    }

    /// Scroll so the current hit is in view.
    fn reveal_current(&mut self) {
        let (Doc::Ready(sizes), Some(hit)) = (&self.doc, self.hits.get(self.current)) else {
            return;
        };
        let top: f32 = sizes[..hit.page]
            .iter()
            .map(|s| s.height * self.scale + PAGE_GAP)
            .sum();
        let y = top + hit.rects.first().map(|r| r.y * self.scale).unwrap_or(0.) - 80.;
        self.scroll.set_offset(point(px(0.), px(-y.max(0.))));
    }

    fn zoom(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.fit_width = false;
        self.scale = (self.scale * factor).clamp(0.25, 6.0);
        cx.notify();
    }

    fn request(&mut self, page: usize, width_px: u32, cx: &mut Context<Self>) {
        let key = (page, width_px);
        if self.pending.contains(&key) {
            return;
        }
        let Ok(w) = worker::worker() else { return };
        self.pending.insert(key);
        let rx = w.render(self.id, page, width_px);
        let task = cx.spawn(async move |this, cx| {
            let r = rx.await;
            let _ = this.update(cx, |this, cx| {
                this.pending.remove(&key);
                match r {
                    Ok(Ok(b)) => {
                        let size = b.bgra.len();
                        let Some(buf) = image::RgbaImage::from_raw(b.width, b.height, b.bgra)
                        else {
                            return;
                        };
                        let img = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
                        let evicted = this.cache.insert(key, img, size);
                        this.to_drop.extend(evicted);
                        cx.notify();
                    }
                    Ok(Err(e)) => log::warn!("pdf render page {}: {e}", key.0),
                    Err(_) => {}
                }
            });
        });
        self._tasks.push(task);
        // Finished tasks pile up otherwise.
        if self._tasks.len() > 64 {
            self._tasks.drain(..32);
        }
    }
}

impl Focusable for PdfPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for PdfPanel {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut state = PanelState::new(PANEL_NAME);
        state.info = PanelInfo::panel(serde_json::json!({ "path": self.path }));
        state
    }
    fn on_removed(&mut self, _: &mut Window, _: &mut Context<Self>) {
        if let Ok(w) = worker::worker() {
            w.close(self.id);
        }
    }
}

impl gpui_kit::component::dock::Panel for PdfPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for PdfPanel {
    fn title_text(&self, _: &App) -> SharedString {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "PDF".into())
            .into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

impl Render for PdfPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for img in self.to_drop.drain(..) {
            let _ = window.drop_image(img);
        }
        let theme = cx.theme().clone();
        let body: AnyElement = match &self.doc {
            Doc::NotInstalled => self.render_install(cx),
            Doc::Loading => div()
                .p_4()
                .text_color(theme.muted_foreground)
                .child("Opening…")
                .into_any_element(),
            Doc::Failed(e) => div()
                .p_4()
                .text_color(theme.danger)
                .child(format!("Can't show this PDF: {e}"))
                .into_any_element(),
            Doc::Ready(sizes) => {
                let sizes = sizes.clone();
                self.render_pages(&sizes, window, cx)
            }
        };
        let page_count = match &self.doc {
            Doc::Ready(s) => s.len(),
            _ => 0,
        };
        let ready = page_count > 0;
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("zoom-out")
                            .xsmall()
                            .label("−")
                            .disabled(!ready)
                            .on_click(cx.listener(|t, _, _, cx| t.zoom(1. / 1.2, cx))),
                    )
                    .child(
                        div()
                            .w(px(48.))
                            .text_xs()
                            .text_center()
                            .child(format!("{:.0}%", self.scale * 100.)),
                    )
                    .child(
                        Button::new("zoom-in")
                            .xsmall()
                            .label("+")
                            .disabled(!ready)
                            .on_click(cx.listener(|t, _, _, cx| t.zoom(1.2, cx))),
                    )
                    .child(
                        Button::new("fit")
                            .xsmall()
                            .label("Fit width")
                            .when(self.fit_width, |b| b.primary())
                            .disabled(!ready)
                            .on_click(cx.listener(|t, _, _, cx| {
                                t.fit_width = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .px_2()
                            .child(if ready {
                                format!("{page_count} pages")
                            } else {
                                String::new()
                            }),
                    )
                    .child(div().flex_1())
                    .child(div().w(px(240.)).child(Input::new(&self.search).xsmall()))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if self.hits.is_empty() {
                            String::new()
                        } else {
                            format!("{}/{}", self.current + 1, self.hits.len())
                        },
                    ))
                    .child(
                        Button::new("prev")
                            .xsmall()
                            .label("↑")
                            .disabled(self.hits.is_empty())
                            .on_click(cx.listener(|t, _, _, cx| t.step(-1, cx))),
                    )
                    .child(
                        Button::new("next")
                            .xsmall()
                            .label("↓")
                            .disabled(self.hits.is_empty())
                            .on_click(cx.listener(|t, _, _, cx| t.step(1, cx))),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl PdfPanel {
    fn render_install(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let running = self.install.as_ref().is_some_and(|i| i.done.is_none());
        v_flex()
            .p_4()
            .gap_2()
            .max_w(px(640.))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("PDF viewing needs pdfium"))
            .child(div().text_sm().text_color(theme.muted_foreground).child(format!(
                "pdfium is the PDF engine from Chrome. pide downloads a prebuilt copy ({}, about 4 MB, from github.com/bblanchon/pdfium-binaries) into {}.",
                worker::PDFIUM_RELEASE,
                worker::library_dir().parent().map(|p| p.display().to_string()).unwrap_or_default()
            )))
            .child(h_flex().child(Button::new("install-pdfium").small().primary().label(if running { "Installing…" } else { "Install pdfium" }).disabled(running).on_click(
                cx.listener(|this, _, _, cx| this.install(cx)),
            )))
            .when_some(self.install.as_ref(), |this, i| {
                this.child(
                    v_flex()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.secondary)
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .child(i.lines.iter().rev().take(10).rev().cloned().collect::<Vec<_>>().join("\n"))
                        .when_some(i.done.clone(), |t, d| {
                            t.child(match d {
                                Ok(()) => div().text_color(theme.success).child("Installed."),
                                Err(e) => div().text_color(theme.danger).child(e),
                            })
                        }),
                )
            })
            .into_any_element()
    }

    fn render_pages(
        &mut self,
        sizes: &[PageSize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let viewport = self.scroll.bounds();
        if self.fit_width && viewport.size.width > px(0.) {
            let widest = sizes.iter().map(|s| s.width).fold(1., f32::max);
            self.scale = ((f32::from(viewport.size.width) - 48.) / widest).clamp(0.25, 6.0);
        }
        let scale = self.scale;
        let dpr = window.scale_factor();
        // Which pages are on screen (plus one either side): render those.
        let top = -f32::from(self.scroll.offset().y);
        let bottom = top + f32::from(viewport.size.height).max(800.);
        let mut y = 0.;
        let mut pages: Vec<AnyElement> = Vec::new();
        for (i, s) in sizes.iter().enumerate() {
            let (w, h) = (s.width * scale, s.height * scale);
            let near = y + h >= top - h && y <= bottom + h;
            let width_px = (w * dpr).round() as u32;
            let image = if near {
                match self.cache.get((i, width_px)) {
                    Some(img) => Some(img),
                    None => {
                        self.request(i, width_px, cx);
                        // Show any other size of this page while the right one renders.
                        self.cache
                            .images
                            .iter()
                            .find(|((p, _), _)| *p == i)
                            .map(|(_, img)| img.clone())
                    }
                }
            } else {
                None
            };
            let highlights: Vec<AnyElement> = self
                .hits
                .iter()
                .enumerate()
                .filter(|(_, h)| h.page == i)
                .flat_map(|(n, h)| {
                    let current = n == self.current;
                    let color = if current {
                        theme.warning.opacity(0.55)
                    } else {
                        theme.warning.opacity(0.25)
                    };
                    h.rects.iter().map(move |r| {
                        div()
                            .absolute()
                            .left(px(r.x * scale))
                            .top(px(r.y * scale))
                            .w(px(r.w * scale))
                            .h(px(r.h * scale))
                            .bg(color)
                            .into_any_element()
                    })
                })
                .collect();
            pages.push(
                div()
                    .relative()
                    .flex_none()
                    .w(px(w))
                    .h(px(h))
                    .bg(gpui_kit::white())
                    .shadow_sm()
                    .when_some(image, |d, img| {
                        d.child(gpui_kit::img(ImageSource::Render(img)).size_full())
                    })
                    .children(highlights)
                    .into_any_element(),
            );
            y += h + PAGE_GAP;
        }
        div()
            .id(("pdf-scroll", self.id))
            .size_full()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .bg(theme.secondary)
            .child(
                v_flex()
                    .items_center()
                    .gap(px(PAGE_GAP))
                    .py_4()
                    .px_6()
                    .children(pages),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn pdf_extension() {
        assert!(is_pdf(std::path::Path::new("a/B.PDF")));
        assert!(!is_pdf(std::path::Path::new("a.pd")));
    }
}
