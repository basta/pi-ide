//! A list of places in files under a title ("Usages of greet"), in the bottom
//! dock. Filled through `pide_core::opener::show_locations`.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::*;
use pide_core::opener::Location;
use pide_core::{DockPosition, Panel, Services};

pub const PANEL_NAME: &str = "locations";

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| Locations::new(window, cx)))
    });
}

#[derive(Default)]
struct Instances(HashMap<WindowId, WeakEntity<Locations>>);
impl Global for Instances {}

/// This window's Usages panel, if one exists (it may be restored from the layout).
pub fn existing(window: &Window, cx: &App) -> Option<Entity<Locations>> {
    cx.try_global::<Instances>()?
        .0
        .get(&window.window_handle().window_id())?
        .upgrade()
}

pub struct Locations {
    root: PathBuf,
    title: SharedString,
    items: Vec<Location>,
    focus: FocusHandle,
}

impl EventEmitter<PanelEvent> for Locations {}

impl Locations {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = Services::of(window, cx)
            .workspace
            .read(cx)
            .root()
            .to_path_buf();
        let me = cx.entity().downgrade();
        cx.default_global::<Instances>()
            .0
            .insert(window.window_handle().window_id(), me);
        Self {
            root,
            title: "Usages".into(),
            items: vec![],
            focus: cx.focus_handle(),
        }
    }

    pub fn set(&mut self, title: SharedString, items: Vec<Location>, cx: &mut Context<Self>) {
        self.title = title;
        self.items = items;
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }
}

impl Focusable for Locations {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for Locations {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for Locations {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for Locations {
    fn title_text(&self, _: &App) -> SharedString {
        self.title.clone()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Bottom
    }
}

impl Render for Locations {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut last: Option<&PathBuf> = None;
        let files = {
            let mut f: Vec<&PathBuf> = self.items.iter().map(|l| &l.path).collect();
            f.dedup();
            f.len()
        };
        for (i, l) in self.items.iter().enumerate() {
            if last != Some(&l.path) {
                let n = self.items.iter().filter(|x| x.path == l.path).count();
                let rel = l
                    .path
                    .strip_prefix(&self.root)
                    .unwrap_or(&l.path)
                    .display()
                    .to_string();
                rows.push(
                    h_flex()
                        .gap_2()
                        .px_3()
                        .pt_1p5()
                        .text_sm()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(rel))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(n.to_string()),
                        )
                        .into_any_element(),
                );
                last = Some(&l.path);
            }
            let r = l.range.start.min(l.text.len())..l.range.end.min(l.text.len());
            let (before, hit, after) = (
                l.text[..r.start].trim_start().to_string(),
                l.text[r.clone()].to_string(),
                l.text[r.end..].to_string(),
            );
            let (path, line, ch) = (l.path.clone(), l.line, l.character);
            rows.push(
                h_flex()
                    .id(("loc", i))
                    .gap_2()
                    .pl_6()
                    .pr_3()
                    .py_0p5()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .on_click(move |_, window, cx| {
                        pide_core::opener::open_file_at(path.clone(), line, ch, window, cx)
                    })
                    .child(
                        div()
                            .w(px(44.))
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child((line + 1).to_string()),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .child(before)
                            .child(div().bg(theme.warning.opacity(0.35)).child(hit))
                            .child(after),
                    )
                    .into_any_element(),
            );
        }
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if self.items.is_empty() {
                        format!(
                            "Put the cursor on a symbol and use Code → Find Usages ({}).",
                            pide_core::keymap::label("alt-f7")
                        )
                        .to_string()
                    } else {
                        format!(
                            "{} in {} file{}",
                            self.items.len(),
                            files,
                            if files == 1 { "" } else { "s" }
                        )
                    }),
            )
            .child(
                div()
                    .id("locations")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pb_2()
                    .children(rows),
            )
    }
}
