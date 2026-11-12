//! PDF panel (spec M5): pages rendered on a worker thread (PDFKit on macOS,
//! pdfium elsewhere, installed on request into `~/.local/share/pide/pdfium`),
//! zoom, text search with highlights, and a page cache bounded at 256 MB.
//! Reloads when the file changes (a LaTeX build); SyncTeX: `reveal` scrolls to
//! a spot and marks it, Cmd+click jumps to the source line (`synctex edit`).

#[cfg(not(target_os = "macos"))]
mod pdfium;
#[cfg(target_os = "macos")]
mod pdfkit;
pub mod worker;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelInfo, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::opener::PdfSpot;
use pide_core::{DockPosition, Panel};

use worker::{Hit, PageSize};

pub const PANEL_NAME: &str = "pdf";
/// Rendered pages kept in memory, at most.
const CACHE_BYTES: usize = 256 * 1024 * 1024;
const PAGE_GAP: f32 = 12.;
/// Space above the first page (`py_4`).
const TOP_PAD: f32 = 16.;
/// How often to look for a rebuilt file.
const WATCH_EVERY: Duration = Duration::from_millis(700);
/// How long a SyncTeX mark stays.
const MARK_FOR: Duration = Duration::from_millis(1800);

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

/// Every PDF tab, also those rebuilt from a saved layout (to find one by path).
#[derive(Default)]
struct OpenPdfs(Vec<WeakEntity<PdfPanel>>);
impl Global for OpenPdfs {}

/// This window's open tab for `path`, if any.
pub fn find(path: &Path, window: &Window, cx: &App) -> Option<Entity<PdfPanel>> {
    let id = window.window_handle().window_id();
    cx.try_global::<OpenPdfs>()?
        .0
        .iter()
        .filter_map(WeakEntity::upgrade)
        .find(|p| {
            let p = p.read(cx);
            p.window_id == id && p.path == path
        })
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
    window_id: WindowId,
    /// Stable, for element ids.
    id: u64,
    /// The worker's document; a reload opens the new file under a new one.
    doc_id: u64,
    /// The file's modification time when it was opened.
    loaded: Option<SystemTime>,
    /// A reload in flight: keep showing the old pages until it's ready.
    reloading: bool,
    /// Where SyncTeX pointed, until when.
    mark: Option<(PdfSpot, Instant)>,
    /// On-screen bounds of the pages drawn last frame (for Cmd+click).
    page_bounds: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
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
    /// Pages rendered before a reload, shown until the new ones are ready.
    stale: Vec<((usize, u32), Arc<RenderImage>)>,
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
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let me = cx.entity().downgrade();
        let open = &mut cx.default_global::<OpenPdfs>().0;
        open.retain(|w| w.upgrade().is_some());
        open.push(me);
        let mut this = Self {
            path,
            window_id: window.window_handle().window_id(),
            id,
            doc_id: id,
            loaded: None,
            reloading: false,
            mark: None,
            page_bounds: Rc::default(),
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
            stale: vec![],
            to_drop: vec![],
            focus: cx.focus_handle(),
            _tasks: vec![],
            _subs: subs,
        };
        this.open(cx);
        this.watch(cx);
        this
    }

    /// Reload when the file changes on disk and has settled (same time and
    /// size twice in a row: latexmk writes the PDF in pieces).
    fn watch(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self._tasks.push(cx.spawn(async move |this, cx| {
            let mut last: Option<(SystemTime, u64)> = None;
            loop {
                cx.background_executor().timer(WATCH_EVERY).await;
                let p = path.clone();
                let now = cx
                    .background_executor()
                    .spawn(async move { stamp(&p) })
                    .await;
                let settled = now.is_some() && now == last;
                last = now;
                let Ok(()) = this.update(cx, |this, cx| {
                    if let (true, Some((t, _))) = (settled, now)
                        && this.loaded.is_some_and(|l| l != t)
                        && !this.reloading
                    {
                        this.reload(cx);
                    }
                }) else {
                    return; // the panel is gone
                };
            }
        }));
    }

    /// Read the file again, keeping the zoom and scroll position.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.doc, Doc::Ready(_)) {
            self.open(cx);
            return;
        }
        let Ok(w) = worker::worker() else { return };
        let old = self.doc_id;
        let new = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        self.reloading = true;
        let path = self.path.clone();
        let rx = w.open(new, &path);
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = rx.await.unwrap_or(Err("the PDF engine stopped".into()));
            let modified = cx
                .background_executor()
                .spawn(async move { stamp(&path).map(|(t, _)| t) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.reloading = false;
                match r {
                    Ok(sizes) if !sizes.is_empty() => {
                        w.close(old);
                        this.doc_id = new;
                        this.loaded = modified;
                        this.doc = Doc::Ready(sizes);
                        // Old pages stay up (as stand-ins) until the new ones render.
                        let stale = std::mem::take(&mut this.cache);
                        this.stale = stale.images.into_iter().collect();
                        this.pending.clear();
                        if !this.hits.is_empty() {
                            this.run_search(cx);
                        }
                    }
                    // Half-written or broken: keep the old one, try on the next change.
                    _ => w.close(new),
                }
                cx.notify();
            });
        }));
    }

    /// Scroll to a SyncTeX spot and mark it for a moment.
    pub fn reveal(&mut self, spot: PdfSpot, cx: &mut Context<Self>) {
        let Doc::Ready(sizes) = &self.doc else {
            // Not open yet: try again once it is.
            self._tasks.push(cx.spawn(async move |this, cx| {
                for _ in 0..50 {
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                    let ready = this
                        .update(cx, |this, cx| {
                            let ready = matches!(this.doc, Doc::Ready(_));
                            if ready {
                                this.reveal(spot, cx);
                            }
                            ready
                        })
                        .unwrap_or(true);
                    if ready {
                        return;
                    }
                }
            }));
            return;
        };
        let Some(page) = sizes.get(spot.page) else { return };
        let page_top: f32 = TOP_PAD
            + sizes[..spot.page]
                .iter()
                .map(|s| s.height * self.scale + PAGE_GAP)
                .sum::<f32>();
        let view_h = f32::from(self.scroll.bounds().size.height).max(200.);
        let y = page_top + spot.y.min(page.height) * self.scale - view_h / 3.;
        self.scroll.set_offset(point(px(0.), px(-y.max(0.))));
        self.mark = Some((spot, Instant::now() + MARK_FOR));
        self._tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MARK_FOR).await;
            let _ = this.update(cx, |this, cx| {
                if this.mark.is_some_and(|(_, until)| until <= Instant::now()) {
                    this.mark = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// Cmd+click: the source line under the click (`synctex edit`), opened in the editor.
    fn inverse_search(
        &mut self,
        page: usize,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(origin) = self.page_bounds.borrow().get(&page).map(|b| b.origin) else {
            return;
        };
        let x = f32::from(at.x - origin.x) / self.scale;
        let y = f32::from(at.y - origin.y) / self.scale;
        let pdf = self.path.clone();
        let work = cx
            .background_executor()
            .spawn(async move { synctex_edit(&pdf, page, x, y) });
        cx.spawn_in(window, async move |_, cx| {
            let found = work.await;
            let _ = cx.update(|window, cx| match found {
                Some((file, line)) => {
                    pide_core::opener::open_file_at(file, line.saturating_sub(1), 0, window, cx)
                }
                None => window.push_notification(
                    gpui_kit::component::notification::Notification::info(
                        "SyncTeX found no source here. Was the PDF built with -synctex=1?",
                    ),
                    cx,
                ),
            });
        })
        .detach();
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
        let rx = w.open(self.doc_id, &self.path);
        let path = self.path.clone();
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = rx.await.unwrap_or(Err("the PDF engine stopped".into()));
            let modified = cx
                .background_executor()
                .spawn(async move { stamp(&path).map(|(t, _)| t) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loaded = modified;
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
        let rx = w.search(self.doc_id, &text, false);
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
        let y = TOP_PAD + top + hit.rects.first().map(|r| r.y * self.scale).unwrap_or(0.) - 80.;
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
        let doc_id = self.doc_id;
        let rx = w.render(doc_id, page, width_px);
        let task = cx.spawn(async move |this, cx| {
            let r = rx.await;
            let _ = this.update(cx, |this, cx| {
                if this.doc_id != doc_id {
                    return; // rendered from the file before a reload
                }
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
                        // This page's pre-reload stand-in can go.
                        let (gone, keep): (Vec<_>, Vec<_>) =
                            std::mem::take(&mut this.stale).into_iter().partition(|((p, _), _)| *p == key.0);
                        this.stale = keep;
                        this.to_drop.extend(gone.into_iter().map(|(_, i)| i));
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
            w.close(self.doc_id);
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
                            .map(|b| pide_core::colors::toggle(b, self.fit_width, cx))
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
        self.page_bounds.borrow_mut().clear();
        for (i, s) in sizes.iter().enumerate() {
            let (w, h) = (s.width * scale, s.height * scale);
            let near = y + h >= top - h && y <= bottom + h;
            let width_px = (w * dpr).round() as u32;
            let image = if near {
                match self.cache.get((i, width_px)) {
                    Some(img) => Some(img),
                    None => {
                        self.request(i, width_px, cx);
                        // Show any other size of this page (or the page from
                        // before a reload) while the right one renders.
                        self.cache
                            .images
                            .iter()
                            .chain(self.stale.iter().map(|(k, v)| (k, v)))
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
            let mark = self
                .mark
                .filter(|(m, until)| m.page == i && *until > Instant::now())
                .map(|(m, _)| {
                    // The line SyncTeX points at: a band across the page, and a dot.
                    let accent = theme.danger;
                    div()
                        .absolute()
                        .left_0()
                        .top(px((m.y - 11.) * scale))
                        .w_full()
                        .h(px(14. * scale))
                        .bg(accent.opacity(0.12))
                        .child(
                            div()
                                .absolute()
                                .left(px(m.x * scale - 10.))
                                .top(px(7. * scale - 4.))
                                .size(px(8.))
                                .rounded_full()
                                .bg(accent),
                        )
                });
            let bounds = self.page_bounds.clone();
            pages.push(
                div()
                    .id(("pdf-page", i))
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
                    .children(mark)
                    .when(near, |d| {
                        // Record where this page is on screen, for Cmd+click.
                        d.child(
                            canvas(
                                move |b, _, _| {
                                    bounds.borrow_mut().insert(i, b);
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            if ev.modifiers.platform {
                                this.inverse_search(i, ev.position, window, cx);
                            }
                        }),
                    )
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

/// The file's modification time and size, if it's there.
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// The source file and 1-based line at a point on a page (0-based page, PDF
/// points from its top-left). Blocking.
fn synctex_edit(pdf: &Path, page: usize, x: f32, y: f32) -> Option<(PathBuf, u32)> {
    let exe = pide_runtime::process::find_command("synctex").unwrap_or_else(|| "synctex".into());
    let out = std::process::Command::new(exe)
        .arg("edit")
        .arg("-o")
        .arg(format!("{}:{x}:{y}:{}", page + 1, pdf.display()))
        .output()
        .ok()?;
    parse_edit(&String::from_utf8_lossy(&out.stdout))
}

fn parse_edit(out: &str) -> Option<(PathBuf, u32)> {
    let field = |name: &str| {
        out.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':').map(str::trim))
    };
    let input = field("Input")?;
    let line: u32 = field("Line")?.parse().ok()?;
    // synctex writes `dir/./file.tex`; open the plain path.
    let path: PathBuf = Path::new(input)
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect();
    Some((path, line.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn edit_output() {
        let out = "This is SyncTeX\nSyncTeX result begin\nOutput:report.pdf\nInput:/a/b/./sections/one.tex\nLine:104\nColumn:-1\nSyncTeX result end\n";
        assert_eq!(
            parse_edit(out),
            Some((PathBuf::from("/a/b/sections/one.tex"), 104))
        );
        assert_eq!(parse_edit("SyncTeX ERROR"), None);
    }

    #[::core::prelude::v1::test]
    fn pdf_extension() {
        assert!(is_pdf(std::path::Path::new("a/B.PDF")));
        assert!(!is_pdf(std::path::Path::new("a.pd")));
    }
}
