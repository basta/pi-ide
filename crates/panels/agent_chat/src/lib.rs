//! The pi panel: several sessions (tabs when narrow, a sidebar when wide),
//! each with its own conversation; send, stream, tool calls, stop, crash
//! recovery. Talks only to `Sessions` / `Agent` from core.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelInfo, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::sessions::{SavedSession, list_saved, user_text};
use pide_core::{
    Agent, AgentEvent, AgentStatus, CommandRegistry, CommandSpec, DockPosition, Panel, Services, Sessions,
    SessionsEvent,
};
use serde_json::{Value, json};

pub const PANEL_NAME: &str = "agent_chat";
/// At this width or more the panel shows the session sidebar instead of tabs.
const SIDEBAR_MIN_WIDTH: f32 = 640.;

pub fn init(cx: &mut App) {
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
            let sessions = Services::global(cx).sessions;
            sessions.update(cx, |s, cx| s.restore(files, active, cx));
        }
        panel_handle(cx.new(|cx| AgentChat::new(window, cx)))
    });

    let active = |cx: &App| Services::active_agent(cx);
    let sessions = |cx: &App| Services::global(cx).sessions;
    CommandRegistry::register(cx, CommandSpec::new("agent.abort", "Agent: Stop").key("cmd-f2"), move |_, cx| {
        active(cx).update(cx, |a, cx| a.abort(cx))
    });
    CommandRegistry::register(cx, CommandSpec::new("agent.restart", "Agent: Restart pi"), move |_, cx| {
        active(cx).update(cx, |a, cx| a.restart(cx))
    });
    CommandRegistry::register(cx, CommandSpec::new("agent.new_session", "Agent: New Session"), move |_, cx| {
        sessions(cx).update(cx, |s, cx| {
            s.add(None, None, cx);
        })
    });
    CommandRegistry::register(cx, CommandSpec::new("agent.close_session", "Agent: Close Session"), move |_, cx| {
        let a = active(cx);
        sessions(cx).update(cx, |s, cx| s.close(&a, cx))
    });
    CommandRegistry::register(cx, CommandSpec::new("agent.next_session", "Agent: Next Session").key("ctrl-tab"), move |_, cx| {
        sessions(cx).update(cx, |s, cx| s.cycle(1, cx))
    });
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.prev_session", "Agent: Previous Session").key("ctrl-shift-tab"),
        move |_, cx| sessions(cx).update(cx, |s, cx| s.cycle(-1, cx)),
    );
}

#[derive(Clone, Copy, PartialEq)]
enum ToolState {
    Running,
    Done,
    Failed,
}

enum Item {
    User(String),
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
    Tool { id: String, name: String, summary: String, output: String, failed: bool },
}

pub struct AgentChat {
    sessions: Entity<Sessions>,
    views: HashMap<EntityId, SessionView>,
    agent_subs: HashMap<EntityId, Subscription>,
    input: Entity<TextareaState>,
    focus: FocusHandle,
    /// Last laid-out width, measured each frame (tabs vs sidebar).
    width: Rc<Cell<f32>>,
    /// Narrow mode: the ▾ session list is showing instead of the conversation.
    show_list: bool,
    saved: Vec<SavedSession>,
    _load_saved: Option<Task<()>>,
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
    if s.chars().count() > 160 { format!("{}…", s.chars().take(160).collect::<String>()) } else { s }
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
        format!("{}\n… {} more lines", lines[..8].join("\n"), lines.len() - 8)
    } else {
        text.trim_end().to_string()
    }
}

/// Conversation history from a pi session file (`docs/session-format.md`).
/// Branches are flattened in file order — good enough to read back.
fn parse_history(path: &std::path::Path) -> Vec<HistItem> {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else { return Vec::new() };
    let mut out: Vec<HistItem> = Vec::new();
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let m = &v["message"];
        match m.get("role").and_then(Value::as_str) {
            Some("user") => {
                if let Some(t) = user_text(m) {
                    out.push(HistItem::User(t));
                }
            }
            Some("assistant") => {
                for block in m.get("content").and_then(Value::as_array).into_iter().flatten() {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let t = block.get("text").and_then(Value::as_str).unwrap_or_default();
                            if !t.trim().is_empty() {
                                out.push(HistItem::Text(t.to_string()));
                            }
                        }
                        Some("toolCall") => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                            out.push(HistItem::Tool {
                                id: block.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
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
                let id = m.get("toolCallId").and_then(Value::as_str).unwrap_or_default();
                let failed = m.get("isError").and_then(Value::as_bool).unwrap_or(false);
                let text = result_text(m);
                if let Some(HistItem::Tool { output, failed: f, .. }) =
                    out.iter_mut().rev().find(|h| matches!(h, HistItem::Tool { id: tid, .. } if tid == id))
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
        let sessions = Services::global(cx).sessions;
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .submit_on_enter(true)
                .placeholder("Ask pi…  (Enter to send, Shift+Enter for a new line)")
        });
        let subs = vec![
            cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    this.send(window, cx);
                }
            }),
            cx.subscribe_in(&sessions, window, |this, _, ev: &SessionsEvent, window, cx| {
                let SessionsEvent::Changed = ev;
                this.sync_sessions(window, cx);
                this.show_list = false;
                cx.emit(PanelEvent::LayoutChanged); // open sessions are part of the saved layout
            }),
            cx.observe(&sessions, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            sessions,
            views: HashMap::new(),
            agent_subs: HashMap::new(),
            input,
            focus: cx.focus_handle(),
            width: Rc::new(Cell::new(0.)),
            show_list: false,
            saved: Vec::new(),
            _load_saved: None,
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
            if !self.agent_subs.contains_key(&id) {
                let sub = cx.subscribe_in(&agent, window, |this, agent, ev: &AgentEvent, window, cx| {
                    this.on_agent_event(agent.clone(), ev, window, cx)
                });
                self.agent_subs.insert(id, sub);
            }
            let view = self.views.entry(id).or_default();
            if !view.history_requested {
                if let Some(file) = agent.read(cx).session_file.clone() {
                    view.history_requested = true;
                    self.load_history(id, file, cx);
                }
            }
        }
        cx.notify();
    }

    fn load_history(&mut self, id: EntityId, file: PathBuf, cx: &mut Context<Self>) {
        let parse = cx.background_executor().spawn(async move { parse_history(&file) });
        cx.spawn(async move |this, cx| {
            let hist = parse.await;
            let _ = this.update(cx, |this, cx| {
                let mut items: Vec<Item> = hist
                    .into_iter()
                    .map(|h| match h {
                        HistItem::User(t) => Item::User(t),
                        HistItem::Text(t) => Item::Assistant {
                            markdown: cx.new(|cx| TextViewState::markdown(&t, cx)),
                            text: t,
                            last_index: None,
                            thinking: String::new(),
                            thinking_open: false,
                            error: None,
                        },
                        HistItem::Tool { id, name, summary, output, failed } => Item::Tool {
                            id,
                            name,
                            summary,
                            state: if failed { ToolState::Failed } else { ToolState::Done },
                            output,
                            duration_ms: None,
                        },
                    })
                    .collect();
                if let Some(view) = this.views.get_mut(&id) {
                    if !items.is_empty() {
                        items.push(Item::Notice { text: "Resumed from disk. pi continues this session on your next message.".into(), error: false });
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
        let root = Services::global(cx).workspace.read(cx).root().to_path_buf();
        let list = cx.background_executor().spawn(async move { list_saved(&root) });
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

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        let agent = self.active(cx);
        if text.is_empty() || agent.read(cx).is_busy() {
            return;
        }
        self.input.update(cx, |i, cx| i.set_value("", window, cx));
        agent.update(cx, |a, cx| a.prompt(text, cx));
    }

    fn on_agent_event(&mut self, agent: Entity<Agent>, ev: &AgentEvent, window: &mut Window, cx: &mut Context<Self>) {
        let is_active = agent == self.active(cx);
        let name = agent.read(cx).name.clone();
        let Some(view) = self.views.get_mut(&agent.entity_id()) else { return };
        let items = &mut view.items;
        match ev {
            AgentEvent::UserPrompt { text } => items.push(Item::User(text.clone())),
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
                if let Some(Item::Assistant { markdown, text, last_index, .. }) = items.last_mut() {
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
                if let Some(Item::Assistant { markdown, text, last_index, .. }) = items.last_mut() {
                    if last_index.is_some() && !text.ends_with(content.as_str()) && !content.is_empty() {
                        *text = content.clone();
                        markdown.update(cx, |m, cx| m.set_text(content, cx));
                    }
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
            AgentEvent::ToolEnd { id, result, is_error, duration_ms, .. } => {
                if let Some(Item::Tool { state, output, duration_ms: d, .. }) = tool_mut(items, id) {
                    *state = if *is_error { ToolState::Failed } else { ToolState::Done };
                    *output = result_text(result);
                    *d = *duration_ms;
                }
            }
            AgentEvent::Settled { aborted } => {
                if *aborted {
                    items.push(Item::Notice { text: "Stopped.".into(), error: false });
                }
                if !is_active {
                    window.push_notification(Notification::info(format!("pi finished in \u{201c}{name}\u{201d}.")), cx);
                }
            }
            AgentEvent::Error(e) => items.push(Item::Notice { text: e.clone(), error: true }),
            AgentEvent::Notify { message, error } => {
                let n = if *error { Notification::error(message.clone()) } else { Notification::info(message.clone()) };
                window.push_notification(n, cx);
            }
            AgentEvent::Unknown(v) => log::debug!("unknown agent record: {v}"),
            _ => {}
        }
        view.scroll.scroll_to_bottom();
        cx.notify();
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
            _ => base.border_1().border_color(theme.muted_foreground).into_any_element(),
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
                    .when(selected, |this| this.bg(theme.background).border_t_2().border_color(theme.primary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sessions.update(cx, |s, cx| s.set_active(i, cx));
                    }))
                    .child(Self::dot(a, cx))
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .when(selected || a.unread, |this| this.font_weight(FontWeight::SEMIBOLD))
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
            .child(h_flex().id("session-tabs").flex_1().min_w_0().overflow_x_scroll().children(tabs))
            .child(
                Button::new("new-session").ghost().small().label("+").tooltip("New session").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.sessions.update(cx, |s, cx| {
                            s.add(None, None, cx);
                        })
                    },
                )),
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
                            .when(i == active || a.unread, |this| this.font_weight(FontWeight::SEMIBOLD))
                            .child(a.name.clone()),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(label))
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
                let title = s.name.clone().unwrap_or_else(|| s.preview.chars().take(60).collect());
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
                            .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
                            .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(ago)),
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
            div().px_3().pt_3().pb_1().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child(t)
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
                this.child(div().px_3().py_1().text_xs().text_color(theme.muted_foreground).child("None yet."))
            })
            .children(saved)
    }

    fn render_item(&self, view: &SessionView, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match &view.items[ix] {
            Item::User(text) => {
                div().px_3().py_2().rounded(theme.radius).bg(theme.secondary).child(text.clone()).into_any_element()
            }
            Item::Assistant { markdown, thinking, thinking_open, error, .. } => v_flex()
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
                                if let Some(Item::Assistant { thinking_open, .. }) =
                                    this.views.get_mut(&agent_id).and_then(|v| v.items.get_mut(ix))
                                {
                                    *thinking_open = !*thinking_open;
                                    cx.notify();
                                }
                            }))
                            .child(if open {
                                format!("▾ Thinking\n{}", thinking.trim())
                            } else {
                                format!("▸ Thinking ({} words)", thinking.split_whitespace().count())
                            }),
                    )
                })
                .child(TextView::new(markdown).selectable(true))
                .when_some(error.clone(), |this, e| {
                    this.child(div().text_sm().text_color(theme.danger).child(format!("Ended: {e}")))
                })
                .into_any_element(),
            Item::Tool { name, summary, state, output, duration_ms, .. } => {
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
                            .child(div().flex_1().overflow_hidden().text_ellipsis().child(summary.clone()))
                            .when_some(*duration_ms, |this, d| {
                                this.child(div().text_color(theme.muted_foreground).child(format!("{d} ms")))
                            }),
                    )
                    .when(!output.is_empty(), |this| {
                        this.child(div().text_color(theme.muted_foreground).child(output.clone()))
                    })
                    .into_any_element()
            }
            Item::Notice { text, error } => div()
                .text_sm()
                .text_color(if *error { theme.danger } else { theme.muted_foreground })
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
        let id = agent.entity_id();
        let (items, scroll, empty) = match self.views.get(&id) {
            Some(view) => {
                let els: Vec<AnyElement> = (0..view.items.len()).map(|ix| self.render_item(view, ix, cx)).collect();
                (els, view.scroll.clone(), view.items.is_empty())
            }
            None => (Vec::new(), ScrollHandle::new(), true),
        };
        let theme = cx.theme().clone();
        let agent_close = agent.clone();

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
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
                div().id(("chat-scroll", id.as_u64())).flex_1().overflow_y_scroll().track_scroll(&scroll).child(
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
            )
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
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .when(busy, |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(status_text)))
                    .child(Textarea::new(&self.input))
                    .child(h_flex().justify_end().gap_2().when(busy, |this| {
                        this.child(Button::new("stop").small().danger().label("Stop").on_click(
                            cx.listener(|this, _, _, cx| this.active(cx).update(cx, |a, cx| a.abort(cx))),
                        ))
                    })),
            )
    }
}

fn tool_mut<'a>(items: &'a mut [Item], id: &str) -> Option<&'a mut Item> {
    items.iter_mut().rev().find(|i| matches!(i, Item::Tool { id: tid, .. } if tid == id))
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
                a.session_file.as_ref().map(|f| json!({ "file": f, "name": a.name.to_string() }))
            })
            .collect();
        let mut state = PanelState::new(PANEL_NAME);
        state.info = PanelInfo::panel(json!({ "sessions": open, "active": sessions.active_index() }));
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                                .child(Button::new("new-session-wide").small().primary().label("New").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.sessions.update(cx, |s, cx| {
                                            s.add(None, None, cx);
                                        })
                                    }),
                                )),
                        )
                        .child(self.render_list(cx)),
                )
                .child(self.render_conversation(true, cx))
                .into_any_element()
        } else {
            v_flex()
                .size_full()
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
                            let crossed = (w >= SIDEBAR_MIN_WIDTH) != (width.get() >= SIDEBAR_MIN_WIDTH);
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
