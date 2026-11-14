//! The main view: dock area (left project tree, center editors, right pi),
//! status bar, layout persistence, keymap hot-reload, command dispatch.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::dock::{
    DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSkin, panel_handle,
};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_agent_chat::AgentChat;
use pide_core::{
    AgentStatus, CommandRegistry, RunCommand, Services, WorkspaceEvent, buffer::hash_bytes, paths,
};
use pide_file_tree::FileTree;

use crate::tool_windows::{self, Side};

const DOCK_ID: &str = "pide-main";
/// Bump when the default layout changes incompatibly; old saved layouts are then ignored.
const DOCK_VERSION: usize = 1;

/// Each project window's shell.
#[derive(Default)]
struct Shells(std::collections::HashMap<WindowId, WeakEntity<Shell>>);
impl Global for Shells {}

pub fn forget(window: WindowId, cx: &mut App) {
    cx.default_global::<Shells>().0.remove(&window);
}

/// The dock area of `window`'s shell.
pub fn dock_of(window: &Window, cx: &App) -> Option<Entity<DockArea>> {
    let id = window.window_handle().window_id();
    let shell = cx.try_global::<Shells>()?.0.get(&id)?.upgrade()?;
    Some(shell.read(cx).dock.clone())
}

/// Run: Run Command… — ask for a shell command and run it in the Run panel.
pub fn run_command(window: &mut Window, cx: &mut App) {
    let last = pide_run::existing(window, cx)
        .map(|p| p.read(cx).last_command().to_string())
        .unwrap_or_default();
    pide_run::prompt(
        last,
        Rc::new(|command, window, cx| {
            if let Some(panel) = show_run_panel(window, cx) {
                panel.update(cx, |p, cx| p.start(command, cx));
            }
        }),
        window,
        cx,
    );
}

/// The Run panel in the bottom dock (added the first time), shown and selected.
fn show_run_panel(window: &mut Window, cx: &mut App) -> Option<Entity<pide_run::RunPanel>> {
    let id = window.window_handle().window_id();
    let shell = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())?;
    let dock = shell.read(cx).dock.clone();
    let existing = pide_run::existing(window, cx).filter(|p| {
        dock.read(cx)
            .panel(gpui_kit::component::dock::PanelId::from(p.entity_id()))
            .is_some()
    });
    let panel = match existing {
        Some(p) => p,
        None => {
            let p = cx.new(|cx| pide_run::RunPanel::new(window, cx));
            dock.update(cx, |d, cx| {
                d.add_panel_view(
                    panel_handle(p.clone()),
                    DockPlacement::Bottom,
                    Some(px(240.)),
                    window,
                    cx,
                )
            });
            p
        }
    };
    let pid = gpui_kit::component::dock::PanelId::from(panel.entity_id());
    dock.update(cx, |d, cx| {
        if !d.is_dock_open(DockPlacement::Bottom) {
            d.toggle_dock(DockPlacement::Bottom, window, cx);
        }
        d.select_panel(pid, window, cx);
    });
    Some(panel)
}

/// Navigate: Select Opened File — show the active file in the project tree.
pub fn select_opened_file(window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let Some(shell) = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())
    else {
        return;
    };
    let (dock, path) = {
        let s = shell.read(cx);
        let path = s
            .services
            .workspace
            .read(cx)
            .active()
            .map(|b| b.read(cx).path.clone());
        (s.dock.clone(), path)
    };
    let Some(path) = path else { return };
    dock.update(cx, |d, cx| {
        if !d.is_dock_open(DockPlacement::Left) {
            d.toggle_dock(DockPlacement::Left, window, cx);
        }
    });
    pide_file_tree::reveal(path, window, cx);
}

/// Show the pi panel with `text` as a draft in its message box.
pub fn ask_pi_with(text: String, window: &mut Window, cx: &mut App) {
    ask_pi(window, cx);
    if let Some(chat) = pide_agent_chat::existing(window, cx) {
        chat.update(cx, |c, cx| c.set_draft(text, window, cx));
    }
}

/// pi: Ask pi — show the pi panel and put the cursor in its message box.
pub fn ask_pi(window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let Some(shell) = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())
    else {
        return;
    };
    let dock = shell.read(cx).dock.clone();
    dock.update(cx, |d, cx| {
        if !d.is_dock_open(DockPlacement::Right) {
            d.toggle_dock(DockPlacement::Right, window, cx);
        }
    });
    if let Some(chat) = pide_agent_chat::existing(window, cx) {
        let handle = chat.read(cx).focus_handle(cx);
        window.defer(cx, move |window, cx| handle.focus(window, cx));
    }
}

/// One saved layout per project root.
fn layout_file(root: &Path) -> PathBuf {
    let key = hash_bytes(root.to_string_lossy().as_bytes());
    paths::state_dir()
        .join("layouts")
        .join(format!("{key:016x}.json"))
}

pub struct Shell {
    services: Services,
    /// PDF tabs opened by command, to switch to instead of opening twice.
    /// Image tabs, likewise.
    images: Vec<(PathBuf, WeakEntity<pide_preview::image::ImageView>)>,
    /// Branch, ↓ pull and ↑ push in the status bar (`pide_git::widget`).
    git_widget: Entity<pide_git::widget::GitWidget>,
    /// "12:5" (or with the selection size) for the status bar; polled, since
    /// the editor has no cursor-moved event.
    cursor: Option<String>,
    _cursor_poll: Task<()>,
    lsp: Entity<pide_lsp::LspStore>,

    menu_bar: Entity<gpui_kit::component::menu::AppMenuBar>,
    dock: Entity<DockArea>,
    _skin: Rc<DockSkin>,
    focus: FocusHandle,
    layout_path: PathBuf,
    last_saved: Option<DockAreaState>,
    _save: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl Shell {
    pub fn new(keymap_errors: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let window_id = window.window_handle().window_id();
        let me = cx.weak_entity();
        cx.default_global::<Shells>().0.insert(window_id, me);
        let services = Services::of(window, cx);
        let root = services.workspace.read(cx).root().to_path_buf();
        let layout_path = layout_file(&root);

        let (dock, skin) = DockSkin::dock_area(DOCK_ID, Some(DOCK_VERSION), window, cx);
        pide_editor::set_host(window_id, dock.downgrade(), cx);

        if let Err(e) = Self::load_layout(&dock, &layout_path, window, cx) {
            log::info!("no saved layout ({e:#}); using the default");
            Self::default_layout(&dock, window, cx);
        }
        dock.update(cx, |d, cx| {
            for p in [
                DockPlacement::Left,
                DockPlacement::Right,
                DockPlacement::Bottom,
            ] {
                d.set_dock_collapsible(p, true, window, cx);
            }
        });

        let subs = vec![
            cx.subscribe_in(&dock, window, |this, _, ev: &DockEvent, window, cx| {
                if matches!(ev, DockEvent::LayoutChanged) {
                    this.schedule_save(window, cx);
                }
            }),
            cx.observe(&services.sessions, |_, _, cx| cx.notify()),
            // The tool stripes show which panels are open.
            cx.observe(&dock, |_, _, cx| cx.notify()),
            cx.observe_window_appearance(window, |_, window, cx| {
                crate::theme::on_system_appearance_changed(window, cx)
            }),
            cx.subscribe(&services.workspace, |_, _, ev: &WorkspaceEvent, cx| {
                if !matches!(ev, WorkspaceEvent::TreeChanged) {
                    cx.notify();
                }
            }),
        ];

        // Save the layout on quit, synchronously enough to land before exit.
        cx.on_app_quit(|this, cx| {
            let state = this.dock.read(cx).dump(cx);
            let path = this.layout_path.clone();
            async move { save_state(&path, &state) }
        })
        .detach();
        // And when the window closes. Written right away (a few KB): closing the
        // last window quits, which wouldn't wait for a background task.
        cx.on_release(|this, cx| {
            let state = this.dock.read(cx).dump(cx);
            save_state(&this.layout_path, &state);
        })
        .detach();

        let lsp = pide_lsp::LspStore::for_workspace(&services.workspace, cx);
        let mut subs = subs;
        subs.push(cx.observe(&lsp, |_, _, cx| cx.notify()));
        subs.push(cx.subscribe_in(&lsp, window, |_, store, ev, window, cx| {
            if let pide_lsp::LspEvent::ApplyEdit(req) = ev {
                // A server command (from a quick fix) wants to change files.
                pide_editor::apply_workspace_edit(req.edit.clone(), req.encoding, window, cx);
                store.read(cx).respond(req.server, req.id.clone(), serde_json::json!({ "applied": true }));
            }
            if let pide_lsp::LspEvent::MissingServer { server } = ev {
                let note = Notification::warning(format!(
                    "{server} isn't installed, so these files get no errors, completion or go to definition."
                ))
                .action(|_, _, cx| {
                    let note = cx.entity().downgrade();
                    gpui_kit::component::button::Button::new("open-language-servers")
                        .label("Language Servers…")
                        .on_click(move |_, window, cx| {
                            CommandRegistry::run("lsp.servers", window, cx);
                            let _ = note.update(cx, |n, cx| n.dismiss(window, cx));
                        })
                });
                window.push_notification(note, cx);
            }
        }));
        let git_widget = cx.new(|cx| pide_git::widget::GitWidget::new(window, cx));
        let cursor_poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let alive = this.update_in(cx, |this, window, cx| {
                    let label = pide_core::context::current(window, cx).map(|c| {
                        let (l, col) = c.end;
                        match &c.selected {
                            Some(t) if !t.is_empty() => {
                                format!("{l}:{col} ({} chars)", t.chars().count())
                            }
                            _ => format!("{l}:{col}"),
                        }
                    });
                    if this.cursor != label {
                        this.cursor = label;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let this = Self {
            cursor: None,
            _cursor_poll: cursor_poll,
            images: Vec::new(),
            git_widget,
            services: services.clone(),
            lsp,

            menu_bar: crate::menus::bar(cx),
            dock,
            _skin: skin,
            focus: cx.focus_handle(),
            layout_path,
            last_saved: None,
            _save: None,
            _subs: subs,
        };
        if let Some(file) = std::env::var_os("PIDE_BENCH") {
            window.defer(cx, move |window, cx| {
                pide_editor::run_benchmark(PathBuf::from(file), window, cx)
            });
        }
        window.on_next_frame(|_, _| {
            if let Some(t) = crate::STARTED.get() {
                log::info!(
                    "startup: process start -> first frame {} ms",
                    t.elapsed().as_millis()
                );
            }
        });
        this.focus.focus(window, cx);
        crate::goto_file::warm(window, cx);
        crate::recent_files::track(window, cx);
        crate::unsaved::guard_window(window, cx);
        if !keymap_errors.is_empty() {
            let msg = keymap_errors.join("\n");
            window.defer(cx, move |window, cx| {
                window.push_notification(Notification::error(msg), cx)
            });
        }
        this
    }

    fn default_layout(dock: &Entity<DockArea>, window: &mut Window, cx: &mut Context<Self>) {
        let tree = cx.new(|cx| FileTree::new(window, cx));
        let chat = cx.new(|cx| AgentChat::new(window, cx));
        let left = DockLayout::tabs().panel_view(panel_handle(tree), cx);
        let right = DockLayout::tabs().panel_view(panel_handle(chat), cx);
        dock.update(cx, |d, cx| {
            d.set_center(DockLayout::tabs(), window, cx);
            d.set_dock(DockPlacement::Left, left, window, cx);
            d.set_dock_size(DockPlacement::Left, px(260.), window, cx);
            d.set_dock(DockPlacement::Right, right, window, cx);
            d.set_dock_size(DockPlacement::Right, px(420.), window, cx);
        });
    }

    fn load_layout(
        dock: &Entity<DockArea>,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        // A few KB, read once at startup before the window is shown.
        let json = std::fs::read_to_string(path)?;
        let state: DockAreaState = serde_json::from_str(&json)?;
        anyhow::ensure!(
            state.version == Some(DOCK_VERSION),
            "saved layout version differs"
        );
        dock.update(cx, |d, cx| d.load(state, window, cx))?;
        log::info!("restored layout from {}", path.display());
        Ok(())
    }

    fn schedule_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._save = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let Ok(Some((path, state))) = this.update(cx, |this, cx| {
                let state = this.dock.read(cx).dump(cx);
                if this.last_saved.as_ref() == Some(&state) {
                    return None;
                }
                this.last_saved = Some(state.clone());
                Some((this.layout_path.clone(), state))
            }) else {
                return;
            };
            cx.background_executor()
                .spawn(async move { save_state(&path, &state) })
                .await;
        }));
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let services = self.services.clone();
        let ws = services.workspace.read(cx);
        let root = ws.root().to_path_buf();
        let file = ws.active().map(|b| {
            let b = b.read(cx);
            let rel = b
                .path
                .strip_prefix(&root)
                .unwrap_or(&b.path)
                .display()
                .to_string();
            (
                rel,
                b.line_ending.label(),
                b.is_dirty(),
                b.read_only.is_some(),
            )
        });
        let (errors, warnings) = pide_problems::counts(self.lsp.read(cx));
        let server_label = ws.active().and_then(|b| {
            let path = b.read(cx).path.clone();
            self.lsp
                .read(cx)
                .server_of(&path)
                .map(|s| format!("{}: {}", s.spec.id, s.status_text()))
        });
        let sessions = services.sessions.read(cx);
        let (mut working, mut waiting, mut crashed) = (0, 0, 0);
        for a in sessions.all() {
            let a = a.read(cx);
            match a.status() {
                AgentStatus::Busy | AgentStatus::Starting | AgentStatus::Aborting => working += 1,
                AgentStatus::Crashed { .. } => crashed += 1,
                _ if a.waiting => waiting += 1,
                _ => {}
            }
        }
        let mut parts = Vec::new();
        if working > 0 {
            parts.push(format!("{working} working"));
        }
        if waiting > 0 {
            parts.push(format!("{waiting} need you"));
        }
        if crashed > 0 {
            parts.push(format!("{crashed} crashed"));
        }
        let agent_label = if parts.is_empty() {
            format!(
                "pi: {} session{}, idle",
                sessions.all().len(),
                if sessions.all().len() == 1 { "" } else { "s" }
            )
        } else {
            format!("pi: {}", parts.join(", "))
        };
        let to_review = pide_core::collab::sessions_to_review(sessions, cx).len();
        let active = sessions.active();
        let items: Vec<String> = active
            .read(cx)
            .status_items
            .iter()
            .map(|(_, t)| t.clone())
            .collect();
        let theme = cx.theme();
        h_flex()
            .w_full()
            .justify_between()
            .gap_4()
            .px_3()
            .py_1()
            .text_xs()
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.border)
            .text_color(theme.muted_foreground)
            .child(
                h_flex()
                    .gap_3()
                    .child(self.git_widget.clone())
                    .when(errors + warnings > 0, |this| {
                        this.child(
                            div()
                                .id("problems-count")
                                .cursor_pointer()
                                .child(format!("⊘ {errors}  ⚠ {warnings}"))
                                .on_click(|_, window, cx| {
                                    CommandRegistry::run("view.problems", window, cx);
                                }),
                        )
                    })
                    .when_some(file, |this, (rel, le, dirty, ro)| {
                        this.child(format!("{rel}{}", if dirty { " ●" } else { "" }))
                            .children(self.cursor.clone())
                            .child(le)
                            .when(ro, |this| this.child("read-only"))
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .children(server_label)
                    .children(items)
                    .when(to_review > 0, |this| {
                        let color = pide_core::colors::pi_note(cx);
                        this.child(
                            div()
                                .id("pi-review-chip")
                                .cursor_pointer()
                                .px_1p5()
                                .rounded(px(4.))
                                .bg(color.opacity(0.14))
                                .text_color(cx.theme().foreground)
                                .child(format!("◆ {to_review} to review"))
                                .on_click(|_, window, cx| {
                                    pide_core::collab::review_next(window, cx);
                                }),
                        )
                    })
                    .child(
                        div()
                            .id("agent-status")
                            .cursor_pointer()
                            .child(agent_label)
                            .on_click(|_, window, cx| {
                                CommandRegistry::run("agent.ask", window, cx);
                            }),
                    ),
            )
    }
}

fn save_state(path: &Path, state: &DockAreaState) {
    let result = (|| {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, serde_json::to_string_pretty(state)?)?;
        anyhow::Ok(())
    })();
    if let Err(e) = result {
        log::error!("save layout {}: {e:#}", path.display());
    }
}

/// Open (or focus) the Language Servers tab in `window`.
pub fn show_language_servers(window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let Some(shell) = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())
    else {
        return;
    };
    shell.update(cx, |shell, cx| {
        if let Some(p) = pide_language_servers::existing(window, cx) {
            let pid = gpui_kit::component::dock::PanelId::from(p.entity_id());
            if shell.dock.read(cx).panel(pid).is_some() {
                shell
                    .dock
                    .update(cx, |d, cx| d.select_panel(pid, window, cx));
                return;
            }
        }
        let p = cx.new(|cx| pide_language_servers::LanguageServers::new(window, cx));
        shell.dock.update(cx, |d, cx| {
            d.add_panel_view(
                panel_handle(p.clone()),
                DockPlacement::Center,
                None,
                window,
                cx,
            )
        });
        let _ = p;
    });
}

/// Open (or focus) the pi Settings tab in `window`.
pub fn show_pi_settings(window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let Some(shell) = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())
    else {
        return;
    };
    shell.update(cx, |shell, cx| {
        if let Some(p) = pide_pi_settings::existing(window, cx) {
            let pid = gpui_kit::component::dock::PanelId::from(p.entity_id());
            if shell.dock.read(cx).panel(pid).is_some() {
                shell
                    .dock
                    .update(cx, |d, cx| d.select_panel(pid, window, cx));
                return;
            }
        }
        let p = cx.new(|cx| pide_pi_settings::PiSettings::new(window, cx));
        shell.dock.update(cx, |d, cx| {
            d.add_panel_view(panel_handle(p), DockPlacement::Center, None, window, cx)
        });
    });
}

/// Open (or focus) Find in Files in the bottom dock, searching for the selection if there is one.
pub fn show_find_in_files(window: &mut Window, cx: &mut App) {
    let selection = pide_core::context::current(window, cx).and_then(|c| c.selected);
    let id = window.window_handle().window_id();
    let Some(shell) = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())
    else {
        return;
    };
    let panel = shell.update(cx, |shell, cx| {
        if let Some(p) = pide_search::existing(window, cx) {
            let pid = gpui_kit::component::dock::PanelId::from(p.entity_id());
            if shell.dock.read(cx).panel(pid).is_some() {
                shell.dock.update(cx, |d, cx| {
                    if !d.is_dock_open(DockPlacement::Bottom) {
                        d.toggle_dock(DockPlacement::Bottom, window, cx);
                    }
                    d.select_panel(pid, window, cx);
                });
                return p;
            }
        }
        let p = cx.new(|cx| pide_search::FindInFiles::new(window, cx));
        shell.dock.update(cx, |d, cx| {
            d.add_panel_view(
                panel_handle(p.clone()),
                DockPlacement::Bottom,
                Some(px(260.)),
                window,
                cx,
            );
            if !d.is_dock_open(DockPlacement::Bottom) {
                d.toggle_dock(DockPlacement::Bottom, window, cx);
            }
        });
        p
    });
    window.defer(cx, move |window, cx| {
        panel.update(cx, |p, cx| p.focus_query(selection, window, cx))
    });
}

/// The file of the active editor tab in `window`.
pub fn active_path(window: &Window, cx: &App) -> Option<PathBuf> {
    let s = Services::for_window(window.window_handle().window_id(), cx)?;
    let ws = s.workspace.read(cx);
    Some(ws.active()?.read(cx).path.clone())
}

/// Preview the active Markdown file beside the editor (or focus its preview).
pub fn show_markdown_preview(window: &mut Window, cx: &mut App) {
    let Some(path) = active_path(window, cx).filter(|p| pide_preview::is_markdown(p)) else {
        window.push_notification(Notification::warning("Open a Markdown file first."), cx);
        return;
    };
    let id = window.window_handle().window_id();
    let Some(shell) = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())
    else {
        return;
    };
    shell.update(cx, |shell, cx| {
        let p = cx.new(|cx| pide_preview::MarkdownPreview::new(path, window, cx));
        let pid = gpui_kit::component::dock::PanelId::from(p.entity_id());
        shell.dock.update(cx, |d, cx| {
            d.add_panel_view(panel_handle(p), DockPlacement::Center, None, window, cx);
            let node = d
                .layout(DockPlacement::Center)
                .and_then(|t| t.find_panel_node(pid));
            if let Some(node) = node {
                // `move_panel` detaches it from the group it was added to (`split_at` only inserts).
                let target = gpui_kit::component::dock::InsertTarget::Split {
                    node,
                    placement: gpui_kit::component::Placement::Right,
                    size: None,
                };
                d.move_panel(pid, target, window, cx);
            }
        });
    });
}

/// Open a PDF in the center (or switch to its tab).
pub fn open_pdf(path: PathBuf, window: &mut Window, cx: &mut App) {
    show_pdf(
        pide_core::opener::PdfRequest {
            path,
            beside: false,
            reload: false,
            at: None,
        },
        window,
        cx,
    );
}

/// `opener::show_pdf`: open (beside the active tab when asked), reload, scroll
/// to a SyncTeX spot. A request that isn't a plain open keeps the focus where it is.
pub fn show_pdf(req: pide_core::opener::PdfRequest, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let Some(shell) = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())
    else {
        return;
    };
    let keep_focus = (req.beside || req.reload || req.at.is_some())
        .then(|| window.focused(cx))
        .flatten();
    shell.update(cx, |shell, cx| {
        let open = pide_pdf::find(&req.path, window, cx).filter(|p| {
            let pid = gpui_kit::component::dock::PanelId::from(p.entity_id());
            shell.dock.read(cx).panel(pid).is_some()
        });
        let p = if let Some(p) = open {
            let pid = gpui_kit::component::dock::PanelId::from(p.entity_id());
            shell
                .dock
                .update(cx, |d, cx| d.select_panel(pid, window, cx));
            if req.reload {
                p.update(cx, |p, cx| p.reload(cx));
            }
            p
        } else {
            let p = cx.new(|cx| pide_pdf::PdfPanel::new(req.path.clone(), window, cx));
            let pid = gpui_kit::component::dock::PanelId::from(p.entity_id());
            shell.dock.update(cx, |d, cx| {
                d.add_panel_view(panel_handle(p.clone()), DockPlacement::Center, None, window, cx);
                if req.beside {
                    let node = d
                        .layout(DockPlacement::Center)
                        .and_then(|t| t.find_panel_node(pid));
                    if let Some(node) = node {
                        // `move_panel` detaches it from the group it was added to.
                        let target = gpui_kit::component::dock::InsertTarget::Split {
                            node,
                            placement: gpui_kit::component::Placement::Right,
                            size: None,
                        };
                        d.move_panel(pid, target, window, cx);
                    }
                }
            });
            p
        };
        if let Some(at) = req.at {
            p.update(cx, |p, cx| p.reveal(at, cx));
        }
    });
    if let Some(f) = keep_focus {
        window.focus(&f, cx);
    }
}

/// Open an image in the center (or switch to its tab).
/// A non-file center tab (`opener::open_panel`): select it if it's open, else add it.
pub fn open_center_panel(
    view: std::sync::Arc<dyn gpui_kit::component::dock::BasePanelView>,
    entity: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    let id = window.window_handle().window_id();
    let Some(shell) = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())
    else {
        return;
    };
    shell.update(cx, |shell, cx| {
        let pid = gpui_kit::component::dock::PanelId::from(entity);
        if shell.dock.read(cx).panel(pid).is_some() {
            shell
                .dock
                .update(cx, |d, cx| d.select_panel(pid, window, cx));
        } else {
            shell.dock.update(cx, |d, cx| {
                d.add_panel_view(view, DockPlacement::Center, None, window, cx)
            });
        }
    });
}

pub fn open_image(path: PathBuf, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let Some(shell) = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())
    else {
        return;
    };
    shell.update(cx, |shell, cx| {
        let open = shell
            .images
            .iter()
            .filter_map(|(p, w)| (p == &path).then(|| w.upgrade()).flatten())
            .next();
        if let Some(p) = open {
            let pid = gpui_kit::component::dock::PanelId::from(p.entity_id());
            if shell.dock.read(cx).panel(pid).is_some() {
                shell
                    .dock
                    .update(cx, |d, cx| d.select_panel(pid, window, cx));
                return;
            }
        }
        let p = cx.new(|cx| pide_preview::image::ImageView::new(path.clone(), window, cx));
        shell.images.retain(|(_, w)| w.upgrade().is_some());
        shell.images.push((path, p.downgrade()));
        shell.dock.update(cx, |d, cx| {
            d.add_panel_view(panel_handle(p), DockPlacement::Center, None, window, cx)
        });
    });
}

/// Show locations (Find Usages) in the bottom dock, reusing the panel.
pub fn show_locations(
    title: SharedString,
    items: Vec<pide_core::opener::Location>,
    window: &mut Window,
    cx: &mut App,
) {
    let id = window.window_handle().window_id();
    let Some(shell) = cx
        .try_global::<Shells>()
        .and_then(|h| h.0.get(&id)?.upgrade())
    else {
        return;
    };
    shell.update(cx, |shell, cx| {
        let existing = pide_search::locations::existing(window, cx).filter(|p| {
            shell
                .dock
                .read(cx)
                .panel(gpui_kit::component::dock::PanelId::from(p.entity_id()))
                .is_some()
        });
        let p = match existing {
            Some(p) => p,
            None => {
                let p = cx.new(|cx| pide_search::locations::Locations::new(window, cx));
                shell.dock.update(cx, |d, cx| {
                    d.add_panel_view(
                        panel_handle(p.clone()),
                        DockPlacement::Bottom,
                        Some(px(240.)),
                        window,
                        cx,
                    )
                });
                p
            }
        };
        p.update(cx, |p, cx| p.set(title, items, cx));
        let pid = gpui_kit::component::dock::PanelId::from(p.entity_id());
        shell.dock.update(cx, |d, cx| {
            if !d.is_dock_open(DockPlacement::Bottom) {
                d.toggle_dock(DockPlacement::Bottom, window, cx);
            }
            d.select_panel(pid, window, cx);
        });
    });
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Shell")
            // Not `cx.listener`: that would run every command inside the shell's
            // own update, and commands that read the shell (toggle_dock) panic.
            .on_action(|action: &RunCommand, window, cx| {
                CommandRegistry::run(&action.id, window, cx);
            })
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // Body text is 14 px everywhere; unsized text would otherwise take the
            // 16 px rem and read like a heading. See "Text sizes" in AGENTS.md.
            .text_sm()
            .child(
                h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .px_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().title_bar)
                    .child(self.menu_bar.clone()),
            )
            .child({
                // No tab open: a few starting points in the middle (the dock still
                // takes the clicks; this layer is drawn on top and ignores them).
                let empty = self.dock.read(cx).is_empty(DockPlacement::Center, cx);
                let muted = cx.theme().muted_foreground;
                let hint = |keys: &'static str, what: &'static str| {
                    h_flex()
                        .gap_3()
                        .child(div().w(px(130.)).text_right().child(keys))
                        .child(div().text_color(muted).child(what))
                };
                let editor_area = div()
                    .relative()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.dock.clone())
                    .when(empty, |this| {
                        this.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    v_flex()
                                        .gap_1p5()
                                        .text_sm()
                                        .child(hint("⌘⇧O", "Go to file"))
                                        .child(hint("⌘E", "Recent files"))
                                        .child(hint("⌘⇧F", "Find in files"))
                                        .child(hint("⌘⇧A", "Find action"))
                                        .child(hint("⌘⇧I", "Ask pi")),
                                ),
                        )
                    });
                h_flex()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .child(tool_windows::stripe(Side::Left, &self.dock, cx))
                    .child(editor_area)
                    .child(tool_windows::stripe(Side::Right, &self.dock, cx))
            })
            .child(self.render_status_bar(cx))
    }
}
