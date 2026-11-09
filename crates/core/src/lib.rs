//! pide's small core. Depends on nothing else in this repo; everything else
//! depends on it. Panels talk to each other only through what lives here:
//! typed events and registered commands.

pub mod agent;
pub mod buffer;
pub mod commands;
pub mod events;
pub mod fswatch;
pub mod keymap;
pub mod panel;
pub mod paths;
pub mod workspace;

pub use agent::{Agent, AgentBackend, AgentStatus};
pub use buffer::{Buffer, LineEnding};
pub use commands::{CommandRegistry, CommandSpec, RunCommand};
pub use events::{AgentEvent, DiagnosticsEvent, WorkspaceEvent};
pub use panel::{DockPosition, Panel, Services};
pub use workspace::Workspace;
