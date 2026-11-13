//! The agent seam. Panels hold an `Entity<Agent>` and never see the backend;
//! `agent-pi` implements [`AgentBackend`] over `pi --mode rpc`.

use gpui_kit::*;

use crate::changes::{FileChange, Touch};
use crate::collab::{Pin, ProgressStep, Stop, StopRef};
use crate::events::{AgentEvent, DialogAnswer, DialogRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Not started yet (started lazily on the first prompt) or idle between runs.
    Idle,
    Starting,
    Busy,
    Aborting,
    /// The process exited unexpectedly. `message` is shown with a restart action.
    Crashed {
        message: String,
        stderr_tail: Vec<String>,
    },
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
    /// While busy: `follow_up` false = steer (after the current tool calls),
    /// true = follow-up (after the run ends).
    fn queue(&mut self, text: String, follow_up: bool, agent: &mut Agent, cx: &mut Context<Agent>);
    /// Answer a pending extension dialog.
    fn answer_dialog(
        &mut self,
        id: String,
        answer: DialogAnswer,
        agent: &mut Agent,
        cx: &mut Context<Agent>,
    );
    /// Stop (if running) and forget the crash; the next prompt resumes the session.
    fn restart(&mut self, agent: &mut Agent, cx: &mut Context<Agent>);
    /// Ask for the models this session can use; they arrive in `agent.models`.
    fn request_models(&mut self, _agent: &mut Agent, _cx: &mut Context<Agent>) {}
    /// Ask for the thinking levels the current model supports (`agent.thinking_levels`).
    fn request_thinking_levels(&mut self, _agent: &mut Agent, _cx: &mut Context<Agent>) {}
    /// Set this session's thinking level; `agent.thinking` updates when confirmed.
    fn set_thinking(&mut self, _level: String, _agent: &mut Agent, _cx: &mut Context<Agent>) {}
    /// The session got a new name (already in `agent.name`); save it with the session.
    fn renamed(&mut self, _agent: &mut Agent, _cx: &mut Context<Agent>) {}
    /// Write the session as an HTML page and open it in the browser.
    fn export_html(&mut self, _agent: &mut Agent, _cx: &mut Context<Agent>) {}
    /// Summarize the conversation so far to free context (pi's `compact`).
    fn compact(&mut self, _agent: &mut Agent, _cx: &mut Context<Agent>) {}
    /// Ask for the session's slash commands; they arrive in `agent.commands`.
    fn request_commands(&mut self, _agent: &mut Agent, _cx: &mut Context<Agent>) {}
    /// Ask the model one question on the side: no session, no tools, nothing added
    /// to this session's conversation (e.g. a "catch me up" briefing).
    fn ask_aside(
        &mut self,
        _prompt: String,
        _agent: &mut Agent,
        _cx: &mut Context<Agent>,
    ) -> Task<anyhow::Result<String>> {
        Task::ready(Err(anyhow::anyhow!("this agent can't answer on the side")))
    }
    /// Switch this session's model; `agent.model` updates when the backend confirms.
    fn set_model(
        &mut self,
        _provider: String,
        _id: String,
        _agent: &mut Agent,
        _cx: &mut Context<Agent>,
    ) {
    }
}

/// A slash command the agent offers (`/name`): extension commands, prompt
/// templates, skills (`skill:name`).
#[derive(Clone, Debug, PartialEq)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    /// `extension`, `prompt` or `skill`.
    pub source: String,
}

/// Token use and cost of a session, and how full the context window is.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SessionStats {
    pub cost: f64,
    pub tokens_total: u64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_cache_read: u64,
    /// Context window use (percent, tokens, window); `None` right after compaction.
    pub context_percent: Option<f64>,
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
}

/// An image sent with a prompt (a pasted screenshot).
#[derive(Clone, Debug)]
pub struct ImageAttachment {
    pub mime: String,
    pub data: std::sync::Arc<Vec<u8>>,
}

/// A model as the agent reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub reasoning: bool,
}

pub struct Agent {
    backend: Option<Box<dyn AgentBackend>>,
    status: AgentStatus,
    /// Extension status items (`setStatus`), shown in the status bar.
    pub status_items: Vec<(String, String)>,
    /// Display name: pi's session name, or the start of the first prompt.
    pub name: SharedString,
    /// Finished while you were looking at another session.
    pub unread: bool,
    /// A blocking question (extension dialog) is waiting for an answer.
    pub waiting: bool,
    /// Restart pi when the current run settles, to pick up changed pi settings.
    reload_pending: bool,
    /// The session's model and thinking level, once the backend reports them.
    pub model: Option<ModelInfo>,
    pub thinking: Option<String>,
    /// Models to pick from (filled by `request_models`).
    pub models: Vec<ModelInfo>,
    /// Thinking levels the model supports (filled by `request_thinking_levels`).
    pub thinking_levels: Vec<String>,
    /// Cost and context use, refreshed after each run (backend permitting).
    pub stats: Option<SessionStats>,
    /// Images for the prompt being sent; the backend takes them (others ignore them).
    pub pending_images: Vec<ImageAttachment>,
    /// Slash commands (filled by `request_commands`).
    pub commands: Vec<SlashCommand>,
    /// The backend has answered `request_commands` (the list may be empty).
    pub commands_loaded: bool,
    commands_requested: bool,
    /// pi's session file, once known; used to resume after the process stops.
    pub session_file: Option<std::path::PathBuf>,
    /// Messages waiting to be delivered: (steering, follow-ups).
    pub queue: (Vec<String>, Vec<String>),
    /// Files pi changed in this session, in first-touched order.
    pub changes: Vec<FileChange>,
    /// What pi pinned for the user (`ide_pin`), in order.
    pub pins: Vec<Pin>,
    /// pi's checklist for this session (`ide_progress`), latest version.
    pub progress: Vec<ProgressStep>,
    /// The last "catch me up" briefing (collaborator mode).
    pub catch_up: Option<crate::collab::CatchUp>,
    /// The walkthrough's current stop (see [`Agent::stops`]).
    pub current_stop: Option<StopRef>,
    /// Pending write of `changes` to disk (debounced; dropping cancels).
    persist: Option<Task<()>>,
    /// Tool call id -> path, for file-changing calls still running.
    pending_touches: Vec<(String, std::path::PathBuf)>,
    /// Extension dialogs waiting for an answer, oldest first. `waiting` mirrors `!is_empty()`.
    pub dialogs: Vec<DialogRequest>,
    _dialog_timers: Vec<Task<()>>,
}

pub const UNNAMED: &str = "New session";

impl EventEmitter<AgentEvent> for Agent {}

impl Agent {
    pub fn new(backend: Box<dyn AgentBackend>) -> Self {
        Self {
            backend: Some(backend),
            status: AgentStatus::Idle,
            status_items: Vec::new(),
            name: UNNAMED.into(),
            unread: false,
            waiting: false,
            reload_pending: false,
            model: None,
            thinking: None,
            models: Vec::new(),
            thinking_levels: Vec::new(),
            commands: Vec::new(),
            stats: None,
            pending_images: Vec::new(),
            commands_loaded: false,
            commands_requested: false,
            session_file: None,
            queue: (Vec::new(), Vec::new()),
            dialogs: Vec::new(),
            _dialog_timers: Vec::new(),
            changes: Vec::new(),
            pins: Vec::new(),
            progress: Vec::new(),
            current_stop: None,
            catch_up: None,
            persist: None,
            pending_touches: Vec::new(),
        }
    }

    pub fn status(&self) -> &AgentStatus {
        &self.status
    }

    pub fn is_busy(&self) -> bool {
        matches!(
            self.status,
            AgentStatus::Busy | AgentStatus::Starting | AgentStatus::Aborting
        )
    }

    pub fn set_status(&mut self, status: AgentStatus, cx: &mut Context<Self>) {
        if self.status != status {
            self.status = status;
            cx.emit(AgentEvent::StatusChanged);
            cx.notify();
        }
    }

    fn with_backend(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut dyn AgentBackend, &mut Self, &mut Context<Self>),
    ) {
        if let Some(mut b) = self.backend.take() {
            f(b.as_mut(), self, cx);
            self.backend = Some(b);
        }
    }

    pub fn prompt(&mut self, text: String, cx: &mut Context<Self>) {
        if self.name.as_ref() == UNNAMED {
            self.name = name_from_prompt(&text).into();
            cx.notify();
        }
        self.with_backend(cx, |b, a, cx| b.prompt(text, a, cx));
    }
    /// A prompt with images attached.
    pub fn prompt_with_images(
        &mut self,
        text: String,
        images: Vec<ImageAttachment>,
        cx: &mut Context<Self>,
    ) {
        self.pending_images = images;
        self.prompt(text, cx);
        self.pending_images.clear();
    }

    pub fn unreviewed(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| !c.reviewed && !c.reverted)
            .count()
    }

    pub fn set_reviewed(&mut self, path: &std::path::Path, reviewed: bool, cx: &mut Context<Self>) {
        if let Some(c) = self.changes.iter_mut().find(|c| c.path == path) {
            c.reviewed = reviewed;
            if reviewed {
                // Reviewed in Changes = walked through.
                c.seen = true;
                c.notes.iter_mut().for_each(|n| n.seen = true);
                for p in self.pins.iter_mut().filter(|p| p.path == path) {
                    p.seen = true;
                }
            }
            self.persist_changes(cx);
            cx.notify();
        }
    }

    /// Save the change list next to pi's session, so it survives a restart.
    fn persist_changes(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session_file.clone() else {
            return;
        };
        let saved = crate::changes::Saved {
            changes: self.changes.clone(),
            pins: self.pins.clone(),
            progress: self.progress.clone(),
            catch_up: self.catch_up.clone(),
        };
        let bg = cx.background_executor().clone();
        self.persist = Some(cx.background_executor().spawn(async move {
            bg.timer(std::time::Duration::from_millis(300)).await;
            if let Err(e) = crate::changes::save(&session, &saved) {
                log::warn!("save change list: {e}");
            }
        }));
    }

    /// A resumed session: bring back its saved change list.
    pub fn load_changes(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session_file.clone() else {
            return;
        };
        let load = cx
            .background_executor()
            .spawn(async move { crate::changes::load(&session) });
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let loaded = load.await;
            let _ = this.update(cx, |this, cx| {
                if loaded.is_empty() {
                    return;
                }
                // Merge: something may have arrived while the file was read.
                let mut paths = Vec::new();
                if this.changes.is_empty() {
                    paths = loaded.changes.iter().map(|c| c.path.clone()).collect();
                    this.changes = loaded.changes;
                }
                let mut pins: Vec<Pin> = loaded
                    .pins
                    .into_iter()
                    .filter(|p| !this.pins.iter().any(|q| q.id == p.id))
                    .collect();
                if !pins.is_empty() {
                    pins.append(&mut this.pins);
                    this.pins = pins;
                    this.current_stop = None; // pin indices moved
                }
                if this.progress.is_empty() {
                    this.progress = loaded.progress;
                }
                if this.catch_up.is_none() {
                    this.catch_up = loaded.catch_up;
                }
                this.persist_changes(cx);
                for p in paths {
                    this.refresh_stats(p, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Write the before-snapshot back (or delete a file pi created).
    pub fn revert(
        &mut self,
        path: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let Some(change) = self.changes.iter().find(|c| c.path == path) else {
            return Task::ready(Ok(()));
        };
        let (path, before) = (change.path.clone(), change.before.clone());
        let write = cx.background_executor().spawn({
            let path = path.clone();
            async move { crate::changes::revert(&path, before.as_deref()) }
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            write.await?;
            this.update(cx, |this, cx| {
                if let Some(c) = this.changes.iter_mut().find(|c| c.path == path) {
                    c.reverted = true;
                    c.stats = Some((0, 0));
                }
                this.persist_changes(cx);
                cx.notify();
            })
        })
    }

    /// Recompute +/− for a changed file against the disk (off the UI thread).
    fn refresh_stats(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let Some(change) = self.changes.iter().find(|c| c.path == path) else {
            return;
        };
        let before = change.before.clone();
        let work = cx.background_executor().spawn({
            let path = path.clone();
            async move {
                let now = crate::changes::snapshot(&path);
                let (b, n) = (
                    before.as_deref().unwrap_or(""),
                    now.as_deref().unwrap_or(""),
                );
                let stats = crate::changes::line_stats(b, n);
                let first = crate::changes::hunks(b, n)
                    .first()
                    .map(|h| h.after.start + 1);
                (stats, now.is_none(), first)
            }
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let (stats, gone, first) = work.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(c) = this.changes.iter_mut().find(|c| c.path == path) {
                    c.stats = Some(stats);
                    c.first_line = first;
                    c.deleted_now = gone && c.before.is_some();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Answer the dialog `id` and drop its card.
    pub fn answer_dialog(&mut self, id: &str, answer: DialogAnswer, cx: &mut Context<Self>) {
        if !self.dialogs.iter().any(|d| d.id == id) {
            return;
        }
        self.dialogs.retain(|d| d.id != id);
        self.waiting = !self.dialogs.is_empty();
        let id = id.to_string();
        self.with_backend(cx, |b, a, cx| b.answer_dialog(id, answer, a, cx));
        cx.notify();
    }

    /// Send while busy: steer (`follow_up` false) or queue a follow-up.
    pub fn queue(&mut self, text: String, follow_up: bool, cx: &mut Context<Self>) {
        self.with_backend(cx, |b, a, cx| b.queue(text, follow_up, a, cx));
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

    pub fn request_models(&mut self, cx: &mut Context<Self>) {
        self.with_backend(cx, |b, a, cx| b.request_models(a, cx));
    }

    pub fn request_thinking_levels(&mut self, cx: &mut Context<Self>) {
        self.with_backend(cx, |b, a, cx| b.request_thinking_levels(a, cx));
    }

    pub fn set_thinking(&mut self, level: String, cx: &mut Context<Self>) {
        self.with_backend(cx, |b, a, cx| b.set_thinking(level, a, cx));
    }

    pub fn rename(&mut self, name: String, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() || name == self.name.as_ref() {
            return;
        }
        self.name = name.into();
        cx.notify();
        self.with_backend(cx, |b, a, cx| b.renamed(a, cx));
    }

    pub fn export_html(&mut self, cx: &mut Context<Self>) {
        self.with_backend(cx, |b, a, cx| b.export_html(a, cx));
    }

    pub fn compact(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() {
            self.handle_event(
                AgentEvent::Notify {
                    message: "pi is working; compact when it's done.".into(),
                    error: false,
                },
                cx,
            );
            return;
        }
        self.with_backend(cx, |b, a, cx| b.compact(a, cx));
    }

    /// Fetch the slash commands once (starts the agent if needed).
    pub fn request_commands(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.commands_requested, true) {
            return;
        }
        self.with_backend(cx, |b, a, cx| b.request_commands(a, cx));
    }

    /// See [`AgentBackend::ask_aside`].
    pub fn ask_aside(&mut self, prompt: String, cx: &mut Context<Self>) -> Task<anyhow::Result<String>> {
        let mut task = None;
        self.with_backend(cx, |b, a, cx| task = Some(b.ask_aside(prompt, a, cx)));
        task.unwrap_or_else(|| Task::ready(Err(anyhow::anyhow!("no backend"))))
    }

    /// Keep a new "catch me up" briefing (saved with the session).
    pub fn set_catch_up(&mut self, catch_up: Option<crate::collab::CatchUp>, cx: &mut Context<Self>) {
        self.catch_up = catch_up;
        self.persist_changes(cx);
        cx.notify();
    }

    pub fn set_model(&mut self, provider: String, id: String, cx: &mut Context<Self>) {
        self.with_backend(cx, |b, a, cx| b.set_model(provider, id, a, cx));
    }

    /// pi's settings changed: restart its process so it reads them (pi has no
    /// reload over RPC). Idle sessions restart now and resume on the next
    /// message; a session that's working restarts when its run finishes.
    pub fn reload_config(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() || self.waiting {
            self.reload_pending = true;
        } else {
            self.restart(cx);
        }
    }

    /// Backends call this (from a `cx.spawn` task) for everything they receive.
    pub fn handle_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match &event {
            AgentEvent::Settled { .. } => {
                self.queue = (Vec::new(), Vec::new());
                self.dialogs.clear();
                self.waiting = false;
                self.set_status(AgentStatus::Idle, cx);
                if std::mem::take(&mut self.reload_pending) {
                    self.restart(cx);
                }
            }
            AgentEvent::AssistantStart | AgentEvent::ToolStart { .. } => {
                if !matches!(self.status, AgentStatus::Aborting) {
                    self.set_status(AgentStatus::Busy, cx)
                }
            }
            AgentEvent::FileTouched {
                tool_call_id,
                tool,
                path,
                before,
            } => {
                let touch = Touch {
                    tool_call_id: tool_call_id.clone(),
                    tool: tool.clone(),
                };
                match self.changes.iter_mut().find(|c| &c.path == path) {
                    Some(c) => {
                        c.touches.push(touch);
                        crate::walk::touched_again(c);
                        if c.reverted {
                            // Reverted, then touched again: the new baseline is what was on disk now.
                            c.reverted = false;
                            c.before = before.clone();
                        }
                    }
                    None => self
                        .changes
                        .push(FileChange::new(path.clone(), before.clone(), touch)),
                }
                if tool == "bash" {
                    // Reported after the call ended (see agent-pi's bash_watch).
                    self.refresh_stats(path.clone(), cx);
                } else {
                    self.pending_touches
                        .push((tool_call_id.clone(), path.clone()));
                }
                self.persist_changes(cx);
                cx.notify();
            }
            AgentEvent::ToolEnd { id, .. } => {
                if let Some(i) = self.pending_touches.iter().position(|(t, _)| t == id) {
                    let (_, path) = self.pending_touches.remove(i);
                    self.refresh_stats(path, cx);
                }
            }
            AgentEvent::Dialog(req) => {
                self.dialogs.push(req.clone());
                self.waiting = true;
                if let Some(ms) = req.timeout_ms {
                    // pi resolves it itself after the timeout; drop our card then.
                    let id = req.id.clone();
                    self._dialog_timers
                        .push(cx.spawn(async move |this: WeakEntity<Self>, cx| {
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(ms))
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                if this.dialogs.iter().any(|d| d.id == id) {
                                    this.dialogs.retain(|d| d.id != id);
                                    this.waiting = !this.dialogs.is_empty();
                                    cx.emit(AgentEvent::Notify {
                                        message:
                                            "A pi question timed out; pi used its default answer."
                                                .into(),
                                        error: false,
                                    });
                                    cx.notify();
                                }
                            });
                        }));
                }
                cx.notify();
            }
            AgentEvent::Queue {
                steering,
                follow_up,
            } => {
                self.queue = (steering.clone(), follow_up.clone());
                cx.notify();
            }
            AgentEvent::Pin(pin) => {
                self.pins.retain(|p| p.id != pin.id);
                self.pins.push(pin.clone());
                self.persist_changes(cx);
                cx.notify();
            }
            AgentEvent::ExplainChange {
                path,
                summary,
                hunks,
            } => {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                crate::walk::explain(
                    &mut self.changes,
                    &mut self.pins,
                    &mut self.current_stop,
                    path,
                    summary,
                    hunks,
                    now_ms,
                );
                self.persist_changes(cx);
                cx.notify();
            }
            AgentEvent::Progress(steps) => {
                self.progress = steps.clone();
                self.persist_changes(cx);
                cx.notify();
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

/// Collaborator mode: the walkthrough (logic in [`crate::walk`], on plain data).
impl Agent {
    /// See [`crate::walk::stops`].
    pub fn stops(&self) -> Vec<Stop> {
        crate::walk::stops(&self.changes, &self.pins)
    }

    /// The anchor text behind a stop (to re-find it in an edited buffer).
    pub fn anchor_text(&self, at: &StopRef) -> Option<String> {
        crate::walk::anchor_text(&self.changes, &self.pins, at)
    }

    /// The stop of the pin made by tool call `id`.
    pub fn pin_stop_by_id(&self, id: &str) -> Option<Stop> {
        let i = self.pins.iter().position(|p| p.id == id)?;
        Some(crate::walk::pin_stop(&self.pins, i))
    }

    /// Stops not stepped onto yet.
    pub fn unseen_stops(&self) -> usize {
        self.stops().iter().filter(|s| !s.seen).count()
    }

    /// Has changes or pins you haven't stepped through or reviewed yet.
    pub fn needs_review(&self) -> bool {
        self.unseen_stops() > 0
    }

    /// See [`crate::walk::step`].
    pub fn step_stop(&self, delta: isize) -> Option<Stop> {
        crate::walk::step(&self.stops(), self.current_stop.as_ref(), delta)
    }

    /// The stop the walkthrough is on, if it still exists.
    pub fn current(&self) -> Option<Stop> {
        let c = self.current_stop.as_ref()?;
        self.stops().into_iter().find(|s| &s.at == c)
    }

    /// Make `at` the current stop and mark it seen (see [`crate::walk::visit`]).
    pub fn visit_stop(&mut self, at: &StopRef, cx: &mut Context<Self>) {
        crate::walk::visit(&mut self.changes, &mut self.pins, &mut self.current_stop, at);
        self.persist_changes(cx);
        cx.notify();
    }

    /// Hide a pin or hunk note (see [`crate::walk::dismiss`]).
    pub fn dismiss_stop(&mut self, at: &StopRef, cx: &mut Context<Self>) {
        crate::walk::dismiss(&mut self.changes, &mut self.pins, &mut self.current_stop, at);
        self.persist_changes(cx);
        cx.notify();
    }
}

/// "fix the flaky replay test please" -> "fix the flaky replay test"
pub fn name_from_prompt(text: &str) -> String {
    let (_, text) = crate::context::split_prompt(text); // not the editor-context block
    let words: Vec<&str> = text.split_whitespace().take(5).collect();
    let mut name = words.join(" ");
    if name.chars().count() > 32 {
        name = name.chars().take(31).collect::<String>() + "…";
    }
    if name.is_empty() {
        UNNAMED.to_string()
    } else {
        name
    }
}
