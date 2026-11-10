//! Tool windows, IDEA-style: a stripe of buttons on each side of the window.
//! The left stripe holds the left dock's tools on top and the bottom dock's
//! below; the right stripe holds the right dock's. Clicking a button shows
//! its panel (creating it the first time) or hides it if it's already showing.
//!
//! The panels register themselves (`pide_core::toolwindow::register`); this
//! module only works the dock area.

use std::rc::Rc;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{DockArea, DockPlacement, PanelId};
use gpui_kit::component::{Selectable as _, Sizable as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::toolwindow::{self, ToolWindowHost, ToolWindowSpec};
use pide_core::DockPosition;

pub fn init(cx: &mut App) {
    cx.set_global(ToolWindowHost {
        toggle: Rc::new(|id, window, cx| {
            if let Some(dock) = crate::shell::dock_of(window, cx) {
                toggle(&dock, id, window, cx);
            }
        }),
    });
}

fn placement(p: DockPosition) -> DockPlacement {
    match p {
        DockPosition::Left => DockPlacement::Left,
        DockPosition::Right => DockPlacement::Right,
        DockPosition::Bottom => DockPlacement::Bottom,
        DockPosition::Center => DockPlacement::Center,
    }
}

const PLACEMENTS: [DockPlacement; 4] = [
    DockPlacement::Left,
    DockPlacement::Right,
    DockPlacement::Bottom,
    DockPlacement::Center,
];

/// Where the panel named `id` is in the dock area, if it's there.
fn find(dock: &DockArea, id: &str, cx: &App) -> Option<(PanelId, DockPlacement)> {
    PLACEMENTS.into_iter().find_map(|p| {
        let tree = dock.layout(p)?;
        tree.panels()
            .find(|pid| dock.panel(*pid).is_some_and(|v| v.panel_name(cx) == id))
            .map(|pid| (pid, p))
    })
}

/// Is `panel` the selected tab of its group?
fn is_selected(dock: &DockArea, panel: PanelId, p: DockPlacement) -> bool {
    use gpui_kit::component::dock::PaneRef;
    let Some(tree) = dock.layout(p) else {
        return false;
    };
    let Some(node) = tree.find_panel_node(panel).and_then(|n| tree.find_node(n)) else {
        return false;
    };
    match node.kind() {
        PaneRef::Tabs { panels, active_ix } => panels.get(active_ix) == Some(&panel),
        PaneRef::Split { .. } => false,
    }
}

/// Showing: its dock is open and it's the selected tab there.
fn is_showing(dock: &DockArea, panel: PanelId, p: DockPlacement) -> bool {
    (p == DockPlacement::Center || dock.is_dock_open(p)) && is_selected(dock, panel, p)
}

fn default_size(p: DockPlacement) -> Pixels {
    match p {
        DockPlacement::Left => px(260.),
        DockPlacement::Right => px(420.),
        _ => px(240.),
    }
}

/// Show `id` (creating it the first time) and focus it, or hide its dock if
/// it's already showing.
pub fn toggle(dock: &Entity<DockArea>, id: &str, window: &mut Window, cx: &mut App) {
    let Some(spec) = toolwindow::get(id, cx) else {
        log::error!("unknown tool window {id}");
        return;
    };
    let found = find(dock.read(cx), id, cx);
    if let Some((pid, p)) = found
        && p != DockPlacement::Center
        && is_showing(dock.read(cx), pid, p)
    {
        dock.update(cx, |d, cx| d.toggle_dock(p, window, cx));
        return;
    }
    let (pid, p) = match found {
        Some(f) => f,
        None => {
            let view = (spec.create)(window, cx);
            let pid = view.panel_id(cx);
            let p = placement(spec.dock);
            dock.update(cx, |d, cx| {
                let size = (!d.has_dock(p)).then(|| default_size(p));
                d.add_panel_view(view, p, size, window, cx);
            });
            (pid, p)
        }
    };
    dock.update(cx, |d, cx| {
        if p != DockPlacement::Center && !d.is_dock_open(p) {
            d.toggle_dock(p, window, cx);
        }
        d.select_panel(pid, window, cx);
    });
    if let Some(view) = dock.read(cx).panel(pid).cloned() {
        let focus = view.focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }
}

/// "cmd-shift-1" → "⇧⌘1" for tooltips (macOS modifier order).
fn key_label(key: &str) -> String {
    let parts: Vec<&str> = key.split('-').collect();
    let mut out = String::new();
    for (m, sym) in [("ctrl", '⌃'), ("alt", '⌥'), ("shift", '⇧'), ("cmd", '⌘')] {
        if parts.contains(&m) {
            out.push(sym);
        }
    }
    out + &parts.last().copied().unwrap_or("").to_uppercase()
}

#[derive(Clone, Copy, PartialEq)]
pub enum Side {
    Left,
    Right,
}

/// One edge's stripe of tool buttons.
pub fn stripe(side: Side, dock: &Entity<DockArea>, cx: &App) -> impl IntoElement {
    let d = dock.read(cx);
    // Each tool where it is now (or will open), and whether it's showing.
    let tools: Vec<(ToolWindowSpec, DockPlacement, bool)> = toolwindow::all(cx)
        .into_iter()
        .map(|spec| {
            let (p, showing) = match find(d, spec.id, cx) {
                Some((pid, p)) => (p, is_showing(d, pid, p)),
                None => (placement(spec.dock), false),
            };
            // A tool dragged into the editor area keeps its home stripe.
            let p = if p == DockPlacement::Center {
                placement(spec.dock)
            } else {
                p
            };
            (spec, p, showing)
        })
        .collect();
    let (active_bg, radius) = (cx.theme().foreground.opacity(0.14), cx.theme().radius);
    let button = |(spec, _, showing): &(ToolWindowSpec, DockPlacement, bool)| {
        let id = spec.id;
        let tip = match spec.key {
            Some(k) => format!("{}  {}", spec.title, key_label(k)),
            None => spec.title.to_string(),
        };
        div()
            .rounded(radius)
            .when(*showing, |this| this.bg(active_bg))
            .child(
                Button::new(SharedString::from(format!("tool-{id}")))
                    .ghost()
                    .small()
                    .icon(spec.icon.clone())
                    .selected(*showing)
                    .tooltip(tip)
                    .on_click(move |_, window, cx| toolwindow::toggle(id, window, cx)),
            )
    };
    let (top, bottom): (Vec<_>, Vec<_>) = match side {
        Side::Left => (
            tools.iter().filter(|t| t.1 == DockPlacement::Left).collect(),
            tools.iter().filter(|t| t.1 == DockPlacement::Bottom).collect(),
        ),
        Side::Right => (
            tools.iter().filter(|t| t.1 == DockPlacement::Right).collect(),
            Vec::new(),
        ),
    };
    let theme = cx.theme();
    v_flex()
        .h_full()
        .flex_shrink_0()
        .w(px(32.))
        .py_1()
        .items_center()
        .justify_between()
        .bg(theme.title_bar)
        .border_color(theme.border)
        .map(|this| match side {
            Side::Left => this.border_r_1(),
            Side::Right => this.border_l_1(),
        })
        .child(v_flex().gap_1().children(top.into_iter().map(button)))
        .child(v_flex().gap_1().children(bottom.into_iter().map(button)))
}

#[cfg(test)]
mod tests {
    #[::core::prelude::v1::test]
    fn key_labels() {
        assert_eq!(super::key_label("cmd-1"), "⌘1");
        assert_eq!(super::key_label("cmd-shift-f12"), "⇧⌘F12");
    }
}
