//! Git in the status bar, JetBrains style: the branch (click → the Git popup:
//! Update Project, Push, Fetch, New Branch, and every branch with its actions),
//! ↓n to pull and ↑n to push. Push… lists the outgoing commits first.
//! Commands: `git.pull` (Cmd+T), `git.push` (Cmd+Shift+K), `git.fetch`, `git.branches`.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{CommandRegistry, CommandSpec, Services};

use crate::log::{self, ChangedFile};
use crate::remote::{self, BranchInfo, Outgoing};

pub fn init(cx: &mut App) {
    CommandRegistry::register(
        cx,
        CommandSpec::new("git.pull", "Git: Update Project (Pull)").key("cmd-t"),
        |window, cx| {
            run_op("Updating…", remote::pull, window, cx);
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("git.push", "Git: Push…").key("cmd-shift-k"),
        open_push,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("git.fetch", "Git: Fetch"),
        |window, cx| {
            run_op("Fetching…", remote::fetch, window, cx);
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("git.branches", "Git: Branches…"),
        open_branches,
    );
}

fn root(window: &Window, cx: &App) -> PathBuf {
    Services::of(window, cx)
        .workspace
        .read(cx)
        .root()
        .to_path_buf()
}

/// Run a git operation off the UI thread; its summary or error becomes a notification.
fn run_op(
    start: &'static str,
    op: impl FnOnce(&std::path::Path) -> anyhow::Result<String> + Send + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let root = root(window, cx);
    if !start.is_empty() {
        window.push_notification(Notification::info(start), cx);
    }
    let task = cx.background_executor().spawn(async move { op(&root) });
    window
        .spawn(cx, async move |cx| {
            let result = task.await;
            let _ = cx.update(|window, cx| match result {
                Ok(msg) => window.push_notification(Notification::success(msg), cx),
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            });
        })
        .detach();
}

/// The status bar part: `⎇ main  ↓2 ↑1`.
pub struct GitWidget {
    branch: Option<String>,
    ahead_behind: Option<(u32, u32)>,
    _poll: Task<()>,
}

impl GitWidget {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = root(window, cx);
        let poll = cx.spawn(async move |this, cx| {
            loop {
                let r = root.clone();
                let (branch, ab) = cx
                    .background_executor()
                    .spawn(async move {
                        let b = pide_core::git::branch(&r);
                        let ab = b.as_ref().and_then(|_| crate::repo::ahead_behind(&r));
                        (b, ab)
                    })
                    .await;
                let alive = this.update(cx, |this, cx| {
                    if this.branch != branch || this.ahead_behind != ab {
                        this.branch = branch;
                        this.ahead_behind = ab;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(3)).await;
            }
        });
        Self {
            branch: None,
            ahead_behind: None,
            _poll: poll,
        }
    }
}

impl Render for GitWidget {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(branch) = self.branch.clone() else {
            return h_flex();
        };
        let (ahead, behind) = self.ahead_behind.unwrap_or((0, 0));
        h_flex()
            .gap_2()
            .child(
                div()
                    .id("git-branch")
                    .cursor_pointer()
                    .hover(|d| d.text_color(theme.foreground))
                    .child(format!("⎇ {branch}"))
                    .on_click(|_, window, cx| {
                        CommandRegistry::run("git.branches", window, cx);
                    }),
            )
            .when(behind > 0, |d| {
                d.child(
                    div()
                        .id("git-pull")
                        .cursor_pointer()
                        .text_color(theme.blue)
                        .child(format!("↓{behind}"))
                        .on_click(|_, window, cx| {
                            CommandRegistry::run("git.pull", window, cx);
                        }),
                )
            })
            .when(ahead > 0, |d| {
                d.child(
                    div()
                        .id("git-push")
                        .cursor_pointer()
                        .text_color(theme.green)
                        .child(format!("↑{ahead}"))
                        .on_click(|_, window, cx| {
                            CommandRegistry::run("git.push", window, cx);
                        }),
                )
            })
    }
}

// ---------------------------------------------------------------------------
// The Git popup: actions, then branches.

enum Pick {
    Pull,
    Push,
    Fetch,
    NewBranch,
    Branch(BranchInfo),
}

fn open_branches(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let root = root(window, cx);
    let task = cx
        .background_executor()
        .spawn(async move { remote::branches(&root) });
    window
        .spawn(cx, async move |cx| {
            let list = task.await;
            let _ = cx.update(|window, cx| match list {
                Ok(list) => show_branches(list, window, cx),
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            });
        })
        .detach();
}

fn show_branches(list: Vec<BranchInfo>, window: &mut Window, cx: &mut App) {
    let current = list.iter().find(|b| b.current).map(|b| b.name.clone());
    let key = |id: &str| pide_core::keymap::keys_for(cx, id).unwrap_or_default();
    let picks: Rc<Vec<(String, String, Pick)>> = Rc::new(
        [
            ("Update Project (pull)…", key("git.pull"), Pick::Pull),
            ("Push…", key("git.push"), Pick::Push),
            ("Fetch", String::new(), Pick::Fetch),
            ("New Branch…", String::new(), Pick::NewBranch),
        ]
        .into_iter()
        .map(|(a, b, p)| (a.to_string(), b, p))
        .chain(list.into_iter().map(|b| {
            let mut hint = Vec::new();
            if b.ahead > 0 {
                hint.push(format!("↑{}", b.ahead));
            }
            if b.behind > 0 {
                hint.push(format!("↓{}", b.behind));
            }
            if b.remote {
                hint.push("remote".into());
            }
            let label = if b.current {
                format!("★ {}", b.name)
            } else {
                format!("   {}", b.name)
            };
            (label, hint.join(" "), Pick::Branch(b))
        }))
        .collect(),
    );
    let query = Rc::new(RefCell::new(String::new()));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(Cell::new(true));
    window.open_dialog(cx, move |dialog, _, _| {
        let (picks, query, state, focus_once, current) = (
            picks.clone(),
            query.clone(),
            state.clone(),
            focus_once.clone(),
            current.clone(),
        );
        dialog
            .text_sm()
            .close_button(false)
            .p_0()
            .w(px(420.))
            .content(move |content, window, cx| {
                if focus_once.replace(false) {
                    let state = state.clone();
                    window.defer(cx, move |window, cx| {
                        state.read(cx).focus_handle(cx).focus(window, cx)
                    });
                }
                let q = query.borrow().to_lowercase();
                let order: Vec<usize> = (0..picks.len())
                    .filter(|&i| q.is_empty() || picks[i].0.to_lowercase().contains(&q))
                    .collect();
                let muted = cx.theme().muted_foreground;
                let rows: Vec<CommandItem> = order
                    .iter()
                    .map(|&i| {
                        let (label, hint) = (picks[i].0.clone(), picks[i].1.clone());
                        CommandItem::new().label(label.clone()).child(move |_, _| {
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .child(label.clone()),
                                )
                                .child(div().text_xs().text_color(muted).child(hint.clone()))
                        })
                    })
                    .collect();
                let (query_q, picks_c, order_c, current_c) =
                    (query.clone(), picks.clone(), order.clone(), current.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder("Search branches and actions")
                        .max_h(px(460.))
                        .filterable(false)
                        .items(rows)
                        .on_query(move |q, window, _| {
                            *query_q.borrow_mut() = q.to_string();
                            window.refresh();
                        })
                        .on_confirm(move |ix, window, cx| {
                            let Some(&i) = order_c.get(ix.row) else {
                                return;
                            };
                            window.close_dialog(cx);
                            let picks = picks_c.clone();
                            let current = current_c.clone();
                            window.defer(cx, move |window, cx| match &picks[i].2 {
                                Pick::Pull => {
                                    CommandRegistry::run("git.pull", window, cx);
                                }
                                Pick::Push => {
                                    CommandRegistry::run("git.push", window, cx);
                                }
                                Pick::Fetch => {
                                    CommandRegistry::run("git.fetch", window, cx);
                                }
                                Pick::NewBranch => new_branch_from(
                                    current.unwrap_or_else(|| "HEAD".into()),
                                    window,
                                    cx,
                                ),
                                Pick::Branch(b) => branch_actions(b.clone(), current, window, cx),
                            });
                        }),
                )
            })
    });
}

fn new_branch_from(from: String, window: &mut Window, cx: &mut App) {
    let title = format!("New branch from \u{2018}{from}\u{2019}");
    prompt_name(
        title,
        String::new(),
        Rc::new(move |name, window, cx| {
            if name.is_empty() {
                return;
            }
            let from = from.clone();
            run_op("", move |w| remote::new_branch(w, &name, &from), window, cx);
        }),
        window,
        cx,
    );
}

/// What can be done with one branch (a second popup, like JetBrains' submenu).
fn branch_actions(b: BranchInfo, current: Option<String>, window: &mut Window, cx: &mut App) {
    let cur = current.clone().unwrap_or_else(|| "HEAD".into());
    let q = |s: &str| format!("\u{2018}{s}\u{2019}");
    let mut actions: Vec<(String, &'static str)> = Vec::new();
    if b.current {
        actions.push(("Update Project (pull)…".into(), "pull"));
        actions.push(("Push…".into(), "push"));
        actions.push((format!("New Branch from {}…", q(&b.name)), "new"));
        actions.push(("Show in Git Log".into(), "log"));
        actions.push(("Rename…".into(), "rename"));
    } else {
        actions.push((
            if b.remote {
                "Checkout (as a local tracking branch)".into()
            } else {
                "Checkout".into()
            },
            "checkout",
        ));
        actions.push((format!("New Branch from {}…", q(&b.name)), "new"));
        actions.push(("Show in Git Log".into(), "log"));
        actions.push((format!("Merge {} into {}", q(&b.name), q(&cur)), "merge"));
        actions.push((format!("Rebase {} onto {}", q(&cur), q(&b.name)), "rebase"));
        if !b.remote {
            actions.push(("Rename…".into(), "rename"));
            actions.push(("Delete".into(), "delete"));
        }
    }
    let actions = Rc::new(actions);
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(Cell::new(true));
    window.open_dialog(cx, move |dialog, _, _| {
        let (actions, state, focus_once, b) = (
            actions.clone(),
            state.clone(),
            focus_once.clone(),
            b.clone(),
        );
        dialog
            .text_sm()
            .close_button(false)
            .p_0()
            .w(px(380.))
            .content(move |content, window, cx| {
                if focus_once.replace(false) {
                    let state = state.clone();
                    window.defer(cx, move |window, cx| {
                        state.read(cx).focus_handle(cx).focus(window, cx)
                    });
                }
                let rows: Vec<CommandItem> = actions
                    .iter()
                    .map(|(label, _)| CommandItem::new().label(label.clone()))
                    .collect();
                let (actions_c, b_c) = (actions.clone(), b.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder(format!("⎇ {}", b.name))
                        .max_h(px(420.))
                        .items(rows)
                        .on_confirm(move |ix, window, cx| {
                            let Some(what) = actions_c.get(ix.row).map(|a| a.1) else {
                                return;
                            };
                            window.close_dialog(cx);
                            let b = b_c.clone();
                            window.defer(cx, move |window, cx| branch_action(what, b, window, cx));
                        }),
                )
            })
    });
}

fn branch_action(what: &'static str, b: BranchInfo, window: &mut Window, cx: &mut App) {
    match what {
        "pull" => {
            CommandRegistry::run("git.pull", window, cx);
        }
        "push" => {
            CommandRegistry::run("git.push", window, cx);
        }
        "new" => new_branch_from(b.name, window, cx),
        "log" => {
            let full = if b.remote {
                format!("refs/remotes/{}", b.name)
            } else {
                format!("refs/heads/{}", b.name)
            };
            crate::log_panel::show_ref(full, window, cx);
        }
        "checkout" => run_op("", move |w| remote::checkout(w, &b), window, cx),
        "merge" => run_op("", move |w| remote::merge(w, &b.name), window, cx),
        "rebase" => run_op("", move |w| remote::rebase(w, &b.name), window, cx),
        "delete" => run_op("", move |w| remote::delete(w, &b.name), window, cx),
        "rename" => {
            let old = b.name.clone();
            prompt_name(
                format!("Rename \u{2018}{old}\u{2019}"),
                old.clone(),
                Rc::new(move |name, window, cx| {
                    if name.is_empty() || name == old {
                        return;
                    }
                    let old = old.clone();
                    run_op("", move |w| remote::rename(w, &old, &name), window, cx);
                }),
                window,
                cx,
            );
        }
        _ => {}
    }
}

/// Ask for a name; `on_ok` gets it (trimmed) when confirmed.
fn prompt_name(
    title: String,
    default: String,
    on_ok: Rc<dyn Fn(String, &mut Window, &mut App)>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }
    let input = cx.new(|cx| InputState::new(window, cx).default_value(default.clone()));
    let focus_once = Rc::new(Cell::new(true));
    let run = Rc::new({
        let input = input.clone();
        move |window: &mut Window, cx: &mut App| {
            let name = input.read(cx).value().trim().to_string();
            on_ok(name, window, cx);
        }
    });
    let enter = run.clone();
    let sub = window.subscribe(&input, cx, move |_, ev: &InputEvent, window, cx| {
        if let InputEvent::PressEnter { .. } = ev {
            window.close_dialog(cx);
            enter(window, cx);
        }
    });
    let sub = Rc::new(RefCell::new(Some(sub)));
    window.open_dialog(cx, move |dialog, window, cx| {
        let (input, run, sub) = (input.clone(), run.clone(), sub.clone());
        if focus_once.replace(false) {
            let input = input.clone();
            window.defer(cx, move |window, cx| {
                input.update(cx, |i, cx| {
                    i.focus(window, cx);
                    i.select_all(window, cx);
                })
            });
        }
        dialog
            .text_sm()
            .title(title.clone())
            .w(px(420.))
            .child(Input::new(&input))
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new()
                            .child(Button::new("name-cancel").label("Cancel").outline()),
                    )
                    .child(DialogAction::new().child(Button::new("name-ok").label("OK").primary())),
            )
            .on_ok(move |_, window, cx| {
                run(window, cx);
                true
            })
            .on_close(move |_, _, _| {
                sub.borrow_mut().take();
            })
    });
}

// ---------------------------------------------------------------------------
// Push…: the outgoing commits first.

struct PushView {
    root: PathBuf,
    out: Outgoing,
    selected: usize,
    files: Option<Result<Vec<ChangedFile>, String>>,
    _files: Option<Task<()>>,
}

impl PushView {
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        self.files = None;
        cx.notify();
        let Some(c) = self.out.commits.get(ix) else {
            return;
        };
        let (root, hash) = (self.root.clone(), c.hash.clone());
        let work = cx
            .background_executor()
            .spawn(async move { log::changed_files_rev(&root, &hash) });
        self._files = Some(cx.spawn(async move |this, cx| {
            let files = work.await.map_err(|e| format!("{e:#}"));
            let _ = this.update(cx, |this, cx| {
                if this.selected == ix {
                    this.files = Some(files);
                    cx.notify();
                }
            });
        }));
    }
}

impl Render for PushView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let out = &self.out;
        let commits: Vec<AnyElement> = out
            .commits
            .iter()
            .enumerate()
            .map(|(i, c)| {
                h_flex()
                    .id(("push-commit", i))
                    .pl_6()
                    .pr_2()
                    .py_0p5()
                    .gap_2()
                    .text_sm()
                    .cursor_pointer()
                    .when(i == self.selected, |d| d.bg(theme.list_active))
                    .hover(|d| d.bg(theme.list_hover))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(c.subject.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} · {}",
                                c.author,
                                log::when(c.time, c.offset, now)
                            )),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx)))
                    .into_any_element()
            })
            .collect();
        let files: AnyElement = match &self.files {
            _ if out.commits.is_empty() => div().into_any_element(),
            None => div()
                .p_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Reading changes…")
                .into_any_element(),
            Some(Err(e)) => div()
                .p_2()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element(),
            Some(Ok(list)) => v_flex()
                .py_1()
                .children(list.iter().map(|f| {
                    let color = pide_core::colors::change(
                        pide_core::colors::Change::from_letter(f.kind),
                        cx,
                    );
                    h_flex()
                        .px_2()
                        .gap_2()
                        .text_sm()
                        .child(div().w(px(12.)).text_color(color).child(f.kind.to_string()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(f.path.clone()),
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
                }))
                .into_any_element(),
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(px(6.))
                    .h(px(300.))
                    .overflow_hidden()
                    .child(
                        v_flex()
                            .id("push-commits")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .border_r_1()
                            .border_color(theme.border)
                            .child(
                                h_flex()
                                    .px_2()
                                    .py_1()
                                    .gap_1p5()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(out.branch.clone())
                                    .child(div().text_color(theme.muted_foreground).child("→"))
                                    .child(out.target.clone())
                                    .when(out.new_branch, |d| {
                                        d.child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.success)
                                                .child("new branch"),
                                        )
                                    })
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!(
                                                "{} commit{}",
                                                out.commits.len(),
                                                if out.commits.len() == 1 { "" } else { "s" }
                                            )),
                                    ),
                            )
                            .when(out.commits.is_empty(), |d| {
                                d.child(
                                    div()
                                        .px_6()
                                        .py_1()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("Nothing to push."),
                                )
                            })
                            .children(commits),
                    )
                    .child(
                        div()
                            .id("push-files")
                            .w(px(280.))
                            .flex_none()
                            .h_full()
                            .overflow_y_scroll()
                            .child(files),
                    ),
            )
            .when(out.behind > 0, |d| {
                d.child(
                    div()
                        .p_2()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(theme.warning)
                        .text_sm()
                        .child(format!(
                            "{} has {} commit{} you don't have: a push will be rejected. Update Project ({}) first, or Force Push.",
                            out.target,
                            out.behind,
                            if out.behind == 1 { "" } else { "s" },
                            pide_core::keymap::keys_for(cx, "git.pull").unwrap_or_else(|| "Git → Pull".into())
                        )),
                )
            })
    }
}

fn open_push(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let root = root(window, cx);
    let r = root.clone();
    let task = cx
        .background_executor()
        .spawn(async move { remote::outgoing(&r) });
    window
        .spawn(cx, async move |cx| {
            let out = task.await;
            let _ = cx.update(|window, cx| match out {
                Ok(out) => show_push(root, out, window, cx),
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            });
        })
        .detach();
}

fn show_push(root: PathBuf, out: Outgoing, window: &mut Window, cx: &mut App) {
    let empty = out.commits.is_empty();
    let title = format!("Push Commits to {}", out.target);
    let view = cx.new(|cx| {
        let mut v = PushView {
            root,
            out,
            selected: 0,
            files: None,
            _files: None,
        };
        v.select(0, cx);
        v
    });
    let push = Rc::new({
        let view = view.clone();
        move |force: bool, window: &mut Window, cx: &mut App| {
            let out = view.read(cx).out.clone();
            let label = if force {
                "Force pushing…"
            } else {
                "Pushing…"
            };
            run_op(label, move |w| remote::push(w, &out, force), window, cx);
        }
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let (view, push_ok, push_force) = (view.clone(), push.clone(), push.clone());
        dialog
            .text_sm()
            .title(title.clone())
            .w(px(760.))
            .child(view)
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new()
                            .child(Button::new("push-cancel").label("Cancel").outline()),
                    )
                    .child(
                        Button::new("push-force")
                            .label("Force Push")
                            .outline()
                            .tooltip("git push --force-with-lease: only if the remote is still where you last saw it")
                            .disabled(empty)
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                push_force(true, window, cx);
                            }),
                    )
                    .child(
                        DialogAction::new()
                            .child(Button::new("push-ok").label("Push").primary().disabled(empty)),
                    ),
            )
            .on_ok(move |_, window, cx| {
                if !empty {
                    push_ok(false, window, cx);
                }
                true
            })
    });
}
