//! Git: Local Changes… (Cmd+9): the files `git status` lists; Enter opens one
//! in the diff tab against HEAD (where the ↶ buttons roll a change back).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, WindowExt as _, h_flex};
use gpui_kit::*;
use pide_core::Services;

/// One `git status --porcelain -z` entry: a one-letter state and the path.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub state: char,
    pub path: String,
}

/// Parse `git status --porcelain=v1 -z` output.
pub fn parse_status(out: &str) -> Vec<Change> {
    let mut items = out.split('\0').filter(|s| !s.is_empty());
    let mut changes = Vec::new();
    while let Some(entry) = items.next() {
        if entry.len() < 4 {
            continue;
        }
        let (xy, path) = entry.split_at(3);
        let xy: Vec<char> = xy.chars().take(2).collect();
        let state = match (xy[0], xy[1]) {
            ('?', '?') => 'U',
            ('R', _) | (_, 'R') => {
                items.next(); // the old name
                'R'
            }
            ('D', _) | (_, 'D') => 'D',
            ('A', _) => 'A',
            _ => 'M',
        };
        changes.push(Change {
            state,
            path: path.to_string(),
        });
    }
    changes
}

fn git_status(root: &Path) -> Result<Vec<Change>, String> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(parse_status(&String::from_utf8_lossy(&out.stdout)))
}

pub fn open(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let id = window.window_handle().window_id();
    let Some(root) =
        Services::for_window(id, cx).map(|s| s.workspace.read(cx).root().to_path_buf())
    else {
        return;
    };
    let r = root.clone();
    let task = cx
        .background_executor()
        .spawn(async move { git_status(&r) });
    window
        .spawn(cx, async move |cx| {
            let result = task.await;
            let _ = cx.update(|window, cx| match result {
                Err(e) => {
                    window.push_notification(Notification::warning(format!("git status: {e}")), cx)
                }
                Ok(list) if list.is_empty() => {
                    window.push_notification(Notification::info("No local changes."), cx)
                }
                Ok(list) => show(root, list, window, cx),
            });
        })
        .detach();
}

fn show(root: PathBuf, list: Vec<Change>, window: &mut Window, cx: &mut App) {
    let list = Rc::new(list);
    let query = Rc::new(RefCell::new(String::new()));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(std::cell::Cell::new(true));
    window.open_dialog(cx, move |dialog, _, _| {
        let (list, query, state, focus_once, root) = (
            list.clone(),
            query.clone(),
            state.clone(),
            focus_once.clone(),
            root.clone(),
        );
        dialog
            .close_button(false)
            .p_0()
            .content(move |content, window, cx| {
                if focus_once.replace(false) {
                    let state = state.clone();
                    window.defer(cx, move |window, cx| {
                        state.read(cx).focus_handle(cx).focus(window, cx)
                    });
                }
                let q = query.borrow().to_lowercase();
                let order: Vec<usize> = (0..list.len())
                    .filter(|&i| list[i].path.to_lowercase().contains(&q))
                    .collect();
                let theme = cx.theme().clone();
                let items: Vec<CommandItem> = order
                    .iter()
                    .map(|&i| {
                        let c = list[i].clone();
                        let color = match c.state {
                            'A' | 'U' => theme.success,
                            'D' => theme.danger,
                            _ => theme.warning,
                        };
                        CommandItem::new().label(c.path.clone()).child(move |_, _| {
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(14.))
                                        .flex_none()
                                        .text_color(color)
                                        .child(c.state.to_string()),
                                )
                                .child(c.path.clone())
                        })
                    })
                    .collect();
                let (query_q, list_c, order_c, root_c) =
                    (query.clone(), list.clone(), order.clone(), root.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder(format!(
                            "Local changes ({} files) — Enter shows the diff against HEAD",
                            list.len()
                        ))
                        .max_h(px(460.))
                        .filterable(false)
                        .items(items)
                        .on_query(move |q, window, _| {
                            *query_q.borrow_mut() = q.to_string();
                            window.refresh();
                        })
                        .on_confirm(move |ix, window, cx| {
                            let Some(c) = order_c.get(ix.row).map(|&i| list_c[i].clone()) else {
                                return;
                            };
                            window.close_dialog(cx);
                            let path = root_c.join(&c.path);
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
                                                label: "Local changes".into(),
                                                before_is: "HEAD".into(),
                                            },
                                            window,
                                            cx,
                                        )
                                    });
                                })
                                .detach();
                        }),
                )
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn parses_porcelain_z() {
        let out = " M src/main.rs\0?? notes.txt\0A  new.rs\0R  b.rs\0a.rs\0 D gone.rs\0";
        let c = parse_status(out);
        let got: Vec<(char, &str)> = c.iter().map(|c| (c.state, c.path.as_str())).collect();
        assert_eq!(
            got,
            [
                ('M', "src/main.rs"),
                ('U', "notes.txt"),
                ('A', "new.rs"),
                ('R', "b.rs"),
                ('D', "gone.rs")
            ]
        );
    }
}
