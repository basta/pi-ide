//! Annotate (git blame) for a whole file, as a center tab: each line with who
//! last changed it and when, blocks shaded by age (newest strongest). Click a
//! block for its commit: Show in Git Log, the commit's diff of this file, or
//! ask pi why it changed. Also `git.why_line` for the editor's cursor line.
//!
//! JetBrains shows this in the editor's gutter; GPUI Kit's gutter only takes
//! colored marks, so it's a tab here (docs/GAPS.md).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelInfo, PanelState, panel_handle, register_panel};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{CommandRegistry, CommandSpec, DockPosition, Panel, Services, WorkspaceEvent};

use crate::blame::{self, Blame, BlameCommit};

pub const PANEL_NAME: &str = "git_annotate";
const ROW_H: f32 = 20.;

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |ctx, window, cx| {
        let path = match ctx.info() {
            PanelInfo::Panel(v) => v.get("path").and_then(|p| p.as_str()).map(PathBuf::from),
            _ => None,
        };
        panel_handle(cx.new(|cx| AnnotatePanel::new(path.unwrap_or_default(), None, window, cx)))
    });
    CommandRegistry::register(
        cx,
        CommandSpec::new("git.annotate", "Git: Annotate with Git Blame"),
        |window, cx| {
            let Some(ctx) = pide_core::context::current(window, cx) else {
                window.push_notification(Notification::info("Open a file to annotate it."), cx);
                return;
            };
            let root = root(window, cx);
            open(root.join(&ctx.path), Some(ctx.start.0), window, cx);
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("git.why_line", "Git: Why This Line? (pi)"),
        |window, cx| {
            let Some(ctx) = pide_core::context::current(window, cx) else {
                return;
            };
            let root = root(window, cx);
            let rel = ctx.path.display().to_string();
            let line = ctx.start.0;
            // 1. A note pi left at (or near) this line in an open session: show it in
            //    the note bar, where you can reply.
            if note_near(&root.join(&ctx.path), line as usize, window, cx) {
                return;
            }
            // 2. Committed: pi's reason kept with that commit. 3. Else ask pi.
            let task = cx.background_executor().spawn({
                let root = root.clone();
                async move {
                    let (c, bl) = blame::blame_line_at(&root, &rel, line)?;
                    let notes = (!c.uncommitted())
                        .then(|| pide_core::commit_notes::load(&root, &c.hash))
                        .flatten();
                    anyhow::Ok((c, bl.text, bl.orig_line, rel, notes))
                }
            });
            window
                .spawn(cx, async move |cx| {
                    let r = task.await;
                    let _ = cx.update(|window, cx| match r {
                        Ok((c, text, orig_line, rel, notes)) => {
                            let reason =
                                notes.and_then(|n| n.files.into_iter().find(|f| f.path == rel));
                            match reason {
                                // Notes carry the commit's line numbers: match on the line's number there.
                                Some(f) => show_reason(&c, &f, orig_line, window, cx),
                                None => ask_why(&c, &rel, line, &text, window, cx),
                            }
                        }
                        Err(e) => {
                            window.push_notification(Notification::error(format!("{e:#}")), cx)
                        }
                    });
                })
                .detach();
        },
    );
}

fn root(window: &Window, cx: &App) -> PathBuf {
    Services::of(window, cx)
        .workspace
        .read(cx)
        .root()
        .to_path_buf()
}

/// Lines away from a note that still count as "this line".
const NEAR: usize = 3;

/// Go to the nearest note on `path` within [`NEAR`] lines of `line` (1-based)
/// from an open session (the active one first). False if there is none.
fn note_near(path: &Path, line: usize, window: &mut Window, cx: &mut App) -> bool {
    let sessions = Services::of(window, cx).sessions;
    let (active, all) = {
        let s = sessions.read(cx);
        (s.active(), s.all().to_vec())
    };
    let mut agents = vec![active.clone()];
    agents.extend(all.into_iter().filter(|a| *a != active));
    for agent in agents {
        let stop = agent
            .read(cx)
            .stops()
            .into_iter()
            .filter(|s| s.path == path && s.line.abs_diff(line) <= NEAR)
            .min_by_key(|s| s.line.abs_diff(line));
        if let Some(stop) = stop {
            sessions.update(cx, |s, cx| s.activate(&agent, cx));
            pide_core::collab::go_to(&agent, stop, window, cx);
            return true;
        }
    }
    false
}

/// pi's reason for the commit that last changed the line, as kept with it.
fn show_reason(
    c: &BlameCommit,
    f: &pide_core::commit_notes::FileReason,
    line: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let mut text = f.summary.clone().unwrap_or_default();
    // The hunk note nearest the line (both in that commit's line numbers).
    if let Some((_, note)) = f.notes.iter().min_by_key(|(l, _)| l.abs_diff(line)) {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(note);
    }
    // Stays until closed: it's the answer to a question, not a status.
    window.push_notification(
        Notification::info(text)
            .title(format!(
                "{} · pi in \u{201c}{}\u{201d}",
                c.short(),
                f.session
            ))
            .autohide(false),
        cx,
    );
}

/// Draft "why did this line change?" in the pi box.
fn ask_why(c: &BlameCommit, rel: &str, line: u32, text: &str, window: &mut Window, cx: &mut App) {
    let msg = if c.uncommitted() {
        format!(
            "Line {line} of {rel} (`{}`) isn't committed yet. What is this change for?",
            text.trim()
        )
    } else {
        format!(
            "Why does line {line} of {rel} look like this: `{}`? It was last changed in commit {} \
             (\u{201c}{}\u{201d}, {}). Look at that commit and the history around it \
             (`git show {}`, `git log -L {line},{line}:{rel}`) and explain the intent.",
            text.trim(),
            c.short(),
            c.summary,
            c.author,
            c.short()
        )
    };
    pide_core::opener::ask_agent(msg, window, cx);
}

/// One Annotate tab per file and window.
#[derive(Default)]
struct Open(Vec<(WindowId, PathBuf, WeakEntity<AnnotatePanel>)>);
impl Global for Open {}

pub fn open(path: PathBuf, line: Option<u32>, window: &mut Window, cx: &mut App) {
    let win = window.window_handle().window_id();
    let existing = cx.try_global::<Open>().and_then(|o| {
        o.0.iter()
            .find(|(w, p, _)| *w == win && *p == path)
            .and_then(|(_, _, e)| e.upgrade())
    });
    let panel = match existing {
        Some(p) => {
            p.update(cx, |p, cx| {
                p.goto = line;
                p.reload(cx);
            });
            p
        }
        None => {
            let p = cx.new(|cx| AnnotatePanel::new(path.clone(), line, window, cx));
            let o = cx.default_global::<Open>();
            o.0.retain(|(_, _, e)| e.upgrade().is_some());
            o.0.push((win, path, p.downgrade()));
            p
        }
    };
    let id = panel.entity_id();
    pide_core::opener::open_panel(panel_handle(panel), id, window, cx);
}

pub struct AnnotatePanel {
    path: PathBuf,
    root: PathBuf,
    blame: Option<Result<Rc<Blame>, String>>,
    /// Commit index → age rank 0..1 (0 = newest), for shading.
    age: Rc<Vec<f32>>,
    /// The clicked line (0-based).
    selected: Option<usize>,
    /// 1-based line to scroll to once loaded.
    goto: Option<u32>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _load: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for AnnotatePanel {}

impl AnnotatePanel {
    fn new(path: PathBuf, goto: Option<u32>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = Services::of(window, cx).workspace;
        let root = workspace.read(cx).root().to_path_buf();
        let watched = path.clone();
        let subs = vec![
            cx.subscribe(&workspace, move |this, _, ev: &WorkspaceEvent, cx| {
                if let WorkspaceEvent::BufferSaved(b) = ev
                    && b.read(cx).path == watched
                {
                    this.reload(cx);
                }
            }),
        ];
        let mut this = Self {
            path,
            root,
            blame: None,
            age: Rc::new(Vec::new()),
            selected: None,
            goto,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _load: None,
            _subs: subs,
        };
        this.reload(cx);
        this
    }

    fn rel(&self) -> String {
        self.path
            .strip_prefix(&self.root)
            .unwrap_or(&self.path)
            .display()
            .to_string()
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let (root, rel) = (self.root.clone(), self.rel());
        let work = cx
            .background_executor()
            .spawn(async move { blame::blame(&root, &rel) });
        self._load = Some(cx.spawn(async move |this, cx| {
            let r = work.await;
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(b) => {
                        this.age = Rc::new(ages(&b));
                        this.blame = Some(Ok(Rc::new(b)));
                    }
                    Err(e) => this.blame = Some(Err(format!("{e:#}"))),
                }
                if let Some(line) = this.goto.take() {
                    let ix = line.saturating_sub(1) as usize;
                    this.selected = Some(ix);
                    this.scroll.scroll_to_item(ix, ScrollStrategy::Center);
                }
                cx.notify();
            });
        }));
    }

    fn selected_commit(&self) -> Option<(BlameCommit, usize, String)> {
        let Some(Ok(b)) = &self.blame else {
            return None;
        };
        let ix = self.selected?;
        let line = b.lines.get(ix)?;
        Some((b.commits[line.commit].clone(), ix, line.text.clone()))
    }

    /// This file's change in the selected commit, as a diff tab.
    fn show_diff(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((c, _, _)) = self.selected_commit() else {
            return;
        };
        if c.uncommitted() {
            return;
        }
        let root = self.root.clone();
        let path = self.path.clone();
        let work = cx.background_executor().spawn(async move {
            let before = c
                .previous
                .as_ref()
                .and_then(|p| crate::log::file_at(&root, p, &c.filename))
                .or_else(|| crate::log::file_at(&root, &format!("{}^", c.hash), &c.filename));
            let after = crate::log::file_at(&root, &c.hash, &c.filename);
            (before, after, c)
        });
        window
            .spawn(cx, async move |cx| {
                let (before, after, c) = work.await;
                let Some(after) = after else { return };
                let _ = cx.update(|window, cx| {
                    pide_core::opener::open_diff(
                        pide_core::opener::DiffRequest {
                            path,
                            before,
                            after: Some(after),
                            label: c.short().to_string().into(),
                            before_is: "the commit's parent".into(),
                        },
                        window,
                        cx,
                    )
                });
            })
            .detach();
    }
}

/// Newest commit 0.0 … oldest 1.0 (by time).
fn ages(b: &Blame) -> Vec<f32> {
    let mut order: Vec<usize> = (0..b.commits.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(b.commits[i].time));
    let mut age = vec![0.0; b.commits.len()];
    let n = order.len().max(2) - 1;
    for (rank, i) in order.into_iter().enumerate() {
        age[i] = rank as f32 / n as f32;
    }
    age
}

impl Focusable for AnnotatePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for AnnotatePanel {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut state = PanelState::new(PANEL_NAME);
        state.info = PanelInfo::panel(serde_json::json!({ "path": self.path }));
        state
    }
}

impl gpui_kit::component::dock::Panel for AnnotatePanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for AnnotatePanel {
    fn title_text(&self, _: &App) -> SharedString {
        let name = Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        format!("Annotate {name}").into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

impl Render for AnnotatePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let root = v_flex().size_full().track_focus(&self.focus);
        let b = match &self.blame {
            None => {
                return root.child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Reading git blame…"),
                );
            }
            Some(Err(e)) => {
                return root.child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(e.clone()),
                );
            }
            Some(Ok(b)) => b.clone(),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let header = self.selected_commit().map(|(c, ix, text)| {
            let rel = self.rel();
            let line = ix as u32 + 1;
            let (c2, c3) = (c.clone(), c.clone());
            v_flex()
                .px_3()
                .py_2()
                .gap_1()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(if c.uncommitted() {
                            "Not committed yet".to_string()
                        } else {
                            c.summary.clone()
                        }),
                )
                .when(!c.uncommitted(), |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} · {} · {}",
                                c.short(),
                                c.author,
                                crate::log::when_full(c.time, c.offset)
                            )),
                    )
                })
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("ann-why")
                                .small()
                                .label("Why this change? (pi)")
                                .on_click(move |_, window, cx| {
                                    ask_why(&c3, &rel, line, &text, window, cx)
                                }),
                        )
                        .when(!c.uncommitted(), |d| {
                            d.child(
                                Button::new("ann-log")
                                    .small()
                                    .ghost()
                                    .label("Show in Git Log")
                                    .on_click(move |_, window, cx| {
                                        let hash = c2.hash.clone();
                                        window.defer(cx, move |window, cx| {
                                            crate::log_panel::show_commit(hash, window, cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("ann-diff")
                                    .small()
                                    .ghost()
                                    .label("Diff")
                                    .tooltip("This file's change in that commit")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_diff(window, cx)
                                    })),
                            )
                        }),
                )
        });
        let selected_commit = self
            .selected
            .and_then(|ix| b.lines.get(ix))
            .map(|l| l.commit);
        let age = self.age.clone();
        let mono = theme.mono_font_family.clone();
        let view = cx.entity().downgrade();
        let digits = b.lines.len().to_string().len();
        let list = uniform_list("annotate-lines", b.lines.len(), {
            let b = b.clone();
            let theme = theme.clone();
            move |range, _, _| {
                range
                    .map(|ix| {
                        let line = &b.lines[ix];
                        let c = &b.commits[line.commit];
                        let first = ix == 0 || b.lines[ix - 1].commit != line.commit;
                        let on = selected_commit == Some(line.commit);
                        let shade = if c.uncommitted() {
                            theme.warning.opacity(0.18)
                        } else {
                            theme.blue.opacity(0.28 - 0.22 * age[line.commit])
                        };
                        let view = view.clone();
                        h_flex()
                            .h(px(ROW_H))
                            .text_sm()
                            .whitespace_nowrap()
                            .child(
                                h_flex()
                                    .id(("blame", ix))
                                    .w(px(200.))
                                    .h_full()
                                    .flex_none()
                                    .px_2()
                                    .gap_2()
                                    .text_xs()
                                    .cursor_pointer()
                                    .bg(if on { theme.list_active } else { shade })
                                    .text_color(theme.muted_foreground)
                                    .when(first, |d| {
                                        d.child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(if c.uncommitted() {
                                                    "not committed".to_string()
                                                } else {
                                                    c.author.clone()
                                                }),
                                        )
                                        .when(!c.uncommitted(), |d| {
                                            d.child(crate::log::when(c.time, c.offset, now))
                                        })
                                    })
                                    .on_click(move |_, _, cx| {
                                        let _ = view.update(cx, |this, cx| {
                                            this.selected = Some(ix);
                                            cx.notify();
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .w(px(10. + digits as f32 * 8.))
                                    .flex_none()
                                    .pr_2()
                                    .text_right()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child((ix + 1).to_string()),
                            )
                            .child(
                                div()
                                    .pl_2()
                                    .font_family(mono.clone())
                                    .child(line.text.clone()),
                            )
                    })
                    .collect()
            }
        })
        .track_scroll(&self.scroll)
        .size_full();
        root.children(header)
            .child(div().flex_1().min_h_0().child(list))
    }
}
