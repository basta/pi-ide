//! Tool windows: the panels that live in a side or bottom dock (Project, pi,
//! Problems…) and get a button on the window's edge stripes, IDEA-style.
//!
//! Each panel crate registers its own [`ToolWindowSpec`] in its `init`; the
//! app draws the stripes and installs the [`ToolWindowHost`] that shows,
//! hides and moves them. New panels need no change in the app.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::IconName;
use gpui_kit::component::dock::{BasePanel, BasePanelView as PanelView, DockArea};
use gpui_kit::*;

use crate::settings::{self, ProjectSettings, Settings};
use crate::settings_page::Scope;
use crate::{CommandRegistry, CommandSpec, DockPosition, Services};

/// Builds a tool window's panel.
pub type PanelFactory = Rc<dyn Fn(&mut Window, &mut App) -> Arc<dyn PanelView>>;

/// Takes a tool window's panel out of the dock (it was turned off). The dock
/// only removes panels by their type, which only the panel's crate knows:
/// build it with [`remover`].
pub type Remover = Rc<dyn Fn(&Arc<dyn PanelView>, &Entity<DockArea>, &mut Window, &mut App)>;

/// The [`Remover`] for panels of type `P`.
pub fn remover<P: BasePanel>() -> Remover {
    Rc::new(|view, dock, window, cx| {
        if let Ok(entity) = view.view().downcast::<P>() {
            dock.update(cx, |d, cx| d.remove_panel(entity, window, cx));
        }
    })
}

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
    /// Builds the panel the first time it's shown in a window. A closure, so a
    /// tool window found at run time (a script panel) can carry what it builds.
    pub create: PanelFactory,
    pub remove: Remover,
}

#[derive(Default)]
struct ToolWindows(Vec<ToolWindowSpec>);
impl Global for ToolWindows {}

/// What the app does with a tool window (it owns the dock area).
#[derive(Clone)]
pub struct ToolWindowHost {
    /// Show and focus it; hide it if it's already the one showing in its dock.
    pub toggle: Rc<dyn Fn(&'static str, &mut Window, &mut App)>,
    /// Take it out of the window's dock, if it's there.
    pub close: Rc<dyn Fn(&str, &mut Window, &mut App)>,
    /// Show it (never hide); `focus`: also move the focus there.
    pub show: Rc<dyn Fn(&'static str, bool, &mut Window, &mut App)>,
}
impl Global for ToolWindowHost {}

/// Register a tool window and its toggle command.
pub fn register(cx: &mut App, spec: ToolWindowSpec) {
    let mut command = CommandSpec::new(spec.command, format!("View: {}", spec.title));
    if let Some(key) = spec.key {
        command = command.key(key);
        // Digit and F-key toggles keep working in a terminal; a letter one
        // (Commit's Cmd+K, Ctrl+K off macOS) is the shell's there.
        let last = key.rsplit('-').next().unwrap_or(key);
        if !(last.len() == 1 && last.chars().all(|c| c.is_ascii_alphabetic())) {
            command = command.over_terminal();
        }
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

/// Remove a tool window (a script panel that was deleted). Its toggle command stays
/// registered but does nothing.
pub fn unregister(cx: &mut App, id: &str) {
    if cx.has_global::<ToolWindows>() {
        cx.global_mut::<ToolWindows>().0.retain(|s| s.id != id);
    }
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

/// Show tool window `id` (creating it the first time) without hiding it when
/// it's already showing: a build shows its output, the focus stays in the
/// editor unless `focus`.
pub fn show(id: &'static str, focus: bool, window: &mut Window, cx: &mut App) {
    if let Some(s) = cx.try_global::<ToolWindowHost>().map(|h| h.show.clone()) {
        s(id, focus, window, cx);
    }
}

// ---------------------------------------------------------------------------
// On and off: `tool_windows.disabled` in settings.json (everyone), and
// `tool_windows.disabled`/`enabled` in a project's `.pide/settings.json`.

/// Each open project's own settings, by root.
#[derive(Default)]
struct Projects(HashMap<PathBuf, ProjectSettings>);
impl Global for Projects {}

/// Read a project's `.pide/settings.json` (a few bytes, when its window opens).
pub fn load_project(root: &Path, cx: &mut App) {
    let s = settings::load_project(root);
    cx.default_global::<Projects>().0.insert(root.to_path_buf(), s);
}

fn everyone_off(id: &str, cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_some_and(|s| s.tool_windows_off.iter().any(|x| x == id))
}

/// What a project says about `id`: Some(on) when it overrides everyone.
pub fn project_override(id: &str, root: &Path, cx: &App) -> Option<bool> {
    let p = cx.try_global::<Projects>()?.0.get(root)?;
    if p.tool_windows_off.iter().any(|x| x == id) {
        Some(false)
    } else if p.tool_windows_on.iter().any(|x| x == id) {
        Some(true)
    } else {
        None
    }
}

/// Is `id` on in the project at `root` (or, with no root, for everyone)?
pub fn enabled_in(id: &str, root: Option<&Path>, cx: &App) -> bool {
    root.and_then(|r| project_override(id, r, cx))
        .unwrap_or_else(|| !everyone_off(id, cx))
}

/// Is `id` on in `window`'s project?
pub fn is_enabled(id: &str, window: &Window, cx: &App) -> bool {
    let root = Services::for_window(window.window_handle().window_id(), cx)
        .map(|s| s.workspace.read(cx).root().to_path_buf());
    enabled_in(id, root.as_deref(), cx)
}

/// The tool windows that are on in `window`, in registration order.
pub fn enabled(window: &Window, cx: &App) -> Vec<ToolWindowSpec> {
    all(cx)
        .into_iter()
        .filter(|s| is_enabled(s.id, window, cx))
        .collect()
}

/// Turn `id` on or off for everyone, or for `window`'s project (`None` there:
/// back to what everyone has). Saves, and closes it wherever it's now off.
pub fn set_enabled(id: &str, scope: Scope, on: Option<bool>, window: &mut Window, cx: &mut App) {
    let id = id.to_string();
    match scope {
        Scope::Everyone => {
            let on = on.unwrap_or(true);
            let mut list = cx
                .try_global::<Settings>()
                .map(|s| s.tool_windows_off.clone())
                .unwrap_or_default();
            list.retain(|x| x != &id);
            if !on {
                list.push(id.clone());
            }
            list.sort();
            cx.default_global::<Settings>().tool_windows_off = list.clone();
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = settings::save_key(settings::TOOL_WINDOWS_OFF, list.into())
                    {
                        log::error!("save settings: {e}");
                    }
                })
                .detach();
        }
        Scope::Project => {
            let Some(services) = Services::for_window(window.window_handle().window_id(), cx)
            else {
                return;
            };
            let root = services.workspace.read(cx).root().to_path_buf();
            let p = cx
                .default_global::<Projects>()
                .0
                .entry(root.clone())
                .or_default();
            p.tool_windows_off.retain(|x| x != &id);
            p.tool_windows_on.retain(|x| x != &id);
            match on {
                Some(true) => p.tool_windows_on.push(id.clone()),
                Some(false) => p.tool_windows_off.push(id.clone()),
                None => {}
            }
            p.tool_windows_off.sort();
            p.tool_windows_on.sort();
            let (off, on) = (p.tool_windows_off.clone(), p.tool_windows_on.clone());
            cx.background_executor()
                .spawn(async move {
                    let r = settings::save_project_key(&root, settings::TOOL_WINDOWS_OFF, off.into())
                        .and_then(|_| {
                            settings::save_project_key(&root, settings::TOOL_WINDOWS_ON, on.into())
                        });
                    if let Err(e) = r {
                        log::error!("save project settings: {e}");
                    }
                })
                .detach();
        }
    }
    close_disabled(window, cx);
    // Other windows aren't borrowed once this update is over.
    let this = window.window_handle().window_id();
    cx.defer(move |cx| {
        for w in cx.windows() {
            if w.window_id() != this {
                let _ = w.update(cx, |_, window, cx| close_disabled(window, cx));
            }
        }
    });
}

/// Take every tool window that's off out of `window`'s dock.
pub fn close_disabled(window: &mut Window, cx: &mut App) {
    let Some(close) = cx.try_global::<ToolWindowHost>().map(|h| h.close.clone()) else {
        return;
    };
    for spec in all(cx) {
        if !is_enabled(spec.id, window, cx) {
            close(spec.id, window, cx);
        }
    }
    window.refresh();
}

/// Take one tool window out of `window`'s dock (a script panel whose grant was revoked).
pub fn close(id: &str, window: &mut Window, cx: &mut App) {
    if let Some(close) = cx.try_global::<ToolWindowHost>().map(|h| h.close.clone()) {
        close(id, window, cx);
    }
}
