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
use pide_core::{AgentStatus, CommandRegistry, RunCommand, Services, WorkspaceEvent, buffer::hash_bytes, fswatch, keymap, paths};
use pide_file_tree::FileTree;

const DOCK_ID: &str = "pide-main";
/// Bump when the default layout changes incompatibly; old saved layouts are then ignored.
const DOCK_VERSION: usize = 1;

struct ShellHandle(WeakEntity<Shell>);
impl Global for ShellHandle {}

pub fn toggle_dock(placement: DockPlacement, window: &mut Window, cx: &mut App) {
    if let Some(shell) = cx.try_global::<ShellHandle>().and_then(|h| h.0.upgrade()) {
        let dock = shell.read(cx).dock.clone();
        dock.update(cx, |d, cx| d.toggle_dock(placement, window, cx));
    }
}

/// One saved layout per project root.
fn layout_file(root: &Path) -> PathBuf {
    let key = hash_bytes(root.to_string_lossy().as_bytes());
    paths::state_dir().join("layouts").join(format!("{key:016x}.json"))
}

pub struct Shell {
    dock: Entity<DockArea>,
    _skin: Rc<DockSkin>,
    focus: FocusHandle,
    layout_path: PathBuf,
    last_saved: Option<DockAreaState>,
    _save: Option<Task<()>>,
    _keymap_watch: Option<(fswatch::Watcher, Task<()>)>,
    _subs: Vec<Subscription>,
}

impl Shell {
    pub fn new(keymap_errors: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.set_global(ShellHandle(cx.weak_entity()));
        let services = Services::global(cx);
        let root = services.workspace.read(cx).root().to_path_buf();
        let layout_path = layout_file(&root);

        let (dock, skin) = DockSkin::dock_area(DOCK_ID, Some(DOCK_VERSION), window, cx);
        cx.set_global(pide_editor::EditorHost { dock: dock.downgrade() });

        if let Err(e) = Self::load_layout(&dock, &layout_path, window, cx) {
            log::info!("no saved layout ({e:#}); using the default");
            Self::default_layout(&dock, window, cx);
        }
        dock.update(cx, |d, cx| {
            for p in [DockPlacement::Left, DockPlacement::Right, DockPlacement::Bottom] {
                d.set_dock_collapsible(p, true, window, cx);
            }
        });

        let subs = vec![
            cx.subscribe_in(&dock, window, |this, _, ev: &DockEvent, window, cx| {
                if matches!(ev, DockEvent::LayoutChanged) {
                    this.schedule_save(window, cx);
                }
            }),
            cx.observe(&services.agent, |_, _, cx| cx.notify()),
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

        let mut this = Self {
            dock,
            _skin: skin,
            focus: cx.focus_handle(),
            layout_path,
            last_saved: None,
            _save: None,
            _keymap_watch: None,
            _subs: subs,
        };
        this.watch_keymap(window, cx);
        if let Some(file) = std::env::var_os("PIDE_BENCH") {
            window.defer(cx, move |window, cx| pide_editor::run_benchmark(PathBuf::from(file), window, cx));
        }
        window.on_next_frame(|_, _| {
            if let Some(t) = crate::STARTED.get() {
                log::info!("startup: process start -> first frame {} ms", t.elapsed().as_millis());
            }
        });
        this.focus.focus(window, cx);
        if !keymap_errors.is_empty() {
            let msg = keymap_errors.join("\n");
            window.defer(cx, move |window, cx| window.push_notification(Notification::error(msg), cx));
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

    fn load_layout(dock: &Entity<DockArea>, path: &Path, window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<()> {
        // A few KB, read once at startup before the window is shown.
        let json = std::fs::read_to_string(path)?;
        let state: DockAreaState = serde_json::from_str(&json)?;
        anyhow::ensure!(state.version == Some(DOCK_VERSION), "saved layout version differs");
        dock.update(cx, |d, cx| d.load(state, window, cx))?;
        log::info!("restored layout from {}", path.display());
        Ok(())
    }

    fn schedule_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._save = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
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
            cx.background_executor().spawn(async move { save_state(&path, &state) }).await;
        }));
    }

    fn watch_keymap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = paths::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        match fswatch::watch(&dir, false) {
            Ok((watcher, mut rx)) => {
                let file = paths::keymap_file();
                let task = cx.spawn_in(window, async move |_, cx| {
                    while let Some(changes) = rx.recv().await {
                        if !changes.paths.iter().any(|p| p.file_name() == file.file_name()) {
                            continue;
                        }
                        let _ = cx.update(|window, cx| {
                            let errors = keymap::apply(cx);
                            let note = if errors.is_empty() {
                                Notification::info("Keymap reloaded.")
                            } else {
                                Notification::error(errors.join("\n"))
                            };
                            window.push_notification(note, cx);
                        });
                    }
                });
                self._keymap_watch = Some((watcher, task));
            }
            Err(e) => log::warn!("can't watch {}: {e:#}", dir.display()),
        }
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let services = Services::global(cx);
        let ws = services.workspace.read(cx);
        let root = ws.root().to_path_buf();
        let file = ws.active().map(|b| {
            let b = b.read(cx);
            let rel = b.path.strip_prefix(&root).unwrap_or(&b.path).display().to_string();
            (rel, b.line_ending.label(), b.is_dirty(), b.read_only.is_some())
        });
        let agent = services.agent.read(cx);
        let agent_label = match agent.status() {
            AgentStatus::Idle => "pi: idle".to_string(),
            AgentStatus::Starting => "pi: starting".into(),
            AgentStatus::Busy => "pi: working".into(),
            AgentStatus::Aborting => "pi: stopping".into(),
            AgentStatus::Crashed { .. } => "pi: crashed".into(),
        };
        let items: Vec<String> = agent.status_items.iter().map(|(_, t)| t.clone()).collect();
        let theme = cx.theme();
        h_flex()
            .w_full()
            .justify_between()
            .gap_4()
            .px_3()
            .py_1()
            .text_xs()
            .bg(theme.secondary)
            .border_t_1()
            .border_color(theme.border)
            .text_color(theme.muted_foreground)
            .child(h_flex().gap_3().when_some(file, |this, (rel, le, dirty, ro)| {
                this.child(format!("{rel}{}", if dirty { " ●" } else { "" }))
                    .child(le)
                    .when(ro, |this| this.child("read-only"))
            }))
            .child(h_flex().gap_3().children(items).child(agent_label))
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

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Shell")
            .on_action(cx.listener(|_, action: &RunCommand, window, cx| {
                CommandRegistry::run(&action.id, window, cx);
            }))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(div().flex_1().w_full().overflow_hidden().child(self.dock.clone()))
            .child(self.render_status_bar(cx))
    }
}
