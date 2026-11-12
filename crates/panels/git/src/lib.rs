//! Commit tool window (Cmd+K, JetBrains style): the files changed against
//! HEAD, in "Changes" and "Unversioned Files", each with a checkbox; open a
//! file (▸) to check single changes. Commit takes exactly what's checked,
//! whatever git's staging area holds. ✦ Draft with pi writes the message from
//! the checked diff. Click a file for its diff against HEAD.
//! Status is read with gix (`repo.rs`), refreshed on saves, tree changes and a
//! slow poll; changes go through the `git` command.

pub mod annotate;
pub mod blame;
pub mod commit;
pub mod log;
pub mod log_panel;
pub mod remote;
pub mod repo;
pub mod widget;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
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

use commit::FileHunks;
use repo::{Changes, Entry, Kind};

pub const PANEL_NAME: &str = "git";
/// Lines shown per change under an opened file.
const PREVIEW_LINES: usize = 8;

pub fn init(cx: &mut App) {
    log_panel::init(cx);
    widget::init(cx);
    annotate::init(cx);
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| GitPanel::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "Commit",
            icon: gpui_kit::component::IconName::Github,
            dock: DockPosition::Left,
            command: "git.changes",
            key: Some("cmd-k"),
            create: |window, cx| panel_handle(cx.new(|cx| GitPanel::new(window, cx))),
        },
    );
}

/// What of a file goes into the next commit.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Include {
    All,
    Nothing,
    /// One flag per change (hunk), in order.
    Changes(Vec<bool>),
}

/// A checkbox's look: checked, partly checked, unchecked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tri {
    All,
    Some,
    None,
}

impl Include {
    fn tri(&self) -> Tri {
        match self {
            Include::All => Tri::All,
            Include::Nothing => Tri::None,
            Include::Changes(v) if v.iter().all(|b| *b) => Tri::All,
            Include::Changes(v) if v.iter().any(|b| *b) => Tri::Some,
            Include::Changes(_) => Tri::None,
        }
    }
}

pub struct GitPanel {
    root: PathBuf,
    /// `None` until the first read, or outside a repository.
    changes: Option<Changes>,
    loaded: bool,
    error: Option<String>,
    /// Per file (repo-relative path); missing = the default (tracked: checked,
    /// unversioned: not), JetBrains-style.
    include: HashMap<String, Include>,
    /// Loaded changes of opened or partly checked files (`None`: not text).
    hunks: HashMap<String, Option<Arc<FileHunks>>>,
    expanded: HashSet<String>,
    message: Entity<TextareaState>,
    amend: bool,
    busy: bool,
    drafting: bool,
    focus: FocusHandle,
    _refresh: Option<Task<()>>,
    _draft: Option<Task<()>>,
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
                .auto_grow(3, 10)
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
                    } => this.commit(false, window, cx),
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
            changes: None,
            loaded: false,
            error: None,
            include: HashMap::new(),
            hunks: HashMap::new(),
            expanded: HashSet::new(),
            message,
            amend: false,
            busy: false,
            drafting: false,
            focus: cx.focus_handle(),
            _refresh: None,
            _draft: None,
            _poll: poll,
            _subs: subs,
        };
        this.refresh(cx);
        this
    }

    /// Files whose single changes we need: opened, or partly checked.
    fn hunk_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.expanded.iter().cloned().collect();
        for (rel, inc) in &self.include {
            if matches!(inc, Include::Changes(_)) && !self.expanded.contains(rel) {
                paths.push(rel.clone());
            }
        }
        paths
    }

    /// Re-read the changes (and the opened files' hunks) in the background.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let paths = self.hunk_paths();
        let task = cx.background_executor().spawn(async move {
            let changes = repo::status(&root).map(|s| s.map(Changes::from));
            let hunks: Vec<(String, Option<FileHunks>)> = match &changes {
                Ok(Some(c)) => paths
                    .into_iter()
                    .map(|rel| {
                        let h = commit::load(&c.abs(&rel));
                        (rel, h)
                    })
                    .collect(),
                _ => Vec::new(),
            };
            (changes, hunks)
        });
        self._refresh = Some(cx.spawn(async move |this, cx| {
            let (changes, hunks) = task.await;
            let _ = this.update(cx, |this, cx| {
                let (changes, error) = match changes {
                    Ok(c) => (c, None),
                    Err(e) => (this.changes.take(), Some(format!("{e:#}"))),
                };
                let mut dirty = this.changes != changes || this.error != error || !this.loaded;
                for (rel, h) in hunks {
                    let h = h.map(Arc::new);
                    if this.hunks.get(&rel) != Some(&h) {
                        // The file changed: per-change checks no longer line up.
                        if let Some(Include::Changes(v)) = this.include.get(&rel)
                            && h.as_ref().is_none_or(|h| h.hunks.len() != v.len())
                        {
                            this.include.insert(rel.clone(), Include::All);
                        }
                        this.hunks.insert(rel, h);
                        dirty = true;
                    }
                }
                if let Some(c) = &changes {
                    let live: HashSet<&str> = c
                        .tracked
                        .iter()
                        .chain(&c.unversioned)
                        .map(|e| e.rel.as_str())
                        .collect();
                    this.include.retain(|rel, _| live.contains(rel.as_str()));
                    this.expanded.retain(|rel| live.contains(rel.as_str()));
                    this.hunks.retain(|rel, _| live.contains(rel.as_str()));
                }
                if dirty {
                    this.changes = changes;
                    this.error = error;
                    this.loaded = true;
                    cx.notify();
                }
            });
        }));
    }

    fn include_of(&self, e: &Entry) -> Include {
        self.include.get(&e.rel).cloned().unwrap_or(if e.kind == Kind::Untracked {
            Include::Nothing
        } else {
            Include::All
        })
    }

    fn set_include(&mut self, rel: &str, inc: Include, cx: &mut Context<Self>) {
        let inc = match inc {
            Include::Changes(v) if v.iter().all(|b| *b) => Include::All,
            Include::Changes(v) if !v.iter().any(|b| *b) => Include::Nothing,
            other => other,
        };
        self.include.insert(rel.to_string(), inc);
        cx.notify();
    }

    fn toggle_file(&mut self, e: &Entry, cx: &mut Context<Self>) {
        let next = if self.include_of(e).tri() == Tri::All {
            Include::Nothing
        } else {
            Include::All
        };
        self.set_include(&e.rel, next, cx);
    }

    fn toggle_group(&mut self, entries: &[Entry], cx: &mut Context<Self>) {
        let all = entries.iter().all(|e| self.include_of(e).tri() == Tri::All);
        for e in entries {
            let next = if all { Include::Nothing } else { Include::All };
            self.include.insert(e.rel.clone(), next);
        }
        cx.notify();
    }

    fn toggle_hunk(&mut self, e: &Entry, ix: usize, cx: &mut Context<Self>) {
        let Some(Some(h)) = self.hunks.get(&e.rel) else {
            return;
        };
        let n = h.hunks.len();
        let mut v = match self.include_of(e) {
            Include::All => vec![true; n],
            Include::Nothing => vec![false; n],
            Include::Changes(v) => v,
        };
        if let Some(b) = v.get_mut(ix) {
            *b = !*b;
        }
        self.set_include(&e.rel, Include::Changes(v), cx);
    }

    fn toggle_expanded(&mut self, rel: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(rel) {
            self.expanded.insert(rel.to_string());
            if !self.hunks.contains_key(rel) {
                self.refresh(cx);
            }
        }
        cx.notify();
    }

    fn entries(&self) -> Vec<Entry> {
        self.changes
            .as_ref()
            .map(|c| c.tracked.iter().chain(&c.unversioned).cloned().collect())
            .unwrap_or_default()
    }

    /// What the next commit takes: whole files, and partly checked files with
    /// the content to commit.
    fn selection(&self) -> (Vec<String>, Vec<(String, String)>) {
        let mut full = Vec::new();
        let mut partial = Vec::new();
        for e in self.entries() {
            match self.include_of(&e) {
                Include::All => full.push(e.rel.clone()),
                Include::Nothing => {}
                Include::Changes(v) => match self.hunks.get(&e.rel) {
                    Some(Some(h)) => {
                        partial.push((e.rel.clone(), commit::partial_content(h, &v)))
                    }
                    _ => full.push(e.rel.clone()),
                },
            }
        }
        (full, partial)
    }

    /// Run a git command off the UI thread, then refresh; errors become a notification.
    fn run<R: Send + 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        op: impl FnOnce(PathBuf) -> anyhow::Result<R> + Send + 'static,
        done: impl FnOnce(&mut Self, R, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(workdir) = self.changes.as_ref().map(|s| s.workdir.clone()) else {
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

    /// Roll the checked tracked files back to HEAD (asks first).
    fn rollback_checked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries: Vec<Entry> = self
            .changes
            .as_ref()
            .map(|c| c.tracked.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|e| self.include_of(e).tri() != Tri::None)
            .collect();
        if entries.is_empty() {
            return;
        }
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        let detail = if names.len() <= 6 {
            names.join("\n")
        } else {
            format!("{}\n… and {} more", names[..5].join("\n"), names.len() - 5)
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!(
                "Roll back {} file{} to HEAD?",
                entries.len(),
                if entries.len() == 1 { "" } else { "s" }
            ),
            Some(&format!("Their uncommitted changes are thrown away:\n{detail}")),
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
                    move |w| repo::rollback_to_head(&w, &entries),
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

    fn checked_count(&self) -> usize {
        self.entries()
            .iter()
            .filter(|e| self.include_of(e).tri() != Tri::None)
            .count()
    }

    fn can_commit(&self, cx: &App) -> bool {
        !self.busy
            && !self.message.read(cx).value().trim().is_empty()
            && (self.amend || self.checked_count() > 0)
    }

    fn commit(&mut self, and_push: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_commit(cx) {
            return;
        }
        let message = self.message.read(cx).value().to_string();
        let amend = self.amend;
        let (full, partial) = self.selection();
        self.run(
            window,
            cx,
            move |w| repo::commit_selection(&w, &message, amend, &full, &partial),
            move |this, summary, window, cx| {
                this.amend = false;
                this.include.clear();
                this.message.update(cx, |m, cx| m.set_value("", window, cx));
                window.push_notification(Notification::success(format!("Committed {summary}")), cx);
                if and_push {
                    window.defer(cx, |window, cx| {
                        pide_core::CommandRegistry::run("git.push", window, cx);
                    });
                }
            },
        );
    }

    /// ✦ Draft with pi: a commit message for the checked changes.
    fn draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(changes) = self.changes.clone() else {
            return;
        };
        if self.drafting || self.checked_count() == 0 {
            return;
        }
        let picked: Vec<(Entry, Include)> = self
            .entries()
            .into_iter()
            .map(|e| {
                let inc = self.include_of(&e);
                (e, inc)
            })
            .filter(|(_, inc)| inc.tri() != Tri::None)
            .collect();
        self.drafting = true;
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            let mut diff = String::new();
            for (e, inc) in &picked {
                let Some(h) = commit::load(&changes.abs(&e.rel)) else {
                    diff.push_str(&format!("Binary file {} changed\n", e.rel));
                    continue;
                };
                let after = match inc {
                    Include::Changes(v) => commit::partial_content(&h, v),
                    _ => h.after.clone(),
                };
                diff.push_str(&commit::unified(&e.rel, &h.before, &after));
            }
            let subjects = repo::recent_subjects(&changes.workdir, 10);
            commit::ask_pi(&changes.workdir, &commit::draft_prompt(&diff, &subjects))
        });
        self._draft = Some(cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.drafting = false;
                match result {
                    Ok(text) => this
                        .message
                        .update(cx, |m, cx| m.set_value(text, window, cx)),
                    Err(e) => window.push_notification(
                        Notification::error(format!("No draft: {e:#}")),
                        cx,
                    ),
                }
                cx.notify();
            });
        }));
    }

    /// The file's diff against HEAD in the diff tab.
    fn show_diff(&self, rel: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.changes.as_ref().map(|s| s.abs(rel)) else {
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

    fn checkbox(&self, id: SharedString, tri: Tri, cx: &App) -> Stateful<Div> {
        let theme = cx.theme();
        let on = tri != Tri::None;
        div()
            .id(id)
            .size(px(14.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.))
            .border_1()
            .text_size(px(10.))
            .cursor_pointer()
            .when(on, |d| {
                let accent = pide_core::colors::accent(cx);
                d.bg(accent).border_color(accent).text_color(theme.background)
            })
            .when(!on, |d| d.border_color(theme.input))
            .child(match tri {
                Tri::All => "✓",
                Tri::Some => "−",
                Tri::None => "",
            })
    }

    fn render_file(&self, e: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        use pide_core::colors::{Change, change};
        let color = change(
            match e.kind {
                Kind::Untracked => Change::Untracked,
                Kind::Added => Change::Added,
                Kind::Deleted => Change::Deleted,
                Kind::Conflict => Change::Conflict,
                Kind::Modified | Kind::Renamed => Change::Modified,
            },
            cx,
        );
        let (dir, name) = match e.rel.rsplit_once('/') {
            Some((d, n)) => (d.to_string(), n.to_string()),
            None => (String::new(), e.rel.clone()),
        };
        let inc = self.include_of(e);
        let open = self.expanded.contains(&e.rel);
        let id = e.rel.clone();
        let entry = e.clone();
        let row = h_flex()
            .id(SharedString::from(format!("f-{id}")))
            .pl(px(26.))
            .pr_2()
            .py_0p5()
            .gap_1p5()
            .text_sm()
            .when(open, |d| d.bg(theme.list_active))
            .hover(|d| d.bg(theme.list_hover))
            .child({
                let entry = entry.clone();
                self.checkbox(format!("c-{id}").into(), inc.tri(), cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_file(&entry, cx)))
            })
            .child({
                let rel = e.rel.clone();
                div()
                    .id(SharedString::from(format!("x-{id}")))
                    .w(px(12.))
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .child(if open { "▾" } else { "▸" })
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_expanded(&rel, cx)))
            })
            .child({
                let rel = e.rel.clone();
                h_flex()
                    .id(SharedString::from(format!("n-{id}")))
                    .flex_1()
                    .min_w_0()
                    .gap_1p5()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .child(div().text_color(color).child(name))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .text_ellipsis()
                            .child(dir),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_diff(&rel, window, cx)
                    }))
            });
        let mut col = v_flex().child(row);
        if open {
            match self.hunks.get(&e.rel) {
                None => {
                    col = col.child(
                        div()
                            .pl(px(48.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Reading changes…"),
                    )
                }
                Some(None) => {
                    col = col.child(
                        div()
                            .pl(px(48.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Not a text file: committed whole."),
                    )
                }
                Some(Some(h)) => {
                    let flags: Vec<bool> = match &inc {
                        Include::All => vec![true; h.hunks.len()],
                        Include::Nothing => vec![false; h.hunks.len()],
                        Include::Changes(v) => v.clone(),
                    };
                    for (i, hunk) in h.hunks.iter().enumerate() {
                        let lines = commit::hunk_lines(h, hunk);
                        let more = lines.len().saturating_sub(PREVIEW_LINES);
                        let entry = entry.clone();
                        let head = format!(
                            "@@ -{},{} +{},{} @@",
                            hunk.before.start + 1,
                            hunk.before.len(),
                            hunk.after.start + 1,
                            hunk.after.len()
                        );
                        col = col.child(
                            v_flex()
                                .ml(px(48.))
                                .mr_2()
                                .my_0p5()
                                .border_1()
                                .border_color(theme.border)
                                .rounded(px(5.))
                                .overflow_hidden()
                                .child(
                                    h_flex()
                                        .gap_1p5()
                                        .px_1p5()
                                        .py_0p5()
                                        .bg(theme.secondary)
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(
                                            self.checkbox(
                                                format!("h-{id}-{i}").into(),
                                                if flags.get(i).copied().unwrap_or(true) {
                                                    Tri::All
                                                } else {
                                                    Tri::None
                                                },
                                                cx,
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.toggle_hunk(&entry, i, cx)
                                            })),
                                        )
                                        .child(head),
                                )
                                .children(lines.into_iter().take(PREVIEW_LINES).map(|(sign, l)| {
                                    div()
                                        .px_1p5()
                                        .text_xs()
                                        .font_family(theme.mono_font_family.clone())
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .bg(if sign == '+' {
                                            theme.success.opacity(0.12)
                                        } else {
                                            theme.danger.opacity(0.12)
                                        })
                                        .child(format!("{sign} {l}"))
                                }))
                                .when(more > 0, |d| {
                                    d.child(
                                        div()
                                            .px_1p5()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("… {more} more lines")),
                                    )
                                }),
                        );
                    }
                }
            }
        }
        col.into_any_element()
    }

    fn group(&self, title: &str, entries: &[Entry], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tris: Vec<Tri> = entries.iter().map(|e| self.include_of(e).tri()).collect();
        let tri = if tris.iter().all(|t| *t == Tri::All) {
            Tri::All
        } else if tris.iter().all(|t| *t == Tri::None) {
            Tri::None
        } else {
            Tri::Some
        };
        let list = entries.to_vec();
        let rows: Vec<AnyElement> = entries.iter().map(|e| self.render_file(e, cx)).collect();
        v_flex()
            .child(
                h_flex()
                    .px_2()
                    .pt_2()
                    .pb_0p5()
                    .gap_1p5()
                    .text_sm()
                    .child(
                        self.checkbox(format!("g-{title}").into(), tri, cx)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_group(&list, cx)
                            })),
                    )
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} file{}",
                                entries.len(),
                                if entries.len() == 1 { "" } else { "s" }
                            )),
                    ),
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
        let n = self.entries().len();
        if n == 0 {
            "Commit".to_string()
        } else {
            format!("Commit · {n}")
        }
    }
}

impl Panel for GitPanel {
    fn title_text(&self, _: &App) -> SharedString {
        "Commit".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Left
    }
}

impl Render for GitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let root = v_flex().size_full().track_focus(&self.focus);
        let Some(changes) = self.changes.clone() else {
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
        let can_commit = self.can_commit(cx);
        let checked = self.checked_count();
        let nothing = changes.tracked.is_empty() && changes.unversioned.is_empty();
        let (full, partial) = self.selection();
        let any_tracked_checked = changes
            .tracked
            .iter()
            .any(|e| self.include_of(e).tri() != Tri::None);
        root.child(
            h_flex()
                .px_1()
                .py_0p5()
                .gap_0p5()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    Button::new("git-refresh")
                        .ghost()
                        .xsmall()
                        .icon(gpui_kit::component::IconName::RefreshCw)
                        .tooltip("Refresh")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
                .child(
                    Button::new("git-rollback")
                        .ghost()
                        .xsmall()
                        .icon(gpui_kit::component::IconName::Undo2)
                        .tooltip("Roll back the checked files to HEAD…")
                        .disabled(!any_tracked_checked || self.busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.rollback_checked(window, cx)
                        })),
                )
                .child(div().flex_1())
                .when(self.busy, |this| {
                    this.child(
                        div()
                            .px_1()
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
                .pb_2()
                .when(nothing, |this| {
                    this.child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No local changes."),
                    )
                })
                .when(!changes.tracked.is_empty(), |this| {
                    this.child(self.group("Changes", &changes.tracked, cx))
                })
                .when(!changes.unversioned.is_empty(), |this| {
                    this.child(self.group("Unversioned Files", &changes.unversioned, cx))
                }),
        )
        .child(
            v_flex()
                .p_2()
                .gap_2()
                .border_t_1()
                .border_color(theme.border)
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
                            Button::new("git-draft")
                                .small()
                                .label(if self.drafting {
                                    "pi is writing…"
                                } else {
                                    "✦ Draft with pi"
                                })
                                .disabled(self.drafting || checked == 0)
                                .tooltip("pi writes a message for the checked changes")
                                .on_click(cx.listener(|this, _, window, cx| this.draft(window, cx))),
                        ),
                )
                .child(Textarea::new(&self.message))
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{} file{} checked{}",
                                    checked,
                                    if checked == 1 { "" } else { "s" },
                                    if partial.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" ({} partly)", partial.len())
                                    }
                                )),
                        )
                        .child(
                            h_flex()
                                .justify_end()
                                .flex_wrap()
                                .gap_1()
                                .child(
                                    Button::new("git-commit-push")
                                        .small()
                                        .label("Commit and Push…")
                                        .disabled(!can_commit)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.commit(true, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("git-commit")
                                        .primary()
                                        .small()
                                        .label(if self.amend { "Amend Commit" } else { "Commit" })
                                        .disabled(!can_commit)
                                        .tooltip(if full.is_empty() && partial.is_empty() && !self.amend {
                                            "Check files to commit"
                                        } else {
                                            "Commit the checked changes (⌘↩)"
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.commit(false, window, cx)
                                        })),
                                ),
                        ),
                ),
        )
    }
}
