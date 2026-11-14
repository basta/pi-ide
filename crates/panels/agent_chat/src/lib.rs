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
use gpui_kit::component::text::{TextView, TextViewState, TextViewStyle};
use gpui_kit::component::{ActiveTheme, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::sessions::{SavedSession, list_saved, user_text};
use pide_core::{
    Agent, AgentEvent, AgentStatus, CommandRegistry, CommandSpec, DialogAnswer, DialogKind,
    DialogRequest, DockPosition, Panel, Services, Sessions, SessionsEvent,
};
use serde_json::{Value, json};

mod agent_events;
mod catch_up;
mod changes_view;
mod conversation;
mod dialogs;
mod history;
mod model_picker;
mod rename;
mod sessions_view;
mod slash;
mod tour;
mod understand;

use conversation::tool_mut;
use history::{HistItem, parse_history, result_text, summarize_args};

pub const PANEL_NAME: &str = "agent_chat";

/// Every session's notes on the current file (open sessions now, closed ones
/// from their saved state, off the UI thread), as a list of locations.
fn notes_in_file(window: &mut Window, cx: &mut App) {
    use pide_core::notes::{FileNote, saved_notes};
    let Some(ctx) = pide_core::context::current(window, cx) else {
        window.push_notification(Notification::info("Open a file first."), cx);
        return;
    };
    let services = Services::of(window, cx);
    let root = services.workspace.read(cx).root().to_path_buf();
    let path = root.join(&ctx.path);
    let mut notes: Vec<FileNote> = Vec::new();
    let mut open_files = Vec::new();
    for agent in services.sessions.read(cx).all() {
        let a = agent.read(cx);
        open_files.extend(a.session_file.clone());
        for stop in a.stops().iter().filter(|s| s.path == path) {
            notes.push(FileNote::from_stop(stop, &a.name));
        }
    }
    let load = cx.background_executor().spawn({
        let (root, path) = (root.clone(), path.clone());
        async move { saved_notes(&root, &path, &open_files) }
    });
    let title: SharedString = format!(
        "pi's notes in {}",
        ctx.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    )
    .into();
    window
        .spawn(cx, async move |cx| {
            notes.extend(load.await);
            notes.sort_by_key(|n| n.line);
            let _ = cx.update(|window, cx| {
                if notes.is_empty() {
                    window.push_notification(
                        Notification::info("pi hasn't left notes on this file in any session."),
                        cx,
                    );
                    return;
                }
                let locations = notes
                    .into_iter()
                    .map(|n| {
                        let head = format!(
                            "{}{}: ",
                            n.session,
                            if n.outdated { " (outdated)" } else { "" }
                        );
                        pide_core::opener::Location {
                            path: path.clone(),
                            line: n.line.saturating_sub(1) as u32,
                            character: 0,
                            range: 0..head.len(),
                            text: format!("{head}{}", n.note.replace('\n', " ")),
                        }
                    })
                    .collect();
                pide_core::opener::show_locations(title, locations, window, cx);
            });
        })
        .detach();
}

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
    // Collaborator mode: the walkthrough over pi's changes and notes.
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.walk_through", "Agent: Walk Through Changes"),
        move |window, cx| {
            let agent = active(window, cx);
            pide_core::collab::start_walk(&agent, window, cx);
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.next_note", "Agent: Next Note"),
        move |window, cx| {
            let agent = active(window, cx);
            pide_core::collab::walk(&agent, 1, window, cx);
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.previous_note", "Agent: Previous Note"),
        move |window, cx| {
            let agent = active(window, cx);
            pide_core::collab::walk(&agent, -1, window, cx);
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.change_story", "Agent: Tell It as a Story"),
        move |window, cx| {
            let agent = active(window, cx);
            if agent.read(cx).changes.iter().all(|c| c.reverted) {
                window.push_notification(
                    Notification::info("pi hasn't changed anything in this session yet."),
                    cx,
                );
                return;
            }
            changes_view::send_or_queue(&agent, changes_view::story_prompt(), cx);
            CommandRegistry::run("agent.ask", window, cx);
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.catch_up", "Agent: Catch Me Up"),
        move |window, cx| {
            CommandRegistry::run("agent.ask", window, cx);
            if let Some(chat) = existing(window, cx) {
                chat.update(cx, |c, cx| {
                    c.show_changes = false;
                    c.catch_up_open = true;
                    c.catch_up(cx)
                });
            }
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.notes_in_file", "Agent: pi's Notes in This File"),
        notes_in_file,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.tour", "Agent: Take a Tour…"),
        move |window, cx| tour::open(sessions(window, cx), window, cx),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.explain_this", "Agent: Explain This Code"),
        move |window, cx| {
            let Some(ctx) = pide_core::context::current(window, cx) else {
                window.push_notification(
                    Notification::info("Put the cursor or a selection in a file first."),
                    cx,
                );
                return;
            };
            let what = if ctx.start.0 == ctx.end.0 {
                format!(
                    "the code around line {} of {} (the function or block it's in)",
                    ctx.start.0,
                    ctx.path.display()
                )
            } else {
                format!("lines {}–{} of {}", ctx.start.0, ctx.end.0, ctx.path.display())
            };
            let prompt = format!(
                "Explain {what} to me. I don't write this code by hand, but I want to understand it. \
                 Don't change anything. Read it (and what it calls, if that matters), then use \
                 ide_pin on the 2–5 places that matter most for understanding it, in the order I \
                 should read them, each with a short note on what it does and why. Then reply with a \
                 short overview: what it's for, how it works, anything surprising."
            );
            let agent = active(window, cx);
            agent.update(cx, |a, cx| {
                if a.is_busy() {
                    a.queue(prompt, true, cx);
                } else {
                    a.prompt(prompt, cx);
                }
            });
            CommandRegistry::run("agent.ask", window, cx);
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.review_next", "Agent: Review Next Session"),
        move |window, cx| {
            pide_core::collab::review_next(window, cx);
        },
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
    /// `ide_pin`: shown from the session's pin with this tool call id.
    Pin(String),
    /// `ide_explain_change` for this file (absolute path).
    Explained(PathBuf),
    /// `ide_tour` (its tool call id): the tour's stops, shown from the session's pins.
    Tour(String),
}

/// One session's conversation as shown in the panel.
#[derive(Default)]
struct SessionView {
    items: Vec<Item>,
    scroll: ScrollHandle,
    history_requested: bool,
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
    /// Update tour was asked in this session: the next tour pi gives replaces these (call ids).
    replacing_tour: HashMap<EntityId, Vec<String>>,
    /// The Understand card is shown on top of the conversation (the ? button).
    show_help: bool,
    /// Sessions a "catch me up" briefing is being written for.
    catching_up: std::collections::HashSet<EntityId>,
    /// The briefing card is shown (it hides with ×; Catch me up shows it again).
    catch_up_open: bool,
    /// Rendered briefing per session: (its text, the markdown view).
    catch_up_md: HashMap<EntityId, (String, Entity<TextViewState>)>,
    /// The finished checklist is expanded (an unfinished one always is).
    progress_open: bool,
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
    /// Tours in saved sessions that aren't open (session file → its tours).
    saved_tours: Vec<(PathBuf, Vec<pide_core::notes::TourInfo>)>,
    _load_saved: Option<Task<()>>,
    /// Text fields for pending `input`/`editor` dialogs, by dialog id.
    dialog_inputs: HashMap<String, (Entity<TextareaState>, Subscription)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for AgentChat {}

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
            progress_open: false,
            replacing_tour: HashMap::new(),
            show_help: false,
            catching_up: Default::default(),
            catch_up_open: true,
            catch_up_md: HashMap::new(),
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
            saved_tours: Vec::new(),
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
                let first_prompt = hist.iter().find_map(|h| match h {
                    HistItem::User(t) => Some(t.clone()),
                    _ => None,
                });
                let _ = agent.update(cx, |a, cx| {
                    // A session pi never named: call it after its first prompt, as new ones are.
                    if a.name.as_ref() == pide_core::agent::UNNAMED
                        && let Some(t) = &first_prompt
                    {
                        a.name = pide_core::agent::name_from_prompt(t).into();
                        cx.notify();
                    }
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
                        HistItem::Pin(id) => Item::Pin(id),
                        HistItem::Tour(id) => Item::Tour(id),
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
        let open = self.sessions.read(cx).open_files(cx);
        let list = cx.background_executor().spawn(async move {
            (list_saved(&root), pide_core::notes::saved_tours(&root, &open))
        });
        self._load_saved = Some(cx.spawn(async move |this, cx| {
            let (saved, tours) = list.await;
            let _ = this.update(cx, |this, cx| {
                this.saved = saved;
                this.saved_tours = tours;
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
                                        .ghost()
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
