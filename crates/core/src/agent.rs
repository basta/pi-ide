//! The agent seam. Panels hold an `Entity<Agent>` and never see the backend;
//! `agent-pi` implements [`AgentBackend`] over `pi --mode rpc`.

use gpui_kit::*;

use crate::events::AgentEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Not started yet (started lazily on the first prompt) or idle between runs.
    Idle,
    Starting,
    Busy,
    Aborting,
    /// The process exited unexpectedly. `message` is shown with a restart action.
    Crashed { message: String, stderr_tail: Vec<String> },
}

/// What a backend must do. Methods are called on the UI thread, inside the
/// `Agent`'s own update (so use the `agent` argument; never `cx.entity().update`).
/// They must not block: do I/O on the runtime thread and report back through
/// [`Agent::handle_event`] from a `cx.spawn` task.
pub trait AgentBackend: 'static {
    fn prompt(&mut self, text: String, agent: &mut Agent, cx: &mut Context<Agent>);
    /// Must take effect within 1 s.
    fn abort(&mut self, agent: &mut Agent, cx: &mut Context<Agent>);
    fn new_session(&mut self, agent: &mut Agent, cx: &mut Context<Agent>);
    /// Stop (if running) and forget the crash; the next prompt resumes the session.
    fn restart(&mut self, agent: &mut Agent, cx: &mut Context<Agent>);
}

pub struct Agent {
    backend: Option<Box<dyn AgentBackend>>,
    status: AgentStatus,
    /// Extension status items (`setStatus`), shown in the status bar.
    pub status_items: Vec<(String, String)>,
}

impl EventEmitter<AgentEvent> for Agent {}

impl Agent {
    pub fn new(backend: Box<dyn AgentBackend>) -> Self {
        Self { backend: Some(backend), status: AgentStatus::Idle, status_items: Vec::new() }
    }

    pub fn status(&self) -> &AgentStatus {
        &self.status
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.status, AgentStatus::Busy | AgentStatus::Starting | AgentStatus::Aborting)
    }

    pub fn set_status(&mut self, status: AgentStatus, cx: &mut Context<Self>) {
        if self.status != status {
            self.status = status;
            cx.emit(AgentEvent::StatusChanged);
            cx.notify();
        }
    }

    fn with_backend(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut dyn AgentBackend, &mut Self, &mut Context<Self>)) {
        if let Some(mut b) = self.backend.take() {
            f(b.as_mut(), self, cx);
            self.backend = Some(b);
        }
    }

    pub fn prompt(&mut self, text: String, cx: &mut Context<Self>) {
        self.with_backend(cx, |b, a, cx| b.prompt(text, a, cx));
    }
    pub fn abort(&mut self, cx: &mut Context<Self>) {
        self.with_backend(cx, |b, a, cx| b.abort(a, cx));
    }
    pub fn new_session(&mut self, cx: &mut Context<Self>) {
        self.with_backend(cx, |b, a, cx| b.new_session(a, cx));
    }
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.status_items.clear();
        self.with_backend(cx, |b, a, cx| b.restart(a, cx));
    }

    /// Backends call this (from a `cx.spawn` task) for everything they receive.
    pub fn handle_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match &event {
            AgentEvent::Settled { .. } => self.set_status(AgentStatus::Idle, cx),
            AgentEvent::AssistantStart | AgentEvent::ToolStart { .. } => {
                if !matches!(self.status, AgentStatus::Aborting) {
                    self.set_status(AgentStatus::Busy, cx)
                }
            }
            AgentEvent::Status { key, text } => {
                self.status_items.retain(|(k, _)| k != key);
                if let Some(t) = text {
                    self.status_items.push((key.clone(), t.clone()));
                }
                cx.notify();
            }
            _ => {}
        }
        cx.emit(event);
    }
}
