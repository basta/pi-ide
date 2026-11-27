//! Markdown preview (spec M5): renders a Markdown buffer with GPUI Kit's text
//! view and follows it as you type (unsaved edits included).

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::dock::{PanelEvent, PanelInfo, PanelState, panel_handle, register_panel};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::*;
use pide_core::{Buffer, DockPosition, Panel, Services};

pub mod image;

pub const PANEL_NAME: &str = "markdown_preview";

pub fn init(cx: &mut App) {
    image::init(cx);
    register_panel(cx, PANEL_NAME, |ctx, window, cx| {
        let path = match ctx.info() {
            PanelInfo::Panel(v) => v.get("path").and_then(|p| p.as_str()).map(PathBuf::from),
            _ => None,
        };
        panel_handle(cx.new(|cx| MarkdownPreview::new(path.unwrap_or_default(), window, cx)))
    });
}

pub fn is_markdown(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md" | "markdown" | "mdx")
    )
}

pub struct MarkdownPreview {
    pub path: PathBuf,
    view: Entity<TextViewState>,
    error: Option<SharedString>,
    focus: FocusHandle,
    _load: Option<Task<()>>,
    _update: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for MarkdownPreview {}

impl MarkdownPreview {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let view = cx.new(|cx| TextViewState::markdown("", cx));
        let mut this = Self {
            path: path.clone(),
            view,
            error: None,
            focus: cx.focus_handle(),
            _load: None,
            _update: None,
            _subs: vec![],
        };
        if path.as_os_str().is_empty() {
            this.error = Some("This preview's file is unknown.".into());
            return this;
        }
        let workspace = Services::of(window, cx).workspace;
        let open = workspace.update(cx, |w, cx| w.open(path, cx));
        this._load = Some(cx.spawn(async move |this, cx| {
            let result = open.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(buffer) => this.attach(buffer, cx),
                Err(e) => {
                    this.error = Some(format!("Can't open: {e:#}").into());
                    cx.notify();
                }
            });
        }));
        this
    }

    fn attach(&mut self, buffer: Entity<Buffer>, cx: &mut Context<Self>) {
        let text = buffer.read(cx).text().to_string();
        self.view.update(cx, |v, cx| {
            v.set_text(&pide_core::math::normalize_delimiters(&text), cx)
        });
        self._subs
            .push(cx.observe(&buffer, |this, buffer, cx| this.schedule(buffer, cx)));
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    /// Re-render shortly after the last change (typing stays smooth in big files).
    fn schedule(&mut self, buffer: Entity<Buffer>, cx: &mut Context<Self>) {
        self._update = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let _ = this.update(cx, |this, cx| {
                let text = buffer.read(cx).text().to_string();
                this.view.update(cx, |v, cx| {
                    v.set_text(&pide_core::math::normalize_delimiters(&text), cx)
                });
                cx.notify();
            });
        }));
    }
}

impl Focusable for MarkdownPreview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for MarkdownPreview {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut state = PanelState::new(PANEL_NAME);
        state.info = PanelInfo::panel(serde_json::json!({ "path": self.path }));
        state
    }
}

impl gpui_kit::component::dock::Panel for MarkdownPreview {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for MarkdownPreview {
    fn title_text(&self, _: &App) -> SharedString {
        format!(
            "Preview {}",
            self.path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        )
        .into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

impl Render for MarkdownPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .id("md-preview")
            .size_full()
            .track_focus(&self.focus)
            .overflow_y_scroll()
            .bg(theme.background)
            .px_6()
            .py_4()
            .child(match &self.error {
                Some(e) => div()
                    .text_color(theme.muted_foreground)
                    .child(e.clone())
                    .into_any_element(),
                None => div()
                    .max_w(px(820.))
                    .child(
                        TextView::new(&self.view)
                            .markdown_extensions(pide_core::math::markdown_extensions())
                            .selectable(true),
                    )
                    .into_any_element(),
            })
    }
}
