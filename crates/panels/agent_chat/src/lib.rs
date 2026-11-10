//! The pi panel: several sessions (tabs when narrow, a sidebar when wide),
//! each with its own conversation; send, stream, tool calls, stop, crash
//! recovery. Talks only to `Sessions` / `Agent` from core.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelInfo, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{InputEvent, Paste, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::sessions::{SavedSession, list_saved, user_text};
use pide_core::{
    Agent, AgentEvent, AgentStatus, CommandRegistry, CommandSpec, DialogAnswer, DialogKind,
    DialogRequest, DockPosition, Panel, Services, Sessions, SessionsEvent,
};
use serde_json::{Value, json};

mod model_picker;
mod rename;
mod slash;

pub const PANEL_NAME: &str = "agent_chat";

actions!(pide_chat, [QueueFollowUp, CompleteSlash, BackToEditor]);
/// At this width or more the panel shows the session sidebar instead of tabs.
const SIDEBAR_MIN_WIDTH: f32 = 640.;

pub fn init(cx: &mut App) {
    // Option+Enter in the pi input queues a follow-up (Enter steers while pi works).
    cx.bind_keys([
        KeyBinding::new("alt-enter", QueueFollowUp, Some("AgentInput")),
        // Tab completes a suggested slash command (otherwise moves focus on).
        KeyBinding::new("tab", CompleteSlash, Some("AgentInput")),
        // Esc goes back to the code (JetBrains tool-window habit).
        KeyBinding::new("escape", BackToEditor, Some("AgentInput")),
    ]);
    register_panel(cx, PANEL_NAME, |ctx, window, cx| {
        // Reopen the sessions that were open when the layout was saved.
        if let PanelInfo::Panel(v) = ctx.info() {
            let files: Vec<(PathBuf, Option<String>)> = v
                .get("sessions")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|s| {
                            let file = s.get("file").and_then(Value::as_str)?;
                            let name = s.get("name").and_then(Value::as_str).map(str::to_string);
                            Some((PathBuf::from(file), name))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let active = v.get("active").and_then(Value::as_u64).unwrap_or(0) as usize;
            let sessions = Services::of(window, cx).sessions;
            sessions.update(cx, |s, cx| s.restore(files, active, cx));
        }
        panel_handle(cx.new(|cx| AgentChat::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "pi",
            icon: gpui_kit::component::IconName::Bot,
            dock: pide_core::DockPosition::Right,
            command: "view.toggle_agent",
            key: Some("cmd-0"),
            create: |window, cx| panel_handle(cx.new(|cx| AgentChat::new(window, cx))),
        },
    );

    let active = |window: &Window, cx: &App| Services::active_agent(window, cx);
    let sessions = |window: &Window, cx: &App| Services::of(window, cx).sessions;
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.abort", "Agent: Stop").key("cmd-f2"),
        move |window, cx| active(window, cx).update(cx, |a, cx| a.abort(cx)),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.rename", "Agent: Rename Session…"),
        move |window, cx| rename::open(active(window, cx), window, cx),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.export", "Agent: Export Session as HTML"),
        move |window, cx| active(window, cx).update(cx, |a, cx| a.export_html(cx)),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.compact", "Agent: Compact Session"),
        move |window, cx| active(window, cx).update(cx, |a, cx| a.compact(cx)),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.restart", "Agent: Restart pi"),
        move |window, cx| active(window, cx).update(cx, |a, cx| a.restart(cx)),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.new_session", "Agent: New Session"),
        move |window, cx| {
            sessions(window, cx).update(cx, |s, cx| {
                s.add(None, None, cx);
            })
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.close_session", "Agent: Close Session"),
        move |window, cx| {
            let a = active(window, cx);
            sessions(window, cx).update(cx, |s, cx| s.close(&a, cx))
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.next_session", "Agent: Next Session").key("ctrl-tab"),
        move |window, cx| sessions(window, cx).update(cx, |s, cx| s.cycle(1, cx)),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.prev_session", "Agent: Previous Session").key("ctrl-shift-tab"),
        move |window, cx| sessions(window, cx).update(cx, |s, cx| s.cycle(-1, cx)),
    );
}

#[derive(Clone, Copy, PartialEq)]
enum ToolState {
    Running,
    Done,
    Failed,
}

enum Item {
    /// (editor-context chip, the user's text)
    User(Option<String>, String),
    Assistant {
        markdown: Entity<TextViewState>,
        text: String,
        last_index: Option<usize>,
        thinking: String,
        thinking_open: bool,
        error: Option<String>,
    },
    Tool {
        id: String,
        name: String,
        summary: String,
        state: ToolState,
        output: String,
        duration_ms: Option<u64>,
    },
    Notice {
        text: String,
        error: bool,
    },
}

/// One session's conversation as shown in the panel.
#[derive(Default)]
struct SessionView {
    items: Vec<Item>,
    scroll: ScrollHandle,
    history_requested: bool,
}

/// Plain (`Send`) history parsed from a pi session file off the UI thread.
enum HistItem {
    User(String),
    Text(String),
    Tool {
        id: String,
        name: String,
        summary: String,
        output: String,
        failed: bool,
    },
    /// The session's model / thinking level as last recorded (shown before pi starts).
    Model {
        provider: String,
        id: String,
    },
    Thinking(String),
}

#[derive(Default)]
struct Instances(HashMap<WindowId, WeakEntity<AgentChat>>);
impl Global for Instances {}

/// This window's pi panel, if it exists.
pub fn existing(window: &Window, cx: &App) -> Option<Entity<AgentChat>> {
    cx.try_global::<Instances>()?
        .0
        .get(&window.window_handle().window_id())?
        .upgrade()
}

pub struct AgentChat {
    sessions: Entity<Sessions>,
    workspace: Entity<pide_core::Workspace>,
    /// What "send editor context" would send now; refreshed each render.
    context_now: Option<pide_core::context::EditorContext>,
    /// Re-checks the editor context twice a second (the editor has no
    /// selection-changed event), redrawing only when it changed.
    _context_poll: Task<()>,
    views: HashMap<EntityId, SessionView>,
    agent_subs: HashMap<EntityId, Subscription>,
    input: Entity<TextareaState>,
    /// Images pasted into the composer, sent with the next prompt.
    attachments: Vec<(pide_core::ImageAttachment, std::sync::Arc<Image>)>,
    /// Image counts to note under the next prompt each session echoes.
    image_notes: HashMap<EntityId, usize>,
    /// Suggestions were showing at the last keystroke.
    slash_open: bool,
    /// Project files for `@` mentions, listed on the first `@`.
    files: Option<Rc<Vec<String>>>,
    files_loading: bool,
    focus: FocusHandle,
    /// Last laid-out width, measured each frame (tabs vs sidebar).
    width: Rc<Cell<f32>>,
    /// Narrow mode: the ▾ session list is showing instead of the conversation.
    show_list: bool,
    /// The active session shows its Changes list instead of the chat.
    show_changes: bool,
    saved: Vec<SavedSession>,
    _load_saved: Option<Task<()>>,
    /// Text fields for pending `input`/`editor` dialogs, by dialog id.
    dialog_inputs: HashMap<String, (Entity<TextareaState>, Subscription)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for AgentChat {}

/// One-line description of a tool call's arguments.
fn summarize_args(name: &str, args: &Value) -> String {
    let field = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let s = match name {
        "bash" => field("command"),
        "read" | "write" | "edit" | "ls" => field("path"),
        "grep" | "find" => field("pattern"),
        _ => None,
    }
    .unwrap_or_else(|| args.to_string());
    let s = s.replace('\n', " ⏎ ");
    if s.chars().count() > 160 {
        format!("{}…", s.chars().take(160).collect::<String>())
    } else {
        s
    }
}

/// The text of a tool result (`content` blocks), clipped for display.
fn result_text(result: &Value) -> String {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| result.to_string());
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > 8 {
        format!(
            "{}\n… {} more lines",
            lines[..8].join("\n"),
            lines.len() - 8
        )
    } else {
        text.trim_end().to_string()
    }
}

/// Conversation history from a pi session file (`docs/session-format.md`).
/// Branches are flattened in file order — good enough to read back.
/// The entries of the session's current branch, root first: pi sessions are
/// trees (`id` / `parentId`), and the branch in use ends at the last entry.
/// Entries without ids (older files) are kept in file order.
fn current_branch(entries: Vec<Value>) -> Vec<Value> {
    let ids: std::collections::HashMap<String, usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            v.get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), i))
        })
        .collect();
    let Some(last) = entries.iter().rposition(|v| v.get("id").is_some()) else {
        return entries;
    };
    let mut chain = vec![last];
    let mut at = last;
    while let Some(parent) = entries[at].get("parentId").and_then(Value::as_str) {
        match ids.get(parent) {
            Some(&p) if !chain.contains(&p) => {
                chain.push(p);
                at = p;
            }
            _ => break,
        }
    }
    chain.reverse();
    // Entries outside the tree (no id: the session header) stay where they were.
    let keep: std::collections::HashSet<usize> = chain.iter().copied().collect();
    entries
        .into_iter()
        .enumerate()
        .filter(|(i, v)| keep.contains(i) || v.get("id").is_none())
        .map(|(_, v)| v)
        .collect()
}

fn parse_history(path: &std::path::Path) -> Vec<HistItem> {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let entries: Vec<Value> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .collect();
    let mut out: Vec<HistItem> = Vec::new();
    for v in current_branch(entries) {
        match v.get("type").and_then(Value::as_str) {
            Some("message") => {}
            Some("model_change") => {
                if let (Some(p), Some(m)) = (v["provider"].as_str(), v["modelId"].as_str()) {
                    out.push(HistItem::Model {
                        provider: p.into(),
                        id: m.into(),
                    });
                }
                continue;
            }
            Some("thinking_level_change") => {
                if let Some(t) = v["thinkingLevel"].as_str() {
                    out.push(HistItem::Thinking(t.into()));
                }
                continue;
            }
            _ => continue,
        }
        let m = &v["message"];
        match m.get("role").and_then(Value::as_str) {
            Some("user") => {
                if let Some(t) = user_text(m) {
                    out.push(HistItem::User(t));
                }
            }
            Some("assistant") => {
                for block in m
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let t = block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if !t.trim().is_empty() {
                                out.push(HistItem::Text(t.to_string()));
                            }
                        }
                        Some("toolCall") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            out.push(HistItem::Tool {
                                id: block
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                summary: summarize_args(&name, &block["arguments"]),
                                name,
                                output: String::new(),
                                failed: false,
                            });
                        }
                        _ => {}
                    }
                }
            }
            Some("toolResult") => {
                let id = m
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let failed = m.get("isError").and_then(Value::as_bool).unwrap_or(false);
                let text = result_text(m);
                if let Some(HistItem::Tool {
                    output, failed: f, ..
                }) = out
                    .iter_mut()
                    .rev()
                    .find(|h| matches!(h, HistItem::Tool { id: tid, .. } if tid == id))
                {
                    *output = text;
                    *f = failed;
                }
            }
            _ => {}
        }
    }
    out
}

impl AgentChat {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let me = cx.entity().downgrade();
        cx.default_global::<Instances>()
            .0
            .insert(window.window_handle().window_id(), me);
        let Services {
            sessions,
            workspace,
        } = Services::of(window, cx);
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .submit_on_enter(true)
                .placeholder("Ask pi…  (Enter to send, Shift+Enter for a new line)")
        });
        let subs = vec![
            cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::PressEnter { shift: false, .. } => this.send(false, window, cx),
                    InputEvent::Change => {
                        // Typing `/`: fetch the session's commands for the suggestions.
                        // Redraw only for the suggestions: a redraw per keystroke
                        // re-renders the whole conversation.
                        let value = this.input.read(cx).value();
                        let is_slash = slash::typed(&value).is_some();
                        let is_mention = slash::mention(&value).is_some();
                        if is_slash {
                            this.active(cx).update(cx, |a, cx| a.request_commands(cx));
                        }
                        if is_mention {
                            this.load_files(cx);
                        }
                        let slash = is_slash || is_mention;
                        if slash || std::mem::replace(&mut this.slash_open, slash) {
                            this.slash_open = slash;
                            cx.notify();
                        }
                    }
                    _ => {}
                }
            }),
            cx.subscribe_in(
                &sessions,
                window,
                |this, _, ev: &SessionsEvent, window, cx| {
                    let SessionsEvent::Changed = ev;
                    this.sync_sessions(window, cx);
                    this.show_list = false;
                    cx.emit(PanelEvent::LayoutChanged); // open sessions are part of the saved layout
                },
            ),
            cx.observe(&sessions, |_, _, cx| cx.notify()),
            // Files appeared or went away: list them again on the next `@`.
            cx.subscribe(&workspace, |this, _, ev: &pide_core::WorkspaceEvent, _| {
                if matches!(ev, pide_core::WorkspaceEvent::TreeChanged) {
                    this.files = None;
                }
            }),
            cx.subscribe(&workspace, |_, _, ev: &pide_core::WorkspaceEvent, cx| {
                if matches!(ev, pide_core::WorkspaceEvent::ActiveBufferChanged) {
                    cx.notify();
                }
            }),
        ];
        let mut this = Self {
            sessions,
            workspace,
            context_now: None,
            _context_poll: cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(500))
                        .await;
                    let changed = this.update_in(cx, |this, window, cx| {
                        let now = pide_core::context::current(window, cx);
                        let changed = now != this.context_now;
                        if changed {
                            cx.notify();
                        }
                    });
                    if changed.is_err() {
                        break;
                    }
                }
            }),
            views: HashMap::new(),
            agent_subs: HashMap::new(),
            input,
            attachments: Vec::new(),
            image_notes: HashMap::new(),
            slash_open: false,
            files: None,
            files_loading: false,
            focus: cx.focus_handle(),
            width: Rc::new(Cell::new(0.)),
            show_list: false,
            show_changes: false,
            saved: Vec::new(),
            _load_saved: None,
            dialog_inputs: HashMap::new(),
            _subs: subs,
        };
        this.sync_sessions(window, cx);
        this.refresh_saved(cx);
        this
    }

    /// Subscribe to new sessions, drop old ones, load history for resumed ones.
    fn sync_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agents: Vec<Entity<Agent>> = self.sessions.read(cx).all().to_vec();
        let ids: Vec<EntityId> = agents.iter().map(|a| a.entity_id()).collect();
        self.agent_subs.retain(|id, _| ids.contains(id));
        self.views.retain(|id, _| ids.contains(id));
        for agent in agents {
            let id = agent.entity_id();
            self.agent_subs.entry(id).or_insert_with(|| {
                cx.subscribe_in(
                    &agent,
                    window,
                    |this, agent, ev: &AgentEvent, window, cx| {
                        this.on_agent_event(agent.clone(), ev, window, cx)
                    },
                )
            });
            let view = self.views.entry(id).or_default();
            if !view.history_requested
                && let Some(file) = agent.read(cx).session_file.clone()
            {
                view.history_requested = true;
                self.load_history(id, agent.downgrade(), file, cx);
            }
        }
        cx.notify();
    }

    fn load_history(
        &mut self,
        id: EntityId,
        agent: WeakEntity<Agent>,
        file: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let parse = cx
            .background_executor()
            .spawn(async move { parse_history(&file) });
        cx.spawn(async move |this, cx| {
            let mut hist = parse.await;
            let _ = this.update(cx, |this, cx| {
                // Model and thinking level as last recorded, until pi reports its own.
                let mut model = None;
                let mut thinking = None;
                hist.retain(|h| match h {
                    HistItem::Model { provider, id } => {
                        model = Some((provider.clone(), id.clone()));
                        false
                    }
                    HistItem::Thinking(t) => {
                        thinking = Some(t.clone());
                        false
                    }
                    _ => true,
                });
                let _ = agent.update(cx, |a, cx| {
                    if a.model.is_none() {
                        a.model = model.map(|(provider, id)| pide_core::ModelInfo {
                            provider,
                            id,
                            name: String::new(),
                            reasoning: thinking.as_deref().is_some_and(|t| t != "off"),
                        });
                        a.thinking = thinking;
                        cx.notify();
                    }
                });
                let mut items: Vec<Item> = hist
                    .into_iter()
                    .map(|h| match h {
                        HistItem::User(t) => {
                            let (chip, text) = pide_core::context::split_prompt(&t);
                            Item::User(chip, text.to_string())
                        }
                        HistItem::Text(t) => Item::Assistant {
                            markdown: cx.new(|cx| TextViewState::markdown(&t, cx)),
                            text: t,
                            last_index: None,
                            thinking: String::new(),
                            thinking_open: false,
                            error: None,
                        },
                        HistItem::Tool {
                            id,
                            name,
                            summary,
                            output,
                            failed,
                        } => Item::Tool {
                            id,
                            name,
                            summary,
                            state: if failed {
                                ToolState::Failed
                            } else {
                                ToolState::Done
                            },
                            output,
                            duration_ms: None,
                        },
                        HistItem::Model { .. } | HistItem::Thinking(_) => {
                            unreachable!("filtered above")
                        }
                    })
                    .collect();
                if let Some(view) = this.views.get_mut(&id) {
                    if !items.is_empty() {
                        items.push(Item::Notice {
                            text:
                                "Resumed from disk. pi continues this session on your next message."
                                    .into(),
                            error: false,
                        });
                    }
                    // History goes before anything that arrived since.
                    items.append(&mut view.items);
                    view.items = items;
                    view.scroll.scroll_to_bottom();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_saved(&mut self, cx: &mut Context<Self>) {
        let root = self.workspace.read(cx).root().to_path_buf();
        let list = cx
            .background_executor()
            .spawn(async move { list_saved(&root) });
        self._load_saved = Some(cx.spawn(async move |this, cx| {
            let saved = list.await;
            let _ = this.update(cx, |this, cx| {
                this.saved = saved;
                cx.notify();
            });
        }));
    }

    fn active(&self, cx: &App) -> Entity<Agent> {
        self.sessions.read(cx).active()
    }

    /// Enter: send, or steer if pi is working. Option+Enter: queue a follow-up.
    fn send(&mut self, follow_up: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        let agent = self.active(cx);
        let busy = agent.read(cx).is_busy();
        if !self.attachments.is_empty() && busy {
            window.push_notification(
                Notification::warning("Images go with a new prompt: send them when pi is done."),
                cx,
            );
            return;
        }
        let images: Vec<_> = self.attachments.drain(..).map(|(a, _)| a).collect();
        self.input.update(cx, |i, cx| i.set_value("", window, cx));
        // New prompts carry the editor context (when the toggle is on).
        let context = if busy || !self.send_context(cx) {
            None
        } else {
            pide_core::context::current(window, cx)
        };
        let text = match context {
            Some(ctx) => format!("{}{text}", ctx.block()),
            None => text,
        };
        if !images.is_empty() {
            self.image_notes.insert(agent.entity_id(), images.len());
        }
        let deliver = move |cx: &mut App| {
            agent.update(cx, |a, cx| match (busy, images.is_empty()) {
                (true, _) => a.queue(text, follow_up, cx),
                (false, true) => a.prompt(text, cx),
                (false, false) => a.prompt_with_images(text, images, cx),
            })
        };
        // pi reads files from disk: save unsaved edits first.
        let saves = self.save_dirty(cx);
        if saves.is_empty() {
            deliver(cx);
            return;
        }
        cx.spawn(async move |_, cx| {
            for t in saves {
                if let Err(e) = t.await {
                    log::warn!("save before sending to pi: {e:#}");
                }
            }
            cx.update(deliver);
        })
        .detach();
        cx.notify();
    }

    /// Replace the composer's text (a draft from elsewhere, e.g. the Run panel).
    pub fn set_draft(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.set_value(text, window, cx));
        cx.notify();
    }

    /// Save every unsaved, writable file of this project (setting `agent.save_before_send`).
    fn save_dirty(&self, cx: &mut Context<Self>) -> Vec<Task<anyhow::Result<()>>> {
        let on = cx
            .try_global::<pide_core::settings::Settings>()
            .is_none_or(|s| s.save_before_send);
        if !on {
            return Vec::new();
        }
        let dirty: Vec<_> = self
            .workspace
            .read(cx)
            .buffers()
            .iter()
            .filter(|b| {
                let b = b.read(cx);
                b.is_dirty() && b.read_only.is_none()
            })
            .cloned()
            .collect();
        dirty
            .iter()
            .map(|b| self.workspace.update(cx, |w, cx| w.save(b, cx)))
            .collect()
    }

    /// Cmd+V with an image on the clipboard: attach it instead of pasting text.
    fn paste_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        let images: Vec<Image> = item
            .entries()
            .iter()
            .filter_map(|e| match e {
                ClipboardEntry::Image(i) => Some(i.clone()),
                _ => None,
            })
            .collect();
        if images.is_empty() {
            return false;
        }
        for i in images {
            let att = pide_core::ImageAttachment {
                mime: i.format.mime_type().to_string(),
                data: std::sync::Arc::new(i.bytes.clone()),
            };
            self.attachments.push((att, std::sync::Arc::new(i)));
        }
        cx.notify();
        true
    }

    /// What's suggested under the composer: slash commands while typing `/name`,
    /// project files while typing `@path`.
    fn suggestions(&self, cx: &App) -> Vec<slash::Suggestion> {
        let text = self.input.read(cx).value().to_string();
        if let Some(q) = slash::typed(&text) {
            let agent = self.active(cx);
            let commands = &agent.read(cx).commands;
            return slash::matches(commands, q)
                .into_iter()
                .map(|i| {
                    let c = &commands[i];
                    slash::Suggestion {
                        label: format!("/{}", c.name),
                        detail: c.description.clone(),
                        tag: c.source.clone(),
                        replace_with: format!("/{} ", c.name),
                    }
                })
                .collect();
        }
        if let (Some((start, q)), Some(files)) = (slash::mention(&text), self.files.as_ref()) {
            return slash::file_matches(files, q, 8)
                .into_iter()
                .map(|i| slash::Suggestion {
                    label: format!("@{}", files[i]),
                    detail: String::new(),
                    tag: "file".into(),
                    replace_with: format!("{}@{} ", &text[..start], files[i]),
                })
                .collect();
        }
        Vec::new()
    }

    /// Take a suggestion: the composer gets its text.
    fn complete(&mut self, s: &slash::Suggestion, window: &mut Window, cx: &mut Context<Self>) {
        let text = s.replace_with.clone();
        self.input.update(cx, |i, cx| {
            i.set_value(text, window, cx);
            i.focus(window, cx);
        });
        cx.notify();
    }

    /// List the project's files (gitignore-aware) for `@` mentions, once.
    fn load_files(&mut self, cx: &mut Context<Self>) {
        if self.files.is_some() || self.files_loading {
            return;
        }
        self.files_loading = true;
        let root = self.workspace.read(cx).root().to_path_buf();
        let list = cx.background_executor().spawn(async move {
            let mut out = Vec::new();
            let walker = ignore::WalkBuilder::new(&root)
                .hidden(false)
                .filter_entry(|e| e.file_name() != ".git" && e.file_name() != ".DS_Store")
                .build();
            for e in walker.flatten() {
                if e.file_type().is_some_and(|t| t.is_file())
                    && let Ok(rel) = e.path().strip_prefix(&root)
                {
                    out.push(rel.display().to_string());
                    if out.len() >= 50_000 {
                        break;
                    }
                }
            }
            out.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
            out
        });
        cx.spawn(async move |this, cx| {
            let files = list.await;
            let _ = this.update(cx, |this, cx| {
                this.files = Some(Rc::new(files));
                this.files_loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// A desktop notification, only while pide isn't the active app.
    fn desktop_notify(window: &Window, cx: &App, title: &str, body: String) {
        let on = cx
            .try_global::<pide_core::settings::Settings>()
            .is_none_or(|s| s.notify);
        if on && !window.is_window_active() {
            log::info!("desktop notification: {title}");
            pide_runtime::process::desktop_notification(title.to_string(), body);
        }
    }

    fn send_context(&self, cx: &App) -> bool {
        cx.try_global::<pide_core::settings::Settings>()
            .is_none_or(|s| s.send_context)
    }

    fn toggle_context(&mut self, cx: &mut Context<Self>) {
        let on = !self.send_context(cx);
        if cx.has_global::<pide_core::settings::Settings>() {
            cx.global_mut::<pide_core::settings::Settings>()
                .send_context = on;
        }
        cx.background_executor()
            .spawn(async move {
                if let Err(e) =
                    pide_core::settings::save_key("agent.send_context", serde_json::json!(on))
                {
                    log::error!("save settings: {e}");
                }
            })
            .detach();
        cx.notify();
    }

    fn on_agent_event(
        &mut self,
        agent: Entity<Agent>,
        ev: &AgentEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_active = agent == self.active(cx);
        let name = agent.read(cx).name.clone();
        if let AgentEvent::Dialog(req) = ev {
            if !is_active {
                window.push_notification(
                    Notification::warning(format!(
                        "pi in \u{201c}{name}\u{201d} is waiting for your answer."
                    )),
                    cx,
                );
            }
            Self::desktop_notify(window, cx, "pi is asking", format!("{name}: {}", req.title));
            if matches!(req.kind, DialogKind::Input | DialogKind::Editor) {
                self.make_dialog_input(agent, req.clone(), window, cx);
            }
            cx.notify();
            return;
        }
        let image_note = matches!(ev, AgentEvent::UserPrompt { .. })
            .then(|| self.image_notes.remove(&agent.entity_id()))
            .flatten();
        let Some(view) = self.views.get_mut(&agent.entity_id()) else {
            return;
        };
        let items = &mut view.items;
        match ev {
            AgentEvent::UserPrompt { text } => {
                let (chip, text) = pide_core::context::split_prompt(text);
                items.push(Item::User(chip, text.to_string()));
                if let Some(n) = image_note {
                    items.push(Item::Notice {
                        text: format!("With {n} image{}.", if n == 1 { "" } else { "s" }),
                        error: false,
                    });
                }
            }
            AgentEvent::AssistantStart => {
                let markdown = cx.new(|cx| TextViewState::markdown("", cx));
                items.push(Item::Assistant {
                    markdown,
                    text: String::new(),
                    last_index: None,
                    thinking: String::new(),
                    thinking_open: false,
                    error: None,
                });
            }
            AgentEvent::TextDelta { index, delta } => {
                if let Some(Item::Assistant {
                    markdown,
                    text,
                    last_index,
                    ..
                }) = items.last_mut()
                {
                    let mut chunk = String::new();
                    if last_index.is_some_and(|i| i != *index) && !text.is_empty() {
                        chunk.push_str("\n\n");
                    }
                    *last_index = Some(*index);
                    chunk.push_str(delta);
                    text.push_str(&chunk);
                    markdown.update(cx, |m, cx| m.push_str(&chunk, cx));
                }
            }
            AgentEvent::TextEnd { content, .. } => {
                if let Some(Item::Assistant {
                    markdown,
                    text,
                    last_index,
                    ..
                }) = items.last_mut()
                    && last_index.is_some()
                    && !text.ends_with(content.as_str())
                    && !content.is_empty()
                {
                    *text = content.clone();
                    markdown.update(cx, |m, cx| m.set_text(content, cx));
                }
            }
            AgentEvent::ThinkingDelta { delta, .. } => {
                if let Some(Item::Assistant { thinking, .. }) = items.last_mut() {
                    thinking.push_str(delta);
                }
            }
            AgentEvent::AssistantEnd { error } => {
                if let Some(Item::Assistant { error: e, .. }) = items.last_mut() {
                    *e = error.clone();
                }
            }
            AgentEvent::ToolStart { id, name, args } => items.push(Item::Tool {
                id: id.clone(),
                name: name.clone(),
                summary: summarize_args(name, args),
                state: ToolState::Running,
                output: String::new(),
                duration_ms: None,
            }),
            AgentEvent::ToolUpdate { id, partial } => {
                if let Some(Item::Tool { output, .. }) = tool_mut(items, id) {
                    *output = result_text(partial);
                }
            }
            AgentEvent::ToolEnd {
                id,
                result,
                is_error,
                duration_ms,
                ..
            } => {
                if let Some(Item::Tool {
                    state,
                    output,
                    duration_ms: d,
                    ..
                }) = tool_mut(items, id)
                {
                    *state = if *is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Done
                    };
                    *output = result_text(result);
                    *d = *duration_ms;
                }
            }
            AgentEvent::Settled { aborted } => {
                if *aborted {
                    items.push(Item::Notice {
                        text: "Stopped.".into(),
                        error: false,
                    });
                }
                if !is_active {
                    window.push_notification(
                        Notification::info(format!("pi finished in \u{201c}{name}\u{201d}.")),
                        cx,
                    );
                }
                if !*aborted {
                    Self::desktop_notify(window, cx, "pi finished", name.to_string());
                }
            }
            AgentEvent::Error(e) => items.push(Item::Notice {
                text: e.clone(),
                error: true,
            }),
            AgentEvent::Notify { message, error } => {
                let n = if *error {
                    Notification::error(message.clone())
                } else {
                    Notification::info(message.clone())
                };
                window.push_notification(n, cx);
            }
            AgentEvent::Unknown(v) => log::debug!("unknown agent record: {v}"),
            _ => {}
        }
        view.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// A text field for an `input` (Enter submits) or `editor` (Cmd+Enter submits) dialog.
    fn make_dialog_input(
        &mut self,
        agent: Entity<Agent>,
        req: DialogRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = req.kind == DialogKind::Editor;
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state = if editor {
                state.auto_grow(4, 14)
            } else {
                state.auto_grow(1, 3).submit_on_enter(true)
            };
            if let Some(p) = &req.placeholder {
                state = state.placeholder(p.clone());
            }
            if let Some(text) = &req.prefill {
                state = state.default_value(text.clone());
            }
            state
        });
        let id = req.id.clone();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, input, ev: &InputEvent, _, cx| {
                let submit = match ev {
                    InputEvent::PressEnter {
                        secondary,
                        shift: false,
                    } => !editor || *secondary,
                    _ => false,
                };
                if submit {
                    let value = input.read(cx).value().to_string();
                    this.answer(&agent, &id, DialogAnswer::Value(value), cx);
                }
            },
        );
        let focus = input.focus_handle(cx);
        self.dialog_inputs.insert(req.id, (input, sub));
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }

    fn answer(
        &mut self,
        agent: &Entity<Agent>,
        id: &str,
        answer: DialogAnswer,
        cx: &mut Context<Self>,
    ) {
        agent.update(cx, |a, cx| a.answer_dialog(id, answer, cx));
        self.dialog_inputs.remove(id);
        cx.notify();
    }

    fn render_dialog(
        &self,
        agent: &Entity<Agent>,
        d: &DialogRequest,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let btn = |label: &'static str,
                   id: &str,
                   answer: DialogAnswer,
                   primary: bool,
                   cx: &mut Context<Self>| {
            let (agent, id) = (agent.clone(), id.to_string());
            let b = Button::new(SharedString::from(format!("dlg-{id}-{label}")))
                .small()
                .label(label);
            let b = if primary { b.primary() } else { b };
            b.on_click(
                cx.listener(move |this, _, _, cx| this.answer(&agent, &id, answer.clone(), cx)),
            )
        };
        let mut card = v_flex()
            .gap_2()
            .p_3()
            .m_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.warning)
            .bg(theme.secondary)
            .text_sm()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.warning)
                    .child("PI IS WAITING FOR YOU"),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(d.title.clone()),
            )
            .when_some(d.message.clone(), |this, m| {
                this.child(div().text_color(theme.muted_foreground).child(m))
            });
        match d.kind {
            DialogKind::Confirm => {
                card = card.child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(btn("Cancel", &d.id, DialogAnswer::Cancelled, false, cx))
                        .child(btn(
                            "Deny",
                            &d.id,
                            DialogAnswer::Confirmed(false),
                            false,
                            cx,
                        ))
                        .child(btn("Allow", &d.id, DialogAnswer::Confirmed(true), true, cx)),
                );
            }
            DialogKind::Select => {
                let options: Vec<AnyElement> = d
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| {
                        let (agent, id, value) = (agent.clone(), d.id.clone(), o.clone());
                        Button::new(SharedString::from(format!("dlg-{}-opt-{i}", d.id)))
                            .small()
                            .w_full()
                            .label(o.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer(&agent, &id, DialogAnswer::Value(value.clone()), cx)
                            }))
                            .into_any_element()
                    })
                    .collect();
                card = card.child(v_flex().gap_1().children(options)).child(
                    h_flex().justify_end().child(btn(
                        "Cancel",
                        &d.id,
                        DialogAnswer::Cancelled,
                        false,
                        cx,
                    )),
                );
            }
            DialogKind::Input | DialogKind::Editor => {
                let hint = if d.kind == DialogKind::Editor {
                    "⌘Enter to submit"
                } else {
                    "Enter to submit"
                };
                if let Some((input, _)) = self.dialog_inputs.get(&d.id) {
                    card = card.child(Textarea::new(input));
                }
                let (agent2, id2) = (agent.clone(), d.id.clone());
                card = card.child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(hint),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(btn("Cancel", &d.id, DialogAnswer::Cancelled, false, cx))
                                .child(
                                    Button::new(SharedString::from(format!("dlg-{}-ok", d.id)))
                                        .small()
                                        .primary()
                                        .label("Submit")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let value = this
                                                .dialog_inputs
                                                .get(&id2)
                                                .map(|(i, _)| i.read(cx).value().to_string())
                                                .unwrap_or_default();
                                            this.answer(
                                                &agent2,
                                                &id2,
                                                DialogAnswer::Value(value),
                                                cx,
                                            )
                                        })),
                                ),
                        ),
                );
            }
        }
        card.into_any_element()
    }

    /// Revert one file after asking: it overwrites the file, so it also drops later edits
    /// (yours, or another session's — named in the question).
    fn confirm_revert(
        &mut self,
        agent: Entity<Agent>,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.workspace.read(cx).root().to_path_buf();
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let others: Vec<String> = self
            .sessions
            .read(cx)
            .all()
            .iter()
            .filter(|a| **a != agent)
            .filter(|a| {
                a.read(cx)
                    .changes
                    .iter()
                    .any(|c| c.path == path && !c.reverted)
            })
            .map(|a| a.read(cx).name.to_string())
            .collect();
        let created = agent
            .read(cx)
            .changes
            .iter()
            .any(|c| c.path == path && c.before.is_none());
        let mut text = if created {
            format!("pi created {rel} in this session. Reverting deletes it.")
        } else {
            format!(
                "This puts {rel} back to how it was before pi first touched it in this session. Any later edits to it are lost, including yours."
            )
        };
        if !others.is_empty() {
            text.push_str(&format!(
                "\n\nIt was also changed by: {}. Their changes to this file are undone too.",
                others.join(", ")
            ));
        }
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (agent, path) = (agent.clone(), path.clone());
            alert
                .title(format!(
                    "Revert {}?",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                ))
                .description(text.clone())
                .confirm()
                .ok_text("Revert")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let task = agent.update(cx, |a, cx| a.revert(&path, cx));
                    window
                        .spawn(cx, async move |cx| {
                            if let Err(e) = task.await {
                                let _ = cx.update(|window, cx| {
                                    window.push_notification(
                                        Notification::error(format!("Revert failed: {e:#}")),
                                        cx,
                                    )
                                });
                            }
                        })
                        .detach();
                    true
                })
        });
    }

    /// Revert every file this session changed (after asking).
    fn confirm_revert_all(
        &mut self,
        agent: Entity<Agent>,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let n = paths.len();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (agent, paths) = (agent.clone(), paths.clone());
            alert
                .title(format!("Revert all {n} file{}?", if n == 1 { "" } else { "s" }))
                .description("Every file goes back to how it was before pi first touched it in this session. Later edits to them are lost, including yours.")
                .confirm()
                .ok_text("Revert all")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let tasks: Vec<_> = paths.iter().map(|p| agent.update(cx, |a, cx| a.revert(p, cx))).collect();
                    window
                        .spawn(cx, async move |cx| {
                            let mut failed = Vec::new();
                            for t in tasks {
                                if let Err(e) = t.await {
                                    failed.push(format!("{e:#}"));
                                }
                            }
                            if !failed.is_empty() {
                                let _ = cx.update(|window, cx| {
                                    window.push_notification(Notification::error(format!("Some files weren't reverted: {}", failed.join("; "))), cx)
                                });
                            }
                        })
                        .detach();
                    true
                })
        });
    }

    fn render_changes(&self, agent: &Entity<Agent>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let root = self.workspace.read(cx).root().to_path_buf();
        let label: SharedString = agent.read(cx).name.clone();
        let changes = agent.read(cx).changes.clone();
        if changes.is_empty() {
            return div()
                .p_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("pi hasn't changed any files in this session yet. Files it edits or writes show up here.")
                .into_any_element();
        }
        // Files other open sessions also changed.
        let shared: Vec<PathBuf> = self
            .sessions
            .read(cx)
            .all()
            .iter()
            .filter(|a| *a != agent)
            .flat_map(|a| {
                a.read(cx)
                    .changes
                    .iter()
                    .filter(|c| !c.reverted)
                    .map(|c| c.path.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        let rows: Vec<AnyElement> = changes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let rel = c
                    .path
                    .strip_prefix(&root)
                    .unwrap_or(&c.path)
                    .display()
                    .to_string();
                let badge = c.badge();
                let badge_color = match badge {
                    "A" => theme.success,
                    "D" => theme.danger,
                    _ => theme.warning,
                };
                let flagged = shared.contains(&c.path);
                let (diff_path, diff_before, diff_label) =
                    (c.path.clone(), c.before.clone(), label.clone());
                let (agent_r, path_r) = (agent.clone(), c.path.clone());
                let (agent_v, path_v, reviewed) = (agent.clone(), c.path.clone(), c.reviewed);
                v_flex()
                    .id(("change", i))
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.accent))
                    .on_click(cx.listener(move |_, _, window, cx| {
                        pide_core::opener::open_diff(
                            pide_core::opener::DiffRequest {
                                path: diff_path.clone(),
                                before: diff_before.clone(),
                                after: None,
                                label: diff_label.clone(),
                                before_is: "when pi first touched it".into(),
                            },
                            window,
                            cx,
                        );
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .w(px(12.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(badge_color)
                                    .child(badge),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .when(c.reverted, |this| {
                                        this.line_through().text_color(theme.muted_foreground)
                                    })
                                    .when(!c.reviewed && !c.reverted, |this| {
                                        this.font_weight(FontWeight::SEMIBOLD)
                                    })
                                    .child(rel),
                            )
                            .when_some(c.stats, |this, (a, d)| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.success)
                                        .child(format!("+{a}")),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.danger)
                                        .child(format!("−{d}")),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .pl(px(20.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(div().flex_1().child(if c.reverted {
                                "reverted".into()
                            } else {
                                c.tools_label()
                            }))
                            .when(flagged, |this| {
                                this.child(
                                    div()
                                        .text_color(theme.warning)
                                        .child("also changed by another session"),
                                )
                            })
                            .when(!c.reverted, |this| {
                                this.child(
                                    Button::new(("reviewed", i))
                                        .xsmall()
                                        .ghost()
                                        .label(if reviewed {
                                            "Reviewed ✓"
                                        } else {
                                            "Mark reviewed"
                                        })
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.stop_propagation();
                                            agent_v.update(cx, |a, cx| {
                                                a.set_reviewed(&path_v, !reviewed, cx)
                                            });
                                        })),
                                )
                                .child(
                                    Button::new(("revert", i))
                                        .xsmall()
                                        .danger()
                                        .label("Revert")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.confirm_revert(
                                                agent_r.clone(),
                                                path_r.clone(),
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        let open: Vec<PathBuf> = changes
            .iter()
            .filter(|c| !c.reverted)
            .map(|c| c.path.clone())
            .collect();
        let unreviewed = changes
            .iter()
            .filter(|c| !c.reviewed && !c.reverted)
            .count();
        let (agent_all, agent_rev) = (agent.clone(), agent.clone());
        let header = h_flex()
            .gap_2()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().child(format!(
                "{} file{} · {} to review",
                changes.len(),
                if changes.len() == 1 { "" } else { "s" },
                unreviewed
            )))
            .when(unreviewed > 0, |this| {
                this.child(
                    Button::new("review-all")
                        .xsmall()
                        .label("Mark all reviewed")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let paths: Vec<PathBuf> = agent_all
                                .read(cx)
                                .changes
                                .iter()
                                .map(|c| c.path.clone())
                                .collect();
                            agent_all.update(cx, |a, cx| {
                                for p in &paths {
                                    a.set_reviewed(p, true, cx);
                                }
                            });
                        })),
                )
            })
            .when(!open.is_empty(), |this| {
                this.child(
                    Button::new("revert-all")
                        .xsmall()
                        .danger()
                        .label("Revert all…")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_revert_all(agent_rev.clone(), open.clone(), window, cx)
                        })),
                )
            });
        v_flex()
            .id("changes-list")
            .size_full()
            .overflow_y_scroll()
            .text_sm()
            .child(header)
            .children(rows)
            .into_any_element()
    }

    // ---------- rendering ----------

    fn dot(agent: &Agent, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let base = div().size(px(8.)).rounded_full().flex_none();
        match agent.status() {
            AgentStatus::Crashed { .. } => base.bg(theme.danger).into_any_element(),
            _ if agent.waiting => base.bg(theme.warning).into_any_element(),
            AgentStatus::Busy | AgentStatus::Starting | AgentStatus::Aborting => {
                base.bg(theme.primary).into_any_element()
            }
            _ if agent.unread => div()
                .flex_none()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.success)
                .child("✓")
                .into_any_element(),
            _ => base
                .border_1()
                .border_color(theme.muted_foreground)
                .into_any_element(),
        }
    }

    fn status_text(agent: &Agent) -> &'static str {
        match agent.status() {
            AgentStatus::Idle if agent.waiting => "Waiting for your answer",
            AgentStatus::Idle => "Idle",
            AgentStatus::Starting => "Starting pi…",
            AgentStatus::Busy => "pi is working…",
            AgentStatus::Aborting => "Stopping…",
            AgentStatus::Crashed { .. } => "pi crashed",
        }
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sessions = self.sessions.read(cx);
        let active = sessions.active_index();
        let theme = cx.theme().clone();
        let tabs: Vec<AnyElement> = sessions
            .all()
            .iter()
            .enumerate()
            .map(|(i, agent)| {
                let a = agent.read(cx);
                let selected = i == active;
                let agent_close = agent.clone();
                h_flex()
                    .id(("session-tab", i))
                    .flex_none()
                    .gap_1p5()
                    .items_center()
                    .px_2p5()
                    .py_1p5()
                    .max_w(px(180.))
                    .border_r_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.bg(theme.background)
                            .border_t_2()
                            .border_color(theme.primary)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sessions.update(cx, |s, cx| s.set_active(i, cx));
                    }))
                    .child(Self::dot(a, cx))
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .when(selected || a.unread, |this| {
                                this.font_weight(FontWeight::SEMIBOLD)
                            })
                            .child(a.name.clone()),
                    )
                    .child(
                        div()
                            .id(("session-close", i))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .px_0p5()
                            .child("✕")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.sessions.update(cx, |s, cx| s.close(&agent_close, cx));
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        h_flex()
            .w_full()
            .text_sm()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .id("session-tabs")
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .child(
                Button::new("new-session")
                    .ghost()
                    .small()
                    .label("+")
                    .tooltip("New session")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sessions.update(cx, |s, cx| {
                            s.add(None, None, cx);
                        })
                    })),
            )
            .child(
                Button::new("session-list")
                    .ghost()
                    .small()
                    .label(if self.show_list { "▴" } else { "▾" })
                    .tooltip("All sessions")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_list = !this.show_list;
                        if this.show_list {
                            this.refresh_saved(cx);
                        }
                        cx.notify();
                    })),
            )
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let sessions = self.sessions.read(cx);
        let active = sessions.active_index();
        let open_files: Vec<PathBuf> = sessions.open_files(cx);
        let open: Vec<AnyElement> = sessions
            .all()
            .iter()
            .enumerate()
            .map(|(i, agent)| {
                let a = agent.read(cx);
                let label = match a.status() {
                    AgentStatus::Busy | AgentStatus::Starting | AgentStatus::Aborting => "working",
                    AgentStatus::Crashed { .. } => "crashed",
                    _ if a.waiting => "needs you",
                    _ if a.unread => "done",
                    _ => "",
                };
                h_flex()
                    .id(("open-session", i))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .when(i == active, |this| this.bg(theme.accent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sessions.update(cx, |s, cx| s.set_active(i, cx));
                    }))
                    .child(Self::dot(a, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .when(i == active || a.unread, |this| {
                                this.font_weight(FontWeight::SEMIBOLD)
                            })
                            .child(a.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                    .into_any_element()
            })
            .collect();
        let saved: Vec<AnyElement> = self
            .saved
            .iter()
            .filter(|s| !open_files.contains(&s.path))
            .take(50)
            .enumerate()
            .map(|(i, s)| {
                let path = s.path.clone();
                let name = s.name.clone();
                let title = s
                    .name
                    .clone()
                    .unwrap_or_else(|| s.preview.chars().take(60).collect());
                let ago = s.modified.elapsed().map(fmt_ago).unwrap_or_default();
                v_flex()
                    .id(("saved-session", i))
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.accent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (path, name) = (path.clone(), name.clone());
                        this.sessions.update(cx, |s, cx| {
                            s.add(Some(path), name, cx);
                        });
                    }))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(ago),
                            ),
                    )
                    .when(s.name.is_some(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(s.preview.clone()),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let heading = |t: &'static str| {
            div()
                .px_3()
                .pt_3()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(t)
        };
        v_flex()
            .id("session-list")
            .size_full()
            .overflow_y_scroll()
            .text_sm()
            .child(heading("OPEN"))
            .children(open)
            .child(heading("SAVED IN THIS PROJECT"))
            .when(saved.is_empty(), |this| {
                this.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("None yet."),
                )
            })
            .children(saved)
    }

    fn render_item(&self, view: &SessionView, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match &view.items[ix] {
            Item::User(chip, text) => v_flex()
                .gap_1()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .bg(theme.secondary)
                .when_some(chip.clone(), |this, c| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .font_family(theme.mono_font_family.clone())
                            .child(format!("⌁ {c}")),
                    )
                })
                .child(text.clone())
                .into_any_element(),
            Item::Assistant {
                markdown,
                thinking,
                thinking_open,
                error,
                ..
            } => v_flex()
                .gap_1()
                .when(!thinking.is_empty(), |this| {
                    let open = *thinking_open;
                    let agent_id = self.active(cx).entity_id();
                    this.child(
                        div()
                            .id(("thinking", ix))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(Item::Assistant { thinking_open, .. }) = this
                                    .views
                                    .get_mut(&agent_id)
                                    .and_then(|v| v.items.get_mut(ix))
                                {
                                    *thinking_open = !*thinking_open;
                                    cx.notify();
                                }
                            }))
                            .child(if open {
                                format!("▾ Thinking\n{}", thinking.trim())
                            } else {
                                format!(
                                    "▸ Thinking ({} words)",
                                    thinking.split_whitespace().count()
                                )
                            }),
                    )
                })
                .child(TextView::new(markdown).selectable(true))
                .when_some(error.clone(), |this, e| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(theme.danger)
                            .child(format!("Ended: {e}")),
                    )
                })
                .into_any_element(),
            Item::Tool {
                name,
                summary,
                state,
                output,
                duration_ms,
                ..
            } => {
                let (mark, color) = match state {
                    ToolState::Running => ("…", theme.muted_foreground),
                    ToolState::Done => ("✓", theme.success),
                    ToolState::Failed => ("✗", theme.danger),
                };
                v_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_color(color).child(mark))
                            .child(div().font_weight(FontWeight::BOLD).child(name.clone()))
                            .child({
                                // A file tool's path opens the file.
                                let file =
                                    matches!(name.as_str(), "read" | "write" | "edit").then(|| {
                                        let p = std::path::Path::new(summary.as_str());
                                        if p.is_absolute() {
                                            p.to_path_buf()
                                        } else {
                                            self.workspace.read(cx).root().join(p)
                                        }
                                    });
                                div()
                                    .id(("tool-path", ix))
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .when_some(file, |d, file| {
                                        d.cursor_pointer().hover(|s| s.underline()).on_click(
                                            move |_, window, cx| {
                                                if file.is_file() {
                                                    pide_core::opener::open_file(
                                                        file.clone(),
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            },
                                        )
                                    })
                                    .child(summary.clone())
                            })
                            .when_some(*duration_ms, |this, d| {
                                this.child(
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{d} ms")),
                                )
                            }),
                    )
                    .when(!output.is_empty(), |this| {
                        this.child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(output.clone()),
                        )
                    })
                    .into_any_element()
            }
            Item::Notice { text, error } => div()
                .text_sm()
                .text_color(if *error {
                    theme.danger
                } else {
                    theme.muted_foreground
                })
                .child(text.clone())
                .into_any_element(),
        }
    }

    fn render_conversation(&mut self, wide: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let agent = self.active(cx);
        let a = agent.read(cx);
        let status = a.status().clone();
        let busy = a.is_busy();
        let name = a.name.clone();
        let status_text = Self::status_text(a);
        let (steering, follow_ups) = a.queue.clone();
        let dialog = a.dialogs.first().cloned();
        let n_changes = a.changes.len();
        let unreviewed = a.unreviewed();
        let show_changes = self.show_changes;
        let slash_rows = self.suggestions(cx);
        // `/` typed but the commands haven't arrived yet.
        let slash_rows_empty_hint = slash_rows.is_empty()
            && !a.commands_loaded
            && slash::typed(&self.input.read(cx).value()).is_some();
        let model_label = match &a.model {
            Some(m) => model_picker::short_label(m),
            None => "Model".to_string(),
        };
        // Thinking level, for models that reason.
        let thinking_label = match (&a.model, &a.thinking) {
            (Some(m), Some(t)) if m.reasoning => Some(format!("thinking: {t}")),
            _ => None,
        };
        let agent_thinking = agent.clone();
        let stats = a.stats.clone().filter(|s| s.tokens_total > 0);
        let model_tip = a.model.as_ref().map(|m| format!("{}/{}", m.provider, m.id));
        let agent_model = agent.clone();
        let id = agent.entity_id();
        let (items, scroll, empty) = match self.views.get(&id) {
            Some(view) => {
                let els: Vec<AnyElement> = (0..view.items.len())
                    .map(|ix| self.render_item(view, ix, cx))
                    .collect();
                (els, view.scroll.clone(), view.items.is_empty())
            }
            None => (Vec::new(), ScrollHandle::new(), true),
        };
        let theme = cx.theme().clone();
        let agent_close = agent.clone();

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full() // the wide layout's row centers children vertically otherwise
            .min_h_0()
            .overflow_hidden()
            .when(wide, |this| {
                this.child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            v_flex()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(name.clone()))
                                .child(div().text_xs().text_color(theme.muted_foreground).child(status_text)),
                        )
                        .child(Button::new("close-session").small().label("Close").on_click(cx.listener(
                            move |this, _, _, cx| this.sessions.update(cx, |s, cx| s.close(&agent_close, cx)),
                        ))),
                )
            })
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Button::new("view-chat").xsmall().label("Chat").when(!show_changes, |b| b.primary()).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.show_changes = false;
                            cx.notify();
                        }),
                    ))
                    .child(
                        Button::new("view-changes")
                            .xsmall()
                            .label(match (n_changes, unreviewed) {
                                (0, _) => "Changes".to_string(),
                                (_, 0) => format!("Changes · {n_changes}"),
                                (_, u) => format!("Changes · {u} to review"),
                            })
                            .when(show_changes, |b| b.primary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_changes = true;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .when_some(stats, |this, st| {
                        let cost = if st.cost < 0.01 { format!("${:.3}", st.cost) } else { format!("${:.2}", st.cost) };
                        let label = match st.context_percent {
                            Some(p) => format!("{cost} · {}% ctx", p.round() as i64),
                            None => cost,
                        };
                        let k = |n: u64| if n >= 1000 { format!("{:.1}k", n as f64 / 1000.0) } else { n.to_string() };
                        let tip = format!(
                            "Tokens: {} in, {} out, {} cache read ({} total){}",
                            k(st.tokens_input),
                            k(st.tokens_output),
                            k(st.tokens_cache_read),
                            k(st.tokens_total),
                            match (st.context_tokens, st.context_window) {
                                (Some(t), Some(w)) => format!(" · context {} of {}", k(t), k(w)),
                                _ => String::new(),
                            }
                        );
                        this.child(Button::new("session-stats").xsmall().ghost().label(label).tooltip(tip))
                    })
                    .when_some(thinking_label, |this, label| {
                        this.child(
                            Button::new("pick-thinking")
                                .xsmall()
                                .ghost()
                                .label(label)
                                .tooltip("Thinking level for this session")
                                .on_click(move |_, window, cx| {
                                    let agent = agent_thinking.clone();
                                    agent.update(cx, |a, cx| a.request_thinking_levels(cx));
                                    model_picker::open_thinking(agent, window, cx);
                                }),
                        )
                    })
                    .child(
                        Button::new("pick-model")
                            .xsmall()
                            .ghost()
                            .label(model_label)
                            .when_some(model_tip, |b, t| b.tooltip(t))
                            .on_click(move |_, window, cx| {
                                let agent = agent_model.clone();
                                agent.update(cx, |a, cx| a.request_models(cx));
                                model_picker::open(agent, window, cx);
                            }),
                    ),
            )
            .when(show_changes, |this| this.child(div().flex_1().min_h_0().child(self.render_changes(&agent, cx))))
            .when(!show_changes, |this| this.child(
                div().id(("chat-scroll", id.as_u64())).flex_1().min_h_0().overflow_y_scroll().track_scroll(&scroll).child(
                    v_flex()
                        .p_3()
                        .gap_3()
                        .text_sm()
                        .when(empty, |this| {
                            this.child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child("Ask pi about this project. It starts on your first message."),
                            )
                        })
                        .children(items),
                ),
            ))
            .when_some(dialog.map(|d| self.render_dialog(&agent, &d, cx)), |this, card| this.child(card))
            .when_some(
                match &status {
                    AgentStatus::Crashed { message, stderr_tail } => Some((message.clone(), stderr_tail.clone())),
                    _ => None,
                },
                |this, (message, tail)| {
                    this.child(
                        v_flex()
                            .gap_1()
                            .p_2()
                            .m_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.danger)
                            .text_xs()
                            .child(div().text_color(theme.danger).child(message))
                            .when(!tail.is_empty(), |this| {
                                this.child(
                                    div()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(theme.muted_foreground)
                                        .child(tail.join("\n")),
                                )
                            })
                            .child(h_flex().child(Button::new("restart").small().primary().label("Restart pi").on_click(
                                cx.listener(|this, _, _, cx| this.active(cx).update(cx, |a, cx| a.restart(cx))),
                            ))),
                    )
                },
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .children(steering.into_iter().map(|t| queued_row("steer", t, &theme)))
                    .children(follow_ups.into_iter().map(|t| queued_row("after this run", t, &theme)))
                    .when(busy, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{status_text}  Enter steers · ⌥Enter queues a follow-up")),
                        )
                    })
                    .when(!slash_rows.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .border_1()
                                .border_color(theme.border)
                                .rounded(theme.radius)
                                .py_1()
                                .text_sm()
                                .children(slash_rows.into_iter().enumerate().map(|(i, c)| {
                                    let pick = c.clone();
                                    h_flex()
                                        .id(("slash", i))
                                        .gap_2()
                                        .px_2()
                                        .py_0p5()
                                        .cursor_pointer()
                                        .when(i == 0, |r| r.bg(theme.list_hover))
                                        .hover(|r| r.bg(theme.list_hover))
                                        .on_click(cx.listener(move |this, _, window, cx| this.complete(&pick, window, cx)))
                                        .child(div().flex_none().font_family(theme.mono_font_family.clone()).child(c.label.clone()))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child(c.detail.clone()),
                                        )
                                        .child(div().flex_1())
                                        .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(c.tag.clone()))
                                })),
                        )
                    })
                    .when(slash_rows_empty_hint, |this| {
                        this.child(div().text_xs().text_color(theme.muted_foreground).child("Loading pi's commands…"))
                    })
                    .when(!self.attachments.is_empty(), |this| {
                        this.child(h_flex().gap_2().flex_wrap().children(self.attachments.iter().enumerate().map(|(i, (_, img_data))| {
                            h_flex()
                                .gap_1()
                                .p_1()
                                .border_1()
                                .border_color(theme.border)
                                .rounded(theme.radius)
                                .child(img(img_data.clone()).h(px(40.)).max_w(px(120.)).object_fit(ObjectFit::Contain))
                                .child(Button::new(("remove-image", i)).xsmall().ghost().label("×").tooltip("Remove").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        if i < this.attachments.len() {
                                            this.attachments.remove(i);
                                        }
                                        cx.notify();
                                    }),
                                ))
                        })))
                    })
                    .child(
                        div()
                            .key_context("AgentInput")
                            // An image on the clipboard becomes an attachment; text pastes as usual.
                            .capture_action(cx.listener(|this, _: &Paste, _, cx| {
                                if this.paste_image(cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .on_action(|_: &BackToEditor, window, cx| {
                                CommandRegistry::run("editor.focus", window, cx);
                            })
                            .on_action(cx.listener(|this, _: &CompleteSlash, window, cx| {
                                match this.suggestions(cx).into_iter().next() {
                                    Some(first) => this.complete(&first, window, cx),
                                    None => window.focus_next(cx),
                                }
                            }))
                            .on_action(cx.listener(|this, _: &QueueFollowUp, window, cx| this.send(true, window, cx)))
                            .child(Textarea::new(&self.input)),
                    )
                    .child(h_flex().justify_between().gap_2().child({
                        let on = self.send_context(cx);
                        let label = match (on, self.context_now.clone()) {
                            (false, _) => "No editor context".to_string(),
                            // `main.rs:12` for a cursor, `main.rs 5–7` for a selection.
                            (true, Some(c)) => format!("⌁ Context: {}", c.label()),
                            (true, None) => "⌁ Context: no file open".to_string(),
                        };
                        Button::new("context-toggle")
                            .xsmall()
                            .label(label)
                            .when(on, |b| b.primary())
                            .tooltip("Send the active file and selection with new prompts")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_context(cx)))
                    }).when(busy, |this| {
                        this.child(Button::new("stop").small().danger().label("Stop").on_click(
                            cx.listener(|this, _, _, cx| this.active(cx).update(cx, |a, cx| a.abort(cx))),
                        ))
                    })),
            )
    }
}

fn queued_row(
    kind: &'static str,
    text: String,
    theme: &gpui_kit::component::theme::Theme,
) -> AnyElement {
    h_flex()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .border_1()
        .border_dashed()
        .border_color(theme.border)
        .text_xs()
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.warning)
                .child(kind),
        )
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(text),
        )
        .into_any_element()
}

fn tool_mut<'a>(items: &'a mut [Item], id: &str) -> Option<&'a mut Item> {
    items
        .iter_mut()
        .rev()
        .find(|i| matches!(i, Item::Tool { id: tid, .. } if tid == id))
}

fn fmt_ago(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86400),
    }
}

impl Focusable for AgentChat {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl gpui_kit::component::dock::BasePanel for AgentChat {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn dump(&self, cx: &App) -> PanelState {
        let sessions = self.sessions.read(cx);
        let open: Vec<Value> = sessions
            .all()
            .iter()
            .filter_map(|a| {
                let a = a.read(cx);
                a.session_file
                    .as_ref()
                    .map(|f| json!({ "file": f, "name": a.name.to_string() }))
            })
            .collect();
        let mut state = PanelState::new(PANEL_NAME);
        state.info =
            PanelInfo::panel(json!({ "sessions": open, "active": sessions.active_index() }));
        state
    }
}

impl gpui_kit::component::dock::Panel for AgentChat {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for AgentChat {
    fn title_text(&self, _: &App) -> SharedString {
        "pi".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Right
    }
}

impl Render for AgentChat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.context_now = pide_core::context::current(window, cx);
        let wide = self.width.get() >= SIDEBAR_MIN_WIDTH;
        let width = self.width.clone();
        let theme = cx.theme().clone();

        let body: AnyElement = if wide {
            h_flex()
                .size_full()
                .child(
                    div()
                        .w(px(260.))
                        .flex_none()
                        .h_full()
                        .bg(theme.secondary)
                        .border_r_1()
                        .border_color(theme.border)
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(theme.border)
                                .child(div().font_weight(FontWeight::SEMIBOLD).child("Sessions"))
                                .child(
                                    Button::new("new-session-wide")
                                        .small()
                                        .primary()
                                        .label("New")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sessions.update(cx, |s, cx| {
                                                s.add(None, None, cx);
                                            })
                                        })),
                                ),
                        )
                        .child(self.render_list(cx)),
                )
                .child(self.render_conversation(true, cx))
                .into_any_element()
        } else {
            v_flex()
                .size_full()
                .overflow_hidden()
                .child(self.render_tabs(cx))
                .child(if self.show_list {
                    self.render_list(cx).into_any_element()
                } else {
                    self.render_conversation(false, cx).into_any_element()
                })
                .into_any_element()
        };

        div()
            .relative()
            .size_full()
            .track_focus(&self.focus)
            // Measure our own width each frame: tabs below SIDEBAR_MIN_WIDTH, sidebar above.
            .child(
                canvas(
                    move |bounds, window, _| {
                        let w = f32::from(bounds.size.width);
                        if (w - width.get()).abs() > 1. {
                            let crossed =
                                (w >= SIDEBAR_MIN_WIDTH) != (width.get() >= SIDEBAR_MIN_WIDTH);
                            width.set(w);
                            if crossed {
                                window.refresh();
                            }
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn history_follows_the_current_branch() {
        let e = |id: &str, parent: Option<&str>| serde_json::json!({ "type": "message", "id": id, "parentId": parent });
        // a → b → c, then the user went back to b and continued with d → e.
        let entries = vec![
            serde_json::json!({ "type": "session" }),
            e("a", None),
            e("b", Some("a")),
            e("c", Some("b")),
            e("d", Some("b")),
            e("e", Some("d")),
        ];
        let ids: Vec<String> = current_branch(entries)
            .iter()
            .map(|v| {
                v.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("-")
                    .to_string()
            })
            .collect();
        assert_eq!(ids, ["-", "a", "b", "d", "e"]);
        // No ids at all: unchanged.
        let plain = vec![serde_json::json!({ "x": 1 }), serde_json::json!({ "x": 2 })];
        assert_eq!(current_branch(plain.clone()), plain);
    }
}
