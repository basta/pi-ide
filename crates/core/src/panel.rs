//! The panel API, kept as small as possible.
//!
//! Every panel is also a GPUI Kit dock panel (which brings `panel_name`, the
//! tab title element and persistence via `dump`). Ours adds what the app needs
//! to place it. A scripted panel (M6) would be a second implementation.

use gpui_kit::*;

use crate::{Agent, Workspace};

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

/// Handles every panel gets at construction.
#[derive(Clone)]
pub struct Services {
    pub workspace: Entity<Workspace>,
    pub agent: Entity<Agent>,
}

impl Global for Services {}

impl Services {
    pub fn global(cx: &App) -> Self {
        cx.global::<Services>().clone()
    }
}
