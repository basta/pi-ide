//! Git tool window (Cmd+9): the branch, staged and unstaged files, a commit box.
//! Click a file for its diff against HEAD; + stages, − unstages, ↶ rolls back.
//! Status is read with gix (`repo.rs`), refreshed on saves, tree changes and a
//! slow poll; changes go through the `git` command.

pub mod repo;

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{DockPosition, Panel, Services, WorkspaceEvent};

use repo::{Entry, Kind, Status};

pub const PANEL_NAME: &str = "git";

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| GitPanel::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "Git",
            icon: gpui_kit::component::IconName::Github,
            dock: DockPosition::Left,
            command: "git.changes",
            key: Some("cmd-9"),
            create: |window, cx| panel_handle(cx.new(|cx| GitPanel::new(window, cx))),
        },
    );
}

pub struct GitPanel {
    root: PathBuf,
    /// `None` until the first read, or outside a repository.
    status: Option<Status>,
    loaded: bool,
    error: Option<String>,
    branch: Option<String>,
    ahead_behind: Option<(u32, u32)>,
    message: Entity<TextareaState>,
    amend: bool,
    busy: bool,
    focus: FocusHandle,
    _refresh: Option<Task<()>>,
    _poll: Task<()>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for GitPanel {}

impl GitPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = Services::of(window, cx).workspace;
        let root = workspace.read(cx).root().to_path_buf();
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("Commit message  (⌘↩ commits)")
        });
        let subs = vec![
            cx.subscribe(&workspace, |this, _, ev: &WorkspaceEvent, cx| {
                if matches!(
                    ev,
                    WorkspaceEvent::BufferSaved(_)
                        | WorkspaceEvent::FileChangedOnDisk { .. }
                        | WorkspaceEvent::FileDeleted(_)
                        | WorkspaceEvent::TreeChanged
                ) {
                    this.refresh(cx);
                }
            }),
            cx.subscribe_in(&message, window, |this, _, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.commit(window, cx),
                    // The Commit button enables once there's a message.
                    InputEvent::Change => cx.notify(),
                    _ => {}
                }
            }),
        ];
        // Catches what no event reports: a terminal's `git commit`, edits to unopened files.
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            root,
            status: None,
            loaded: false,
            error: None,
            branch: None,
            ahead_behind: None,
            message,
            amend: false,
            busy: false,
            focus: cx.focus_handle(),
            _refresh: None,
            _poll: poll,
            _subs: subs,
        };
        this.refresh(cx);
        this
    }

    /// Re-read status in the background (a refresh already running is replaced).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let task = cx.background_executor().spawn(async move {
            let status = repo::status(&root);
            let ab = match &status {
                Ok(Some(s)) => repo::ahead_behind(&s.workdir),
                _ => None,
            };
            (status, ab, pide_core::git::branch(&root))
        });
        self._refresh = Some(cx.spawn(async move |this, cx| {
            let (status, ab, branch) = task.await;
            let _ = this.update(cx, |this, cx| {
                let (status, error) = match status {
                    Ok(s) => (s, None),
                    Err(e) => (this.status.take(), Some(format!("{e:#}"))),
                };
                if this.status != status
                    || this.error != error
                    || this.branch != branch
                    || this.ahead_behind != ab
                    || !this.loaded
                {
                    this.status = status;
                    this.error = error;
                    this.branch = branch;
                    this.ahead_behind = ab;
                    this.loaded = true;
                    cx.notify();
                }
            });
        }));
    }

    /// Run a git command off the UI thread, then refresh; errors become a notification.
    fn run<R: Send + 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        op: impl FnOnce(PathBuf) -> anyhow::Result<R> + Send + 'static,
        done: impl FnOnce(&mut Self, R, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(workdir) = self.status.as_ref().map(|s| s.workdir.clone()) else {
            return;
        };
        self.busy = true;
        cx.notify();
        let task = cx.background_executor().spawn(async move { op(workdir) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(r) => done(this, r, window, cx),
                    Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    fn stage(&mut self, rels: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.run(window, cx, move |w| repo::stage(&w, &rels), |_, _, _, _| {});
    }

    fn unstage(&mut self, rels: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.run(
            window,
            cx,
            move |w| repo::unstage(&w, &rels),
            |_, _, _, _| {},
        );
    }

    fn rollback(&mut self, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Roll back {rel}?"),
            Some("Its uncommitted, unstaged changes are thrown away."),
            &["Roll Back", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.run(
                    window,
                    cx,
                    move |w| repo::rollback(&w, &[rel]),
                    |_, _, _, _| {},
                );
            });
        })
        .detach();
    }

    fn toggle_amend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.amend = !self.amend;
        cx.notify();
        if self.amend && self.message.read(cx).value().trim().is_empty() {
            self.run(
                window,
                cx,
                |w| Ok(repo::last_message(&w).unwrap_or_default()),
                |this, msg, window, cx| {
                    this.message
                        .update(cx, |m, cx| m.set_value(msg, window, cx));
                },
            );
        }
    }

    fn can_commit(&self, cx: &App) -> bool {
        !self.busy
            && !self.message.read(cx).value().trim().is_empty()
            && (self.amend || self.status.as_ref().is_some_and(|s| !s.staged.is_empty()))
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_commit(cx) {
            return;
        }
        let message = self.message.read(cx).value().to_string();
        let amend = self.amend;
        self.run(
            window,
            cx,
            move |w| repo::commit(&w, &message, amend),
            |this, summary, window, cx| {
                this.amend = false;
                this.message.update(cx, |m, cx| m.set_value("", window, cx));
                window.push_notification(Notification::success(format!("Committed {summary}")), cx);
            },
        );
    }

    /// The file's diff against HEAD in the diff tab.
    fn show_diff(&self, rel: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.status.as_ref().map(|s| s.abs(rel)) else {
            return;
        };
        let p = path.clone();
        let before = cx
            .background_executor()
            .spawn(async move { pide_core::git::head_text(&p) });
        window
            .spawn(cx, async move |cx| {
                let before = before.await;
                let _ = cx.update(|window, cx| {
                    pide_core::opener::open_diff(
                        pide_core::opener::DiffRequest {
                            path,
                            before,
                            after: None,
                            label: "Local changes".into(),
                            before_is: "HEAD".into(),
                        },
                        window,
                        cx,
                    )
                });
            })
            .detach();
    }

    fn render_row(&self, e: &Entry, staged: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let color = match e.kind {
            Kind::Added | Kind::Untracked => theme.success,
            Kind::Deleted | Kind::Conflict => theme.danger,
            _ => theme.warning,
        };
        let (dir, name) = match e.rel.rsplit_once('/') {
            Some((d, n)) => (d.to_string(), n.to_string()),
            None => (String::new(), e.rel.clone()),
        };
        let rel = e.rel.clone();
        let id = SharedString::from(format!("{}-{}", if staged { "s" } else { "u" }, e.rel));
        let action = {
            let rel = rel.clone();
            if staged {
                Button::new(SharedString::from(format!("{id}-unstage")))
                    .ghost()
                    .xsmall()
                    .label("−")
                    .tooltip("Unstage")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.unstage(vec![rel.clone()], window, cx)
                    }))
            } else {
                Button::new(SharedString::from(format!("{id}-stage")))
                    .ghost()
                    .xsmall()
                    .label("+")
                    .tooltip("Stage")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.stage(vec![rel.clone()], window, cx)
                    }))
            }
        };
        let rollback =
            (!staged && !matches!(e.kind, Kind::Untracked | Kind::Conflict)).then(|| {
                let rel = rel.clone();
                Button::new(SharedString::from(format!("{id}-rollback")))
                    .ghost()
                    .xsmall()
                    .label("↶")
                    .tooltip("Roll back")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.rollback(rel.clone(), window, cx)
                    }))
            });
        let muted = theme.muted_foreground;
        let hover = theme.list_hover;
        h_flex()
            .id(id)
            .px_2()
            .gap_2()
            .text_sm()
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .child(
                div()
                    .w(px(12.))
                    .flex_none()
                    .text_color(color)
                    .child(e.kind.letter()),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1p5()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(name)
                    .child(div().text_xs().text_color(muted).text_ellipsis().child(dir)),
            )
            .children(rollback)
            .child(action)
            .on_click(cx.listener(move |this, _, window, cx| this.show_diff(&rel, window, cx)))
            .into_any_element()
    }

    fn section(
        &self,
        title: &str,
        entries: &[Entry],
        staged: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let all: Vec<String> = entries.iter().map(|e| e.rel.clone()).collect();
        let rows: Vec<AnyElement> = entries
            .iter()
            .map(|e| self.render_row(e, staged, cx))
            .collect();
        let theme = cx.theme();
        v_flex()
            .child(
                h_flex()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{title} · {}", entries.len()))
                    .when(!entries.is_empty(), |this| {
                        this.child(
                            Button::new(SharedString::from(format!("all-{staged}")))
                                .ghost()
                                .xsmall()
                                .label(if staged { "Unstage all" } else { "Stage all" })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if staged {
                                        this.unstage(all.clone(), window, cx)
                                    } else {
                                        this.stage(all.clone(), window, cx)
                                    }
                                })),
                        )
                    }),
            )
            .children(rows)
    }
}

impl Focusable for GitPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for GitPanel {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for GitPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let n = self
            .status
            .as_ref()
            .map(|s| s.staged.len() + s.unstaged.len())
            .unwrap_or(0);
        if n == 0 {
            "Git".to_string()
        } else {
            format!("Git · {n}")
        }
    }
}

impl Panel for GitPanel {
    fn title_text(&self, _: &App) -> SharedString {
        "Git".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Left
    }
}

impl Render for GitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let root = v_flex().size_full().track_focus(&self.focus);
        let Some(status) = self.status.clone() else {
            let text = match (&self.error, self.loaded) {
                (Some(e), _) => e.clone(),
                (None, false) => "Reading git status…".into(),
                (None, true) => "This project isn't in a git repository.".into(),
            };
            return root.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(text),
            );
        };
        let branch = match (&self.branch, self.ahead_behind) {
            (Some(b), Some((a, bh))) if a + bh > 0 => format!("⎇ {b}  ↑{a} ↓{bh}"),
            (Some(b), _) => format!("⎇ {b}"),
            (None, _) => "⎇ (no branch)".into(),
        };
        let can_commit = self.can_commit(cx);
        let nothing = status.staged.is_empty() && status.unstaged.is_empty();
        root.child(
            h_flex()
                .px_2()
                .py_1()
                .gap_2()
                .justify_between()
                .border_b_1()
                .border_color(theme.border)
                .text_sm()
                .child(branch)
                .when(self.busy, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("working…"),
                    )
                }),
        )
        .when_some(self.error.clone(), |this, e| {
            this.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(e),
            )
        })
        .child(
            div()
                .id("git-files")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .when(nothing, |this| {
                    this.child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No local changes."),
                    )
                })
                .when(!status.staged.is_empty(), |this| {
                    this.child(self.section("Staged", &status.staged, true, cx))
                })
                .when(!status.unstaged.is_empty(), |this| {
                    this.child(self.section("Changes", &status.unstaged, false, cx))
                }),
        )
        .child(
            v_flex()
                .p_2()
                .gap_2()
                .border_t_1()
                .border_color(theme.border)
                .child(Textarea::new(&self.message))
                .child(
                    h_flex()
                        .justify_between()
                        .child(
                            Button::new("git-amend")
                                .ghost()
                                .small()
                                .label(if self.amend { "☑ Amend" } else { "☐ Amend" })
                                .selected(self.amend)
                                .tooltip("Rewrite the last commit")
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.toggle_amend(window, cx)
                                    }),
                                ),
                        )
                        .child(
                            Button::new("git-commit")
                                .primary()
                                .small()
                                .label(if self.amend { "Amend Commit" } else { "Commit" })
                                .disabled(!can_commit)
                                .tooltip(if status.staged.is_empty() && !self.amend {
                                    "Stage files to commit (+)"
                                } else {
                                    "Commit staged files (⌘↩)"
                                })
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.commit(window, cx)),
                                ),
                        ),
                ),
        )
    }
}
