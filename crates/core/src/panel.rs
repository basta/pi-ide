//! The panel API, kept as small as possible.
//!
//! Every panel is also a GPUI Kit dock panel (which brings `panel_name`, the
//! tab title element and persistence via `dump`). Ours adds what the app needs
//! to place it. A scripted panel (M6) would be a second implementation.

use gpui_kit::*;

use crate::{Agent, Sessions, Workspace};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockPosition {
    Left,
    Right,
    Bottom,
    Center,
}

pub trait Panel: gpui_kit::component::dock::Panel {
    /// Stable id; also the GPUI Kit `panel_name` used in saved layouts.
    fn id(&self) -> &'static str {
        self.panel_name()
    }
    /// Plain-text title (tab label, palette).
    fn title_text(&self, cx: &App) -> SharedString;
    fn default_dock(&self) -> DockPosition;
}

/// One project's state: its workspace and its pi sessions. Each project has
/// its own window (spec M2.5); panels find theirs through the window they're in.
#[derive(Clone)]
pub struct Services {
    pub workspace: Entity<Workspace>,
    pub sessions: Entity<Sessions>,
}

/// Every open project window and its `Services`.
#[derive(Default)]
struct ProjectWindows(std::collections::HashMap<WindowId, Services>);
impl Global for ProjectWindows {}

impl Services {
    /// The project of `window`. Every pide window is a project window, so this
    /// only fails for a window the app didn't open (a bug).
    pub fn of(window: &Window, cx: &App) -> Self {
        Self::for_window(window.window_handle().window_id(), cx).expect("window has no project")
    }

    pub fn for_window(id: WindowId, cx: &App) -> Option<Self> {
        cx.try_global::<ProjectWindows>()
            .and_then(|w| w.0.get(&id).cloned())
    }

    /// The session the pi panel in `window` is showing.
    pub fn active_agent(window: &Window, cx: &App) -> Entity<Agent> {
        Self::of(window, cx).sessions.read(cx).active()
    }

    /// Called by the app when it opens a project window, before building its panels.
    pub fn register(id: WindowId, services: Services, cx: &mut App) {
        cx.default_global::<ProjectWindows>().0.insert(id, services);
    }

    /// Called when the window closes. Dropping the last handle stops its pi processes.
    pub fn unregister(id: WindowId, cx: &mut App) -> Option<Services> {
        cx.default_global::<ProjectWindows>().0.remove(&id)
    }

    /// Open projects: (window, project root).
    pub fn open_projects(cx: &App) -> Vec<(WindowId, std::path::PathBuf)> {
        cx.try_global::<ProjectWindows>()
            .map(|w| {
                w.0.iter()
                    .map(|(id, s)| (*id, s.workspace.read(cx).root().to_path_buf()))
                    .collect()
            })
            .unwrap_or_default()
    }
}
