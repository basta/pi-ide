//! Tool windows: the panels that live in a side or bottom dock (Project, pi,
//! Problems…) and get a button on the window's edge stripes, IDEA-style.
//!
//! Each panel crate registers its own [`ToolWindowSpec`] in its `init`; the
//! app draws the stripes and installs the [`ToolWindowHost`] that shows,
//! hides and moves them. New panels need no change in the app.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::IconName;
use gpui_kit::component::dock::BasePanelView as PanelView;
use gpui_kit::*;

use crate::{CommandRegistry, CommandSpec, DockPosition};

#[derive(Clone)]
pub struct ToolWindowSpec {
    /// The panel's `panel_name` (how it's found in the dock and in saved layouts).
    pub id: &'static str,
    pub title: &'static str,
    pub icon: IconName,
    /// Where it opens the first time: Left, Right or Bottom.
    pub dock: DockPosition,
    /// The toggle command (`view.toggle_project`) and its default key.
    pub command: &'static str,
    pub key: Option<&'static str>,
    /// Builds the panel the first time it's shown in a window.
    pub create: fn(&mut Window, &mut App) -> Arc<dyn PanelView>,
}

#[derive(Default)]
struct ToolWindows(Vec<ToolWindowSpec>);
impl Global for ToolWindows {}

/// What the app does with a tool window (it owns the dock area).
#[derive(Clone)]
pub struct ToolWindowHost {
    /// Show and focus it; hide it if it's already the one showing in its dock.
    pub toggle: Rc<dyn Fn(&'static str, &mut Window, &mut App)>,
}
impl Global for ToolWindowHost {}

/// Register a tool window and its toggle command.
pub fn register(cx: &mut App, spec: ToolWindowSpec) {
    let mut command = CommandSpec::new(spec.command, format!("View: {}", spec.title));
    if let Some(key) = spec.key {
        command = command.key(key);
    }
    let id = spec.id;
    CommandRegistry::register(cx, command, move |window, cx| toggle(id, window, cx));
    let all = &mut cx.default_global::<ToolWindows>().0;
    all.retain(|s| s.id != spec.id);
    all.push(spec);
}

/// Every tool window, in registration order.
pub fn all(cx: &App) -> Vec<ToolWindowSpec> {
    cx.try_global::<ToolWindows>()
        .map(|t| t.0.clone())
        .unwrap_or_default()
}

pub fn get(id: &str, cx: &App) -> Option<ToolWindowSpec> {
    cx.try_global::<ToolWindows>()?
        .0
        .iter()
        .find(|s| s.id == id)
        .cloned()
}

pub fn toggle(id: &'static str, window: &mut Window, cx: &mut App) {
    if let Some(t) = cx.try_global::<ToolWindowHost>().map(|h| h.toggle.clone()) {
        t(id, window, cx);
    }
}
