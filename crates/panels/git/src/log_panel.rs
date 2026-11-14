//! Git Log tool window (Cmd+9, bottom dock): branches and tags on the left
//! (click one to show only its history), the commits with their graph in the
//! middle, the selected commit's message and changed files on the right.
//! Click a file for its diff in that commit. History is read with gix
//! (`log.rs`); a cheap check of the refs every few seconds reloads it after a
//! commit, checkout or fetch made anywhere.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{DockPosition, Panel, Services};

use crate::log::{self, ChangedFile, Commit, GraphRow, Log, RefKind};

pub const PANEL_NAME: &str = "git_log";

const ROW_H: f32 = 24.;
const LANE_W: f32 = 12.;
/// Lanes beyond this are clipped (the graph column doesn't grow without end).
const MAX_LANES: usize = 12;

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| GitLogPanel::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "Git Log",
            icon: gpui_kit::component::IconName::Github,
            dock: DockPosition::Bottom,
            command: "git.log",
            key: Some("cmd-9"),
            create: |window, cx| panel_handle(cx.new(|cx| GitLogPanel::new(window, cx))),
        },
    );
}

/// Each window's Git Log, and a branch filter waiting for one that's about to open.
#[derive(Default)]
struct Instances(std::collections::HashMap<WindowId, WeakEntity<GitLogPanel>>);
impl Global for Instances {}
#[derive(Default)]
struct PendingFilter(std::collections::HashMap<WindowId, String>);
impl Global for PendingFilter {}
#[derive(Default)]
struct PendingSelect(std::collections::HashMap<WindowId, String>);
impl Global for PendingSelect {}

/// Show the Git Log (all branches) with one commit selected, opening it if needed.
pub fn show_commit(hash: String, window: &mut Window, cx: &mut App) {
    let win = window.window_handle().window_id();
    let open = cx
        .try_global::<Instances>()
        .and_then(|i| i.0.get(&win))
        .and_then(|w| w.upgrade());
    match open {
        Some(panel) => panel.update(cx, |p, cx| {
            p.pending_select = Some(hash);
            if p.only.is_some() {
                p.set_only(None, cx);
            } else {
                p.apply_pending(cx);
            }
        }),
        None => {
            cx.default_global::<PendingSelect>().0.insert(win, hash);
        }
    }
    pide_core::toolwindow::toggle(PANEL_NAME, window, cx);
}

/// Show the Git Log limited to one ref (`refs/heads/main`), opening it if needed.
pub fn show_ref(full: String, window: &mut Window, cx: &mut App) {
    let win = window.window_handle().window_id();
    let open = cx
        .try_global::<Instances>()
        .and_then(|i| i.0.get(&win))
        .and_then(|w| w.upgrade());
    match open {
        Some(panel) => panel.update(cx, |p, cx| p.set_only(Some(full), cx)),
        None => {
            cx.default_global::<PendingFilter>().0.insert(win, full);
        }
    }
    pide_core::toolwindow::toggle(PANEL_NAME, window, cx);
}

/// What one read produced, with the graph laid out (both off the UI thread).
struct Loaded {
    log: Log,
    graph: Vec<GraphRow>,
}

pub struct GitLogPanel {
    root: PathBuf,
    loaded: Option<Rc<Loaded>>,
    /// Finished at least one read (so "not a repository" can be told apart from "loading").
    read_once: bool,
    error: Option<String>,
    /// Full ref name the log is limited to; `None` = all branches.
    only: Option<String>,
    /// A commit to select once the history is loaded (`show_commit`).
    pending_select: Option<String>,
    /// Refs and HEAD at the last read: a change means reload.
    fingerprint: Option<Vec<(String, String)>>,
    search: Entity<InputState>,
    /// Indices into `log.commits` shown (all, or the search's matches).
    visible: Rc<Vec<usize>>,
    selected: Option<gix::ObjectId>,
    files: Option<Result<Vec<ChangedFile>, String>>,
    /// pi's reasons kept with the selected commit (collaborator mode), once read.
    pi_notes: Option<pide_core::commit_notes::CommitNotes>,
    sessions: Entity<pide_core::Sessions>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _load: Option<Task<()>>,
    _files: Option<Task<()>>,
    _poll: Task<()>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for GitLogPanel {}

impl GitLogPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = Services::of(window, cx).workspace;
        let sessions = Services::of(window, cx).sessions;
        let root = workspace.read(cx).root().to_path_buf();
        let win = window.window_handle().window_id();
        let me = cx.weak_entity();
        cx.default_global::<Instances>().0.insert(win, me);
        let only = cx.default_global::<PendingFilter>().0.remove(&win);
        let pending_select = cx.default_global::<PendingSelect>().0.remove(&win);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Text, hash or author"));
        let subs = vec![cx.subscribe(&search, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.apply_search(cx);
            }
        })];
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                if this.update(cx, |this, cx| this.check_refs(cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            root,
            loaded: None,
            read_once: false,
            error: None,
            only,
            pending_select,
            fingerprint: None,
            search,
            visible: Rc::new(Vec::new()),
            selected: None,
            files: None,
            pi_notes: None,
            sessions,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _load: None,
            _files: None,
            _poll: poll,
            _subs: subs,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let only = self.only.clone();
        let work = cx.background_executor().spawn(async move {
            let fp = log::fingerprint(&root);
            let log = log::read(&root, only.as_deref()).map(|log| {
                log.map(|log| {
                    let graph = log::graph(&log.commits);
                    Loaded { log, graph }
                })
            });
            (log, fp)
        });
        self._load = Some(cx.spawn(async move |this, cx| {
            let (result, fp) = work.await;
            let _ = this.update(cx, |this, cx| {
                this.read_once = true;
                this.fingerprint = fp;
                match result {
                    Ok(loaded) => {
                        this.error = None;
                        this.loaded = loaded.map(Rc::new);
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                // Keep the selection when the commit is still there, else the newest.
                let keep = this.selected.filter(|id| {
                    this.loaded
                        .as_ref()
                        .is_some_and(|l| l.log.commits.iter().any(|c| c.id == *id))
                });
                let first = this
                    .loaded
                    .as_ref()
                    .and_then(|l| l.log.commits.first().map(|c| c.id));
                this.apply_search(cx);
                if this.apply_pending(cx) {
                    cx.notify();
                    return;
                }
                match keep.or(first) {
                    Some(id) if keep.is_none() || this.files.is_none() => this.select(id, cx),
                    Some(_) => {}
                    None => {
                        this.selected = None;
                        this.files = None;
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Select the commit `show_commit` asked for, if it's loaded.
    fn apply_pending(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(hash) = self.pending_select.clone() else {
            return false;
        };
        let Some(loaded) = self.loaded.clone() else {
            return false;
        };
        self.pending_select = None;
        let Some(i) = loaded
            .log
            .commits
            .iter()
            .position(|c| c.id.to_string().starts_with(&hash))
        else {
            return false;
        };
        let id = loaded.log.commits[i].id;
        self.select(id, cx);
        if let Some(pos) = self.visible.iter().position(|&v| v == i) {
            self.scroll.scroll_to_item(pos, ScrollStrategy::Center);
        }
        true
    }

    /// Reload when a branch, tag or HEAD moved since the last read.
    fn check_refs(&mut self, cx: &mut Context<Self>) {
        if self._load.is_some() && !self.read_once {
            return;
        }
        let root = self.root.clone();
        let known = self.fingerprint.clone();
        let check = cx
            .background_executor()
            .spawn(async move { log::fingerprint(&root) != known });
        cx.spawn(async move |this, cx| {
            if check.await {
                let _ = this.update(cx, |this, cx| this.reload(cx));
            }
        })
        .detach();
    }

    fn set_only(&mut self, only: Option<String>, cx: &mut Context<Self>) {
        if self.only != only {
            self.only = only;
            self.reload(cx);
            cx.notify();
        }
    }

    fn apply_search(&mut self, cx: &mut Context<Self>) {
        let q = self.search.read(cx).value().trim().to_lowercase();
        let visible: Vec<usize> = match &self.loaded {
            None => Vec::new(),
            Some(l) if q.is_empty() => (0..l.log.commits.len()).collect(),
            Some(l) => l
                .log
                .commits
                .iter()
                .enumerate()
                .filter(|(_, c)| matches(c, &q))
                .map(|(i, _)| i)
                .collect(),
        };
        self.visible = Rc::new(visible);
        cx.notify();
    }

    fn commit(&self, id: gix::ObjectId) -> Option<&Commit> {
        self.loaded
            .as_ref()?
            .log
            .commits
            .iter()
            .find(|c| c.id == id)
    }

    fn select(&mut self, id: gix::ObjectId, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.files = None;
        cx.notify();
        let (Some(loaded), Some(commit)) = (self.loaded.clone(), self.commit(id).cloned()) else {
            return;
        };
        let workdir = loaded.log.workdir.clone();
        self.pi_notes = None;
        let (root, hash) = (self.root.clone(), id.to_string());
        let work = cx.background_executor().spawn(async move {
            (
                log::changed_files(&workdir, &commit),
                pide_core::commit_notes::load(&root, &hash),
            )
        });
        self._files = Some(cx.spawn(async move |this, cx| {
            let (files, notes) = work.await;
            let files = files.map_err(|e| format!("{e:#}"));
            let _ = this.update(cx, |this, cx| {
                if this.selected == Some(id) {
                    this.files = Some(files);
                    this.pi_notes = notes;
                    cx.notify();
                }
            });
        }));
    }

    /// Up / down move the selection through the visible commits.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded.clone() else { return };
        let pos = self
            .selected
            .and_then(|id| {
                self.visible
                    .iter()
                    .position(|&i| loaded.log.commits[i].id == id)
            })
            .map_or(0, |p| p as isize + delta);
        let pos = pos.clamp(0, self.visible.len() as isize - 1);
        if let Some(&i) = self.visible.get(pos as usize) {
            self.select(loaded.log.commits[i].id, cx);
            self.scroll
                .scroll_to_item(pos as usize, ScrollStrategy::Nearest);
        }
    }

    /// The file's change in the selected commit, in a diff tab.
    fn show_file_diff(&self, file: &ChangedFile, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(loaded), Some(commit)) = (
            self.loaded.clone(),
            self.selected.and_then(|id| self.commit(id)).cloned(),
        ) else {
            return;
        };
        let workdir = loaded.log.workdir.clone();
        let file = file.clone();
        let work = cx.background_executor().spawn(async move {
            let id = commit.id.to_string();
            let before = commit.parents.first().and_then(|p| {
                let old = file.old_path.as_deref().unwrap_or(&file.path);
                log::file_at(&workdir, &p.to_string(), old)
            });
            let after = if file.kind == 'D' {
                Some(String::new())
            } else {
                log::file_at(&workdir, &id, &file.path)
            };
            (workdir.join(&file.path), before, after, commit)
        });
        window
            .spawn(cx, async move |cx| {
                let (path, before, after, commit) = work.await;
                let Some(after) = after else {
                    return; // binary, or unreadable
                };
                let short = commit.short();
                let _ = cx.update(|window, cx| {
                    pide_core::opener::open_diff(
                        pide_core::opener::DiffRequest {
                            path,
                            before,
                            after: Some(after),
                            label: short.clone().into(),
                            before_is: match commit.parents.first() {
                                Some(p) => format!("parent {}", &p.to_string()[..7]).into(),
                                None => "nothing (first commit)".into(),
                            },
                        },
                        window,
                        cx,
                    )
                });
            })
            .detach();
    }

    fn explain(&self, window: &mut Window, cx: &mut App) {
        let Some(c) = self.selected.and_then(|id| self.commit(id)) else {
            return;
        };
        let text = format!(
            "Explain commit {} (\u{201c}{}\u{201d}): what it changes and why. Run `git show {}` to read it.",
            c.short(),
            c.subject,
            c.short()
        );
        pide_core::opener::ask_agent(text, window, cx);
    }

    fn render_branches(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut col = v_flex().py_1();
        let item = |id: SharedString,
                    label: String,
                    hint: String,
                    full: Option<String>,
                    on: bool,
                    cx: &mut Context<Self>| {
            h_flex()
                .id(id)
                .px_3()
                .py_0p5()
                .gap_2()
                .text_sm()
                .cursor_pointer()
                .when(on, |d| d.bg(theme.list_active))
                .hover(|d| d.bg(theme.list_hover))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(label),
                )
                .child(div().text_xs().text_color(theme.muted_foreground).child(hint))
                .on_click(cx.listener(move |this, _, _, cx| this.set_only(full.clone(), cx)))
        };
        let heading = |text: &str| {
            div()
                .px_3()
                .pt_2()
                .pb_0p5()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(text.to_string())
        };
        col = col.child(item(
            "log-all".into(),
            "All branches".into(),
            String::new(),
            None,
            self.only.is_none(),
            cx,
        ));
        let Some(loaded) = self.loaded.clone() else {
            return col;
        };
        let b = &loaded.log.branches;
        for (title, list, prefix) in [
            ("LOCAL", &b.local, "l"),
            ("REMOTE", &b.remote, "r"),
            ("TAGS", &b.tags, "t"),
        ] {
            if list.is_empty() {
                continue;
            }
            col = col.child(heading(title));
            for br in list.iter() {
                let current = prefix == "l" && b.head.as_deref() == Some(br.name.as_str());
                let label = if current {
                    format!("★ {}", br.name)
                } else {
                    br.name.clone()
                };
                let on = self.only.as_deref() == Some(br.full.as_str());
                col = col.child(item(
                    SharedString::from(format!("log-{prefix}-{}", br.full)),
                    label,
                    if current { "HEAD".into() } else { String::new() },
                    Some(br.full.clone()),
                    on,
                    cx,
                ));
            }
        }
        col
    }

    fn render_details(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(c) = self.selected.and_then(|id| self.commit(id)).cloned() else {
            return div()
                .p_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Select a commit.")
                .into_any_element();
        };
        let mono = theme.mono_font_family.clone();
        let hash = c.id.to_string();
        let files: AnyElement = match &self.files {
            None => div()
                .px_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Reading changes…")
                .into_any_element(),
            Some(Err(e)) => div()
                .px_3()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element(),
            Some(Ok(files)) => v_flex()
                .child(
                    div()
                        .px_3()
                        .pt_2()
                        .pb_1()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{} FILE{} CHANGED{}",
                            files.len(),
                            if files.len() == 1 { "" } else { "S" },
                            if c.parents.len() > 1 { " (vs first parent)" } else { "" }
                        )),
                )
                .children(files.iter().enumerate().map(|(i, f)| {
                    let color = pide_core::colors::change(pide_core::colors::Change::from_letter(f.kind), cx);
                    let (dir, name) = match f.path.rsplit_once('/') {
                        Some((d, n)) => (d.to_string(), n.to_string()),
                        None => (String::new(), f.path.clone()),
                    };
                    let file = f.clone();
                    h_flex()
                        .id(("log-file", i))
                        .px_3()
                        .py_0p5()
                        .gap_2()
                        .text_sm()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.list_hover))
                        .child(
                            div()
                                .w(px(12.))
                                .flex_none()
                                .text_color(color)
                                .child(f.kind.to_string()),
                        )
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1p5()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(name)
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .text_ellipsis()
                                        .child(dir),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.success)
                                .child(f.added.map_or("bin".into(), |n| format!("+{n}"))),
                        )
                        .when_some(f.deleted, |d, n| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.danger)
                                    .child(format!("−{n}")),
                            )
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.show_file_diff(&file, window, cx)
                        }))
                }))
                .into_any_element(),
        };
        v_flex()
            .child(
                v_flex()
                    .p_3()
                    .gap_1p5()
                    .border_b_1()
                    .border_color(theme.border)
                    // Body text, like the rest of the panel: a commit message is
                    // content, not a heading.
                    .child(div().child(c.subject.clone()))
                    .when(!c.body.is_empty(), |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .whitespace_normal()
                                .child(c.body.clone()),
                        )
                    })
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_x_1p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(div().font_family(mono).child(c.short()))
                            .child("·")
                            .child(format!("{} <{}>", c.author, c.email))
                            .child("·")
                            .child(log::when_full(c.time, c.offset)),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .pt_1()
                            .child(
                                Button::new("log-explain")
                                    .small()
                                    .label("Ask pi: explain")
                                    .tooltip("Draft a question about this commit in the pi box")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.explain(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("log-copy")
                                    .small()
                                    .ghost()
                                    .label("Copy Hash")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            hash.clone(),
                                        ))
                                    }),
                            ),
                    ),
            )
            .children(self.render_pi_notes(cx))
            .child(files)
            .into_any_element()
    }

    /// Why pi changed this commit's files, as pi explained them when it was made.
    fn render_pi_notes(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notes = self.pi_notes.clone().filter(|n| !n.is_empty())?;
        let theme = cx.theme().clone();
        let color = pide_core::colors::pi_note(cx);
        Some(
            v_flex()
                .px_3()
                .py_2()
                .gap_1()
                .border_b_1()
                .border_color(theme.border)
                .bg(color.opacity(0.06))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child("WHY, PER PI"),
                )
                .children(notes.files.into_iter().enumerate().map(|(i, f)| {
                    let session = f.session.clone();
                    let (file, name) = (f.session_file.clone(), f.session.clone());
                    let sessions = self.sessions.clone();
                    v_flex()
                        .gap_0p5()
                        .text_sm()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().font_family(theme.mono_font_family.clone()).text_xs().child(f.path.clone()))
                                .child(
                                    div()
                                        .id(("pi-session", i))
                                        .text_xs()
                                        .text_color(color)
                                        .cursor_pointer()
                                        .hover(|d| d.underline())
                                        .child(format!("from \u{201c}{session}\u{201d}"))
                                        .when_some(file, |d, file| {
                                            d.on_click(move |_, window, cx| {
                                                let (file, name) = (file.clone(), name.clone());
                                                sessions.update(cx, |s, cx| {
                                                    s.add(Some(file), Some(name), cx);
                                                });
                                                pide_core::CommandRegistry::run("agent.ask", window, cx);
                                            })
                                        }),
                                ),
                        )
                        .when_some(f.summary.clone(), |d, s| d.child(div().child(s)))
                        .children(f.notes.iter().map(|(line, note)| {
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("line {line}: {note}"))
                        }))
                }))
                .into_any_element(),
        )
    }
}

fn matches(c: &Commit, q: &str) -> bool {
    c.subject.to_lowercase().contains(q)
        || c.body.to_lowercase().contains(q)
        || c.author.to_lowercase().contains(q)
        || c.email.to_lowercase().contains(q)
        || c.id.to_string().starts_with(q)
        || c.refs.iter().any(|r| r.name.to_lowercase().contains(q))
}

/// Paint one row's slice of the graph: lane lines and the commit's dot.
fn paint_graph(
    bounds: Bounds<Pixels>,
    row: &GraphRow,
    head: bool,
    colors: &[Hsla],
    background: Hsla,
    window: &mut Window,
) {
    let x = |lane: usize| bounds.origin.x + px(8. + lane as f32 * LANE_W);
    let top = bounds.origin.y;
    let mid = bounds.origin.y + bounds.size.height / 2.;
    let bottom = bounds.origin.y + bounds.size.height;
    let color = |lane: usize| colors[lane % colors.len()];
    let line = |window: &mut Window, from: Point<Pixels>, to: Point<Pixels>, c: Hsla| {
        let mut p = PathBuilder::stroke(px(1.5));
        p.move_to(from);
        p.line_to(to);
        if let Ok(p) = p.build() {
            window.paint_path(p, c);
        }
    };
    for &(a, b) in &row.top {
        if a.max(b) < MAX_LANES {
            line(window, point(x(a), top), point(x(b), mid), color(a));
        }
    }
    for &(a, b) in &row.bottom {
        if a.max(b) < MAX_LANES {
            line(window, point(x(a), mid), point(x(b), bottom), color(b));
        }
    }
    if row.node < MAX_LANES {
        let r = px(4.);
        let dot = Bounds::new(point(x(row.node) - r, mid - r), size(r * 2., r * 2.));
        window.paint_quad(fill(dot, color(row.node)).corner_radii(r));
        if head {
            let inner = px(2.);
            let hole = Bounds::new(
                point(x(row.node) - inner, mid - inner),
                size(inner * 2., inner * 2.),
            );
            window.paint_quad(fill(hole, background).corner_radii(inner));
        }
    }
}

impl Focusable for GitLogPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for GitLogPanel {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for GitLogPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Git Log"
    }
}

impl Panel for GitLogPanel {
    fn title_text(&self, _: &App) -> SharedString {
        "Git Log".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Bottom
    }
}

impl Render for GitLogPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let root = v_flex().size_full().track_focus(&self.focus);
        let Some(loaded) = self.loaded.clone() else {
            let text = match (&self.error, self.read_once) {
                (Some(e), _) => e.clone(),
                (None, false) => "Reading history…".into(),
                (None, true) => "No history: this project isn't in a git repository, or it has no commits yet.".into(),
            };
            return root.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(text),
            );
        };
        let searching = !self.search.read(cx).value().trim().is_empty();
        let visible = self.visible.clone();
        let selected = self.selected;
        let lanes = if searching {
            0
        } else {
            loaded
                .graph
                .iter()
                .map(|r| r.width)
                .max()
                .unwrap_or(1)
                .min(MAX_LANES)
        };
        let graph_w = if searching {
            px(4.)
        } else {
            px(16. + lanes.saturating_sub(1) as f32 * LANE_W)
        };
        let colors: Rc<Vec<Hsla>> = Rc::new(vec![
            theme.blue,
            theme.yellow,
            theme.magenta,
            theme.green,
            theme.cyan,
            theme.red,
        ]);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let view = cx.entity().downgrade();
        let focus = self.focus.clone();
        let list = uniform_list("git-log-rows", visible.len(), {
            let loaded = loaded.clone();
            let theme = theme.clone();
            move |range, _, _| {
                range
                    .map(|pos| {
                        let i = visible[pos];
                        let c = &loaded.log.commits[i];
                        let row = loaded.graph[i].clone();
                        let head = c.refs.iter().any(|r| r.kind == RefKind::Head);
                        let colors = colors.clone();
                        let bg = theme.background;
                        let id = c.id;
                        let view = view.clone();
                        let focus = focus.clone();
                        let on = selected == Some(id);
                        h_flex()
                            .id(("log-row", pos))
                            .h(px(ROW_H))
                            .pr_2()
                            .gap_2()
                            .text_sm()
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .when(on, |d| d.bg(theme.list_active))
                            .hover(|d| d.bg(theme.list_hover))
                            .child(
                                div().w(graph_w).h_full().flex_none().when(!searching, |d| {
                                    d.child(
                                        canvas(
                                            |_, _, _| {},
                                            move |bounds, _, window, _| {
                                                paint_graph(bounds, &row, head, &colors, bg, window)
                                            },
                                        )
                                        .size_full(),
                                    )
                                }),
                            )
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .overflow_hidden()
                                    .children(c.refs.iter().map(|r| {
                                        let (fg, bgc) = match r.kind {
                                            RefKind::Head => (theme.yellow, theme.yellow.opacity(0.15)),
                                            RefKind::Local => (theme.green, theme.green.opacity(0.15)),
                                            RefKind::Remote => {
                                                (theme.magenta, theme.magenta.opacity(0.15))
                                            }
                                            RefKind::Tag => (theme.cyan, theme.cyan.opacity(0.15)),
                                        };
                                        div()
                                            .flex_none()
                                            .px_1p5()
                                            .rounded(px(4.))
                                            .text_xs()
                                            .text_color(fg)
                                            .bg(bgc)
                                            .child(r.name.clone())
                                    }))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(c.subject.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .w(relative(0.16))
                                    .max_w(px(140.))
                                    .flex_none()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(theme.muted_foreground)
                                    .child(c.author.clone()),
                            )
                            .child(
                                div()
                                    .w(relative(0.17))
                                    .max_w(px(120.))
                                    .flex_none()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(theme.muted_foreground)
                                    .child(log::when(c.time, c.offset, now)),
                            )
                            .on_click(move |_, window, cx| {
                                focus.focus(window, cx); // ↑ / ↓ move from here
                                let _ = view.update(cx, |this, cx| this.select(id, cx));
                            })
                    })
                    .collect()
            }
        })
        .track_scroll(&self.scroll)
        .size_full();

        let count = if searching {
            format!("{} of {}", self.visible.len(), loaded.log.commits.len())
        } else if loaded.log.truncated {
            format!("latest {}", loaded.log.commits.len())
        } else {
            format!("{} commits", loaded.log.commits.len())
        };
        root.on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
            match ev.keystroke.key.as_str() {
                "down" => this.step(1, cx),
                "up" => this.step(-1, cx),
                _ => return,
            }
            cx.stop_propagation();
        }))
        .child(
            h_flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("git-log-branches")
                        .w(relative(0.16))
                        .min_w(px(140.))
                        .max_w(px(240.))
                        .flex_none()
                        .h_full()
                        .overflow_y_scroll()
                        .border_r_1()
                        .border_color(theme.border)
                        .child(self.render_branches(cx)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .h_full()
                        .child(
                            h_flex()
                                .px_2()
                                .py_1()
                                .gap_2()
                                .border_b_1()
                                .border_color(theme.border)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(80.))
                                        .max_w(px(280.))
                                        .child(Input::new(&self.search).small()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .whitespace_nowrap()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(count),
                                )
                                .child(div().flex_1())
                                .child(
                                    Button::new("log-refresh")
                                        .xsmall()
                                        .ghost()
                                        .label("Refresh")
                                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                                ),
                        )
                        .when_some(self.error.clone(), |d, e| {
                            d.child(div().px_2().text_xs().text_color(theme.danger).child(e))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .child(list),
                        ),
                )
                .child(
                    div()
                        .id("git-log-details")
                        .w(relative(0.3))
                        .min_w(px(240.))
                        .max_w(px(460.))
                        .flex_none()
                        .h_full()
                        .overflow_y_scroll()
                        .border_l_1()
                        .border_color(theme.border)
                        .child(self.render_details(cx)),
                ),
        )
    }
}
