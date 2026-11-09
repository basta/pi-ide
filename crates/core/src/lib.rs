//! pide's small core. Depends on nothing else in this repo; everything else
//! depends on it. Panels talk to each other only through what lives here:
//! typed events and registered commands.

pub mod agent;
pub mod buffer;
pub mod changes;
pub mod commands;
pub mod context;
pub mod events;
pub mod fswatch;
pub mod git;
pub mod keymap;
pub mod opener;
pub mod panel;
pub mod paths;
pub mod recent;
pub mod sessions;
pub mod settings;
pub mod workspace;

pub use agent::{Agent, AgentBackend, AgentStatus, ModelInfo, SessionStats, SlashCommand};
pub use buffer::{Buffer, LineEnding};
pub use commands::{CommandRegistry, CommandSpec, RunCommand};
pub use events::{
    AgentEvent, DiagnosticsEvent, DialogAnswer, DialogKind, DialogRequest, WorkspaceEvent,
};
pub use panel::{DockPosition, Panel, Services};
pub use sessions::{Sessions, SessionsEvent};
pub use workspace::Workspace;
