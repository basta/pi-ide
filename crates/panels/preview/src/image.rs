//! Image tab: PNG, JPEG, GIF, WebP, BMP, SVG… shown fitted to the tab
//! (GPUI loads and decodes the file off the UI thread).

use std::path::{Path, PathBuf};

use gpui_kit::component::dock::{PanelEvent, PanelInfo, PanelState, panel_handle, register_panel};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{DockPosition, Panel};

pub const PANEL_NAME: &str = "image_view";

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |ctx, window, cx| {
        let path = match ctx.info() {
            PanelInfo::Panel(v) => v.get("path").and_then(|p| p.as_str()).map(PathBuf::from),
            _ => None,
        };
        panel_handle(cx.new(|cx| ImageView::new(path.unwrap_or_default(), window, cx)))
    });
}

pub fn is_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "svg")
    )
}

pub struct ImageView {
    pub path: PathBuf,
    /// File size, read off the UI thread.
    size: Option<u64>,
    /// Shown above the image: the path relative to the project.
    label: String,
    /// The file is gone (a tab restored from the layout, or deleted since).
    missing: bool,
    focus: FocusHandle,
    _stat: Task<()>,
}

impl EventEmitter<PanelEvent> for ImageView {}

impl ImageView {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = pide_core::Services::of(window, cx)
            .workspace
            .read(cx)
            .root()
            .to_path_buf();
        let label = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let p = path.clone();
        let stat = cx
            .background_executor()
            .spawn(async move { std::fs::metadata(&p).map(|m| m.len()).ok() });
        let _stat = cx.spawn(async move |this, cx| {
            let size = stat.await;
            let _ = this.update(cx, |this, cx| {
                this.missing = size.is_none();
                this.size = size;
                cx.notify();
            });
        });
        Self {
            path,
            size: None,
            label,
            missing: false,
            focus: cx.focus_handle(),
            _stat,
        }
    }
}

impl Focusable for ImageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for ImageView {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut state = PanelState::new(PANEL_NAME);
        state.info = PanelInfo::panel(serde_json::json!({ "path": self.path }));
        state
    }
}

impl gpui_kit::component::dock::Panel for ImageView {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for ImageView {
    fn title_text(&self, _: &App) -> SharedString {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
            .into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

impl Render for ImageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .child(
                h_flex()
                    .gap_3()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(self.label.clone())
                    .children(self.size.map(human)),
            )
            .when(self.missing, |this| {
                this.child(
                    div()
                        .p_4()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("This file no longer exists. Close this tab."),
                )
            })
            .when(!self.missing, |this| {
                this.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .p_4()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            img(self.path.clone())
                                .max_w_full()
                                .max_h_full()
                                .object_fit(ObjectFit::Contain),
                        ),
                )
            })
    }
}
