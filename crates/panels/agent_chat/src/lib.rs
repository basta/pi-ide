//! The pi chat panel: send a prompt, stream the reply, show tool calls inline,
//! abort, and recover from a crash. Talks only to `Entity<Agent>`.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{Agent, AgentEvent, AgentStatus, CommandRegistry, CommandSpec, DockPosition, Panel, Services};
use serde_json::Value;

pub const PANEL_NAME: &str = "agent_chat";

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| panel_handle(cx.new(|cx| AgentChat::new(window, cx))));

    let agent = |cx: &App| Services::global(cx).agent;
    CommandRegistry::register(cx, CommandSpec::new("agent.abort", "Agent: Stop").key("cmd-f2"), move |_, cx| {
        agent(cx).update(cx, |a, cx| a.abort(cx))
    });
    CommandRegistry::register(cx, CommandSpec::new("agent.restart", "Agent: Restart pi"), move |_, cx| {
        agent(cx).update(cx, |a, cx| a.restart(cx))
    });
    CommandRegistry::register(cx, CommandSpec::new("agent.new_session", "Agent: New Session"), move |_, cx| {
        agent(cx).update(cx, |a, cx| a.new_session(cx))
    });
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

pub struct AgentChat {
    agent: Entity<Agent>,
    input: Entity<TextareaState>,
    items: Vec<Item>,
    scroll: ScrollHandle,
    focus: FocusHandle,
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

impl AgentChat {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let agent = Services::global(cx).agent;
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
            cx.subscribe_in(&agent, window, Self::on_agent_event),
            cx.observe(&agent, |_, _, cx| cx.notify()),
        ];
        Self { agent, input, items: Vec::new(), scroll: ScrollHandle::new(), focus: cx.focus_handle(), _subs: subs }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() || self.agent.read(cx).is_busy() {
            return;
        }
        self.input.update(cx, |i, cx| i.set_value("", window, cx));
        self.agent.update(cx, |a, cx| a.prompt(text, cx));
    }

    fn stop(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.agent.update(cx, |a, cx| a.abort(cx));
    }

    fn restart(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.agent.update(cx, |a, cx| a.restart(cx));
    }

    fn current_assistant(&mut self) -> Option<&mut Item> {
        match self.items.last_mut() {
            Some(item @ Item::Assistant { .. }) => Some(item),
            _ => None,
        }
    }

    fn on_agent_event(&mut self, _: &Entity<Agent>, ev: &AgentEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            AgentEvent::UserPrompt { text } => self.items.push(Item::User(text.clone())),
            AgentEvent::AssistantStart => {
                let markdown = cx.new(|cx| TextViewState::markdown("", cx));
                self.items.push(Item::Assistant {
                    markdown,
                    text: String::new(),
                    last_index: None,
                    thinking: String::new(),
                    thinking_open: false,
                    error: None,
                });
            }
            AgentEvent::TextDelta { index, delta } => {
                if let Some(Item::Assistant { markdown, text, last_index, .. }) = self.current_assistant() {
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
                // Authoritative content: fix up if streaming drifted (single-block messages).
                if let Some(Item::Assistant { markdown, text, last_index, .. }) = self.current_assistant() {
                    if last_index.is_some() && !text.ends_with(content.as_str()) && !content.is_empty() {
                        *text = content.clone();
                        markdown.update(cx, |m, cx| m.set_text(content, cx));
                    }
                }
            }
            AgentEvent::ThinkingDelta { delta, .. } => {
                if let Some(Item::Assistant { thinking, .. }) = self.current_assistant() {
                    thinking.push_str(delta);
                }
            }
            AgentEvent::AssistantEnd { error } => {
                if let Some(Item::Assistant { error: e, .. }) = self.current_assistant() {
                    *e = error.clone();
                }
            }
            AgentEvent::ToolStart { id, name, args } => self.items.push(Item::Tool {
                id: id.clone(),
                name: name.clone(),
                summary: summarize_args(name, args),
                state: ToolState::Running,
                output: String::new(),
                duration_ms: None,
            }),
            AgentEvent::ToolUpdate { id, partial } => {
                if let Some(Item::Tool { output, .. }) = self.tool_mut(id) {
                    *output = result_text(partial);
                }
            }
            AgentEvent::ToolEnd { id, result, is_error, duration_ms, .. } => {
                if let Some(Item::Tool { state, output, duration_ms: d, .. }) = self.tool_mut(id) {
                    *state = if *is_error { ToolState::Failed } else { ToolState::Done };
                    *output = result_text(result);
                    *d = *duration_ms;
                }
            }
            AgentEvent::Settled { aborted: true } => {
                self.items.push(Item::Notice { text: "Stopped.".into(), error: false })
            }
            AgentEvent::Error(e) => self.items.push(Item::Notice { text: e.clone(), error: true }),
            AgentEvent::Notify { message, error } => {
                let n = if *error { Notification::error(message.clone()) } else { Notification::info(message.clone()) };
                window.push_notification(n, cx);
            }
            AgentEvent::Unknown(v) => {
                log::debug!("unknown agent record: {v}");
            }
            _ => {}
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut Item> {
        self.items.iter_mut().rev().find(|i| matches!(i, Item::Tool { id: tid, .. } if tid == id))
    }

    fn render_item(&self, ix: usize, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match item {
            Item::User(text) => div()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .bg(theme.secondary)
                .child(text.clone())
                .into_any_element(),
            Item::Assistant { markdown, thinking, thinking_open, error, .. } => v_flex()
                .gap_1()
                .when(!thinking.is_empty(), |this| {
                    let open = *thinking_open;
                    this.child(
                        div()
                            .id(("thinking", ix))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(Item::Assistant { thinking_open, .. }) = this.items.get_mut(ix) {
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
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
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
        let status = self.agent.read(cx).status().clone();
        let busy = self.agent.read(cx).is_busy();
        let items: Vec<AnyElement> = (0..self.items.len())
            .map(|ix| {
                // Split borrow: render needs &self.items[ix] and cx.
                let item = std::mem::replace(&mut self.items[ix], Item::Notice { text: String::new(), error: false });
                let el = self.render_item(ix, &item, cx);
                self.items[ix] = item;
                el
            })
            .collect();
        let theme = cx.theme();

        let status_line = match &status {
            AgentStatus::Idle => None,
            AgentStatus::Starting => Some("Starting pi…"),
            AgentStatus::Busy => Some("pi is working…"),
            AgentStatus::Aborting => Some("Stopping…"),
            AgentStatus::Crashed { .. } => None,
        };

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .child(
                div()
                    .id("chat-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        v_flex()
                            .p_3()
                            .gap_3()
                            .text_sm()
                            .when(self.items.is_empty(), |this| {
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
                            .child(
                                h_flex().child(
                                    Button::new("restart").small().primary().label("Restart pi").on_click(cx.listener(Self::restart)),
                                ),
                            ),
                    )
                },
            )
            .child(
                v_flex()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .when_some(status_line, |this, s| {
                        this.child(div().text_xs().text_color(theme.muted_foreground).child(s))
                    })
                    .child(Textarea::new(&self.input))
                    .child(
                        h_flex().justify_end().gap_2().when(busy, |this| {
                            this.child(Button::new("stop").small().danger().label("Stop").on_click(cx.listener(Self::stop)))
                        }),
                    ),
            )
    }
}
