//! Tool windows, IDEA-style: a stripe of buttons on each side of the window.
//! The left stripe holds the left dock's tools on top and the bottom dock's
//! below; the right stripe holds the right dock's. Clicking a button shows
//! its panel (creating it the first time) or hides it if it's already showing.
//!
//! The panels register themselves (`pide_core::toolwindow::register`); this
//! module only works the dock area.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{DockArea, DockPlacement, PanelId};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::{Selectable as _, Sizable as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::toolwindow::{self, ToolWindowHost, ToolWindowSpec};
use pide_core::{CommandRegistry, CommandSpec, DockPosition};

pub fn init(cx: &mut App) {
    cx.set_global(ToolWindowHost {
        toggle: Rc::new(|id, window, cx| {
            if let Some(dock) = crate::shell::dock_of(window, cx) {
                toggle(&dock, id, window, cx);
            }
        }),
    });
    CommandRegistry::register(
        cx,
        CommandSpec::new("view.hide_tool", "View: Hide Active Tool Window").key("shift-escape"),
        |window, cx| {
            if let Some(dock) = crate::shell::dock_of(window, cx) {
                hide_focused(&dock, window, cx);
            }
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("view.hide_all_tools", "View: Hide All Tool Windows").key("cmd-shift-f12"),
        |window, cx| {
            if let Some(dock) = crate::shell::dock_of(window, cx) {
                hide_all(&dock, window, cx);
            }
        },
    );
}

const SIDES: [DockPlacement; 3] = [
    DockPlacement::Left,
    DockPlacement::Right,
    DockPlacement::Bottom,
];

/// Docks hidden by Hide All Tool Windows, per window, to bring back next time.
#[derive(Default)]
struct HiddenByAll(HashMap<WindowId, Vec<DockPlacement>>);
impl Global for HiddenByAll {}

pub fn forget(window: WindowId, cx: &mut App) {
    cx.default_global::<HiddenByAll>().0.remove(&window);
}

/// The selected panel of the first tab group in `p`.
fn selected_panel(dock: &DockArea, p: DockPlacement) -> Option<PanelId> {
    use gpui_kit::component::dock::PaneRef;
    let tree = dock.layout(p)?;
    tree.node_ids()
        .into_iter()
        .find_map(|n| match tree.find_node(n)?.kind() {
            PaneRef::Tabs { panels, active_ix } => panels.get(active_ix).copied(),
            PaneRef::Split { .. } => None,
        })
}

/// Focus the editor area's selected tab (after hiding a tool window).
fn focus_center(dock: &Entity<DockArea>, window: &mut Window, cx: &mut App) {
    let d = dock.read(cx);
    if let Some(view) = selected_panel(d, DockPlacement::Center).and_then(|p| d.panel(p)) {
        let focus = view.focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }
}

/// Shift+Esc: hide the dock holding the focus and go back to the editor.
fn hide_focused(dock: &Entity<DockArea>, window: &mut Window, cx: &mut App) {
    let d = dock.read(cx);
    let focused = SIDES.into_iter().find(|&p| {
        d.is_dock_open(p)
            && d.layout(p).is_some_and(|t| {
                t.panels().any(|pid| {
                    d.panel(pid)
                        .is_some_and(|v| v.focus_handle(cx).contains_focused(window, cx))
                })
            })
    });
    if let Some(p) = focused {
        dock.update(cx, |d, cx| d.toggle_dock(p, window, cx));
        focus_center(dock, window, cx);
    }
}

/// Cmd+Shift+F12: hide every open dock; again brings them back.
fn hide_all(dock: &Entity<DockArea>, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let open: Vec<DockPlacement> = SIDES
        .into_iter()
        .filter(|&p| dock.read(cx).is_dock_open(p))
        .collect();
    let hiding = !open.is_empty();
    let to_toggle = if !hiding {
        cx.default_global::<HiddenByAll>()
            .0
            .remove(&id)
            .unwrap_or_default()
    } else {
        cx.default_global::<HiddenByAll>()
            .0
            .insert(id, open.clone());
        open
    };
    dock.update(cx, |d, cx| {
        for p in to_toggle {
            if d.has_dock(p) {
                d.toggle_dock(p, window, cx);
            }
        }
    });
    if hiding {
        focus_center(dock, window, cx);
    }
}

/// Move a tool window to another dock (creating it there if it isn't open yet).
pub fn move_to(id: &str, to: DockPlacement, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::dock::{DockLayout, InsertTarget, PaneRef};
    let Some(dock) = crate::shell::dock_of(window, cx) else {
        return;
    };
    let Some(spec) = toolwindow::get(id, cx) else {
        return;
    };
    let pid = match find(dock.read(cx), id, cx) {
        Some((_, from)) if from == to => return,
        Some((pid, _)) => {
            dock.update(cx, |d, cx| {
                if !d.has_dock(to) {
                    d.set_dock(to, DockLayout::tabs(), window, cx);
                    d.set_dock_size(to, default_size(to), window, cx);
                    d.set_dock_collapsible(to, true, window, cx);
                }
                let Some(tree) = d.layout(to) else { return };
                let tabs = tree.node_ids().into_iter().find(|n| {
                    tree.find_node(*n)
                        .is_some_and(|n| matches!(n.kind(), PaneRef::Tabs { .. }))
                });
                let target = match tabs {
                    Some(node) => InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: true,
                    },
                    None => InsertTarget::Split {
                        node: tree.root().id(),
                        placement: gpui_kit::component::Placement::Right,
                        size: None,
                    },
                };
                d.move_panel(pid, target, window, cx);
            });
            pid
        }
        None => {
            let view = (spec.create)(window, cx);
            let pid = view.panel_id(cx);
            dock.update(cx, |d, cx| {
                let size = (!d.has_dock(to)).then(|| default_size(to));
                d.add_panel_view(view, to, size, window, cx);
            });
            pid
        }
    };
    dock.update(cx, |d, cx| {
        if !d.is_dock_open(to) {
            d.toggle_dock(to, window, cx);
        }
        d.select_panel(pid, window, cx);
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
        // Hiding the dock that holds the focus would leave nothing focused (and
        // keys with nowhere to go): hand the focus back to the editor area.
        let had_focus = dock.read(cx).layout(p).is_some_and(|t| {
            t.panels().any(|pid| {
                dock.read(cx)
                    .panel(pid)
                    .is_some_and(|v| v.focus_handle(cx).contains_focused(window, cx))
            })
        });
        dock.update(cx, |d, cx| d.toggle_dock(p, window, cx));
        if had_focus {
            focus_center(dock, window, cx);
        }
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
        let tip = match pide_core::keymap::keys_for(cx, spec.command) {
            Some(k) => format!("{}  {}", spec.title, k),
            None => spec.title.to_string(),
        };
        div()
            .id(SharedString::from(format!("tool-wrap-{id}")))
            .rounded(radius)
            .when(*showing, |this| this.bg(active_bg))
            .context_menu(move |menu, _, _| {
                [
                    ("Move to Left", DockPlacement::Left),
                    ("Move to Right", DockPlacement::Right),
                    ("Move to Bottom", DockPlacement::Bottom),
                ]
                .into_iter()
                .fold(menu, |menu, (label, to)| {
                    menu.item(
                        PopupMenuItem::new(label)
                            .on_click(move |_, window, cx| move_to(id, to, window, cx)),
                    )
                })
            })
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
            tools
                .iter()
                .filter(|t| t.1 == DockPlacement::Left)
                .collect(),
            tools
                .iter()
                .filter(|t| t.1 == DockPlacement::Bottom)
                .collect(),
        ),
        Side::Right => (
            tools
                .iter()
                .filter(|t| t.1 == DockPlacement::Right)
                .collect(),
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
