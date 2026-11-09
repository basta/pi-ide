//! Typed events. No string-keyed bus: subscribe with `cx.subscribe` to the
//! entity that emits them.

use std::path::PathBuf;

use gpui_kit::Entity;
use serde_json::Value;

use crate::buffer::Buffer;

/// Emitted by [`crate::Workspace`].
#[derive(Clone)]
pub enum WorkspaceEvent {
    BufferOpened(Entity<Buffer>),
    BufferChanged(Entity<Buffer>),
    BufferSaved(Entity<Buffer>),
    /// The file changed on disk. `reloaded` = the clean buffer took the new
    /// text; otherwise the buffer is dirty and now has a conflict.
    FileChangedOnDisk { buffer: Entity<Buffer>, reloaded: bool },
    FileDeleted(Entity<Buffer>),
    BufferClosed(PathBuf),
    /// The focused editor changed (or none is focused).
    ActiveBufferChanged,
    /// Files were created, removed or renamed under the project root.
    TreeChanged,
}

/// Emitted by the LSP layer (M3).
#[derive(Clone, Debug)]
pub enum DiagnosticsEvent {
    Updated { path: PathBuf },
}

/// Emitted by [`crate::Agent`]. Backend-neutral: the pi RPC backend maps its
/// wire records onto these, an ACP backend could do the same.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// `Agent::status` changed.
    StatusChanged,
    /// A prompt the user sent (echoed once the backend accepted it).
    UserPrompt { text: String },
    /// An assistant message started streaming.
    AssistantStart,
    TextDelta { index: usize, delta: String },
    TextEnd { index: usize, content: String },
    ThinkingDelta { index: usize, delta: String },
    ThinkingEnd { index: usize, content: String },
    /// The assistant message finished; `error` set if it ended in an error/abort.
    AssistantEnd { error: Option<String> },
    ToolStart { id: String, name: String, args: Value },
    ToolUpdate { id: String, partial: Value },
    ToolEnd { id: String, name: String, result: Value, is_error: bool, duration_ms: Option<u64> },
    /// pi's pending messages changed: steering (delivered after the current tool
    /// calls) and follow-ups (delivered when the run ends). Complete lists.
    Queue { steering: Vec<String>, follow_up: Vec<String> },
    /// The agent will not continue on its own.
    Settled { aborted: bool },
    /// Fire-and-forget extension UI: status bar item.
    Status { key: String, text: Option<String> },
    /// Fire-and-forget extension UI: toast.
    Notify { message: String, error: bool },
    /// Something the backend wants the user to see (rejected prompt, extension error…).
    Error(String),
    /// The agent process exited.
    Exited { code: Option<i32>, stderr_tail: Vec<String> },
    /// A record we don't understand yet. Shown raw; never a crash.
    Unknown(Value),
}
