//! Recent Files (Cmd+E, JetBrains): the files this window showed, most recent
//! first. The current file is skipped, so Cmd+E, Enter flips back to the
//! previous one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme, WindowExt as _, h_flex};
use gpui_kit::*;
use pide_core::{Services, WorkspaceEvent};

const KEEP: usize = 50;

struct Recent {
    paths: Vec<PathBuf>,
    _sub: Subscription,
}

#[derive(Default)]
struct RecentFiles(HashMap<WindowId, Recent>);
impl Global for RecentFiles {}

/// Most recent first, without duplicates, at most `KEEP`.
fn touch(list: &mut Vec<PathBuf>, path: PathBuf) {
    list.retain(|p| *p != path);
    list.insert(0, path);
    list.truncate(KEEP);
}

/// Start following this window's active file.
pub fn track(window: &Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let Some(workspace) = Services::for_window(id, cx).map(|s| s.workspace) else {
        return;
    };
    let sub = cx.subscribe(&workspace, move |workspace, ev: &WorkspaceEvent, cx| {
        if let WorkspaceEvent::ActiveBufferChanged = ev {
            let Some(path) = workspace.read(cx).active().map(|b| b.read(cx).path.clone()) else {
                return;
            };
            if let Some(r) = cx.default_global::<RecentFiles>().0.get_mut(&id) {
                touch(&mut r.paths, path);
            }
        }
    });
    let first = workspace.read(cx).active().map(|b| b.read(cx).path.clone());
    let mut paths = Vec::new();
    if let Some(p) = first {
        paths.push(p);
    }
    cx.default_global::<RecentFiles>()
        .0
        .insert(id, Recent { paths, _sub: sub });
}

pub fn forget(id: WindowId, cx: &mut App) {
    if cx.has_global::<RecentFiles>() {
        cx.global_mut::<RecentFiles>().0.remove(&id);
    }
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
    let current = Services::for_window(id, cx).and_then(|s| {
        s.workspace
            .read(cx)
            .active()
            .map(|b| b.read(cx).path.clone())
    });
    let paths: Vec<PathBuf> = cx
        .try_global::<RecentFiles>()
        .and_then(|r| r.0.get(&id))
        .map(|r| {
            r.paths
                .iter()
                .filter(|p| Some(*p) != current.as_ref())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let rel: Rc<Vec<String>> = Rc::new(
        paths
            .iter()
            .map(|p| p.strip_prefix(&root).unwrap_or(p).display().to_string())
            .collect(),
    );
    let order = Rc::new(RefCell::new((0..rel.len()).collect::<Vec<_>>()));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(std::cell::Cell::new(true));
    let paths = Rc::new(paths);
    window.open_dialog(cx, move |dialog, _, _| {
        let (rel, order, state, focus_once, paths) = (
            rel.clone(),
            order.clone(),
            state.clone(),
            focus_once.clone(),
            paths.clone(),
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
                let muted = cx.theme().muted_foreground;
                let items: Vec<CommandItem> = order
                    .borrow()
                    .iter()
                    .map(|&i| {
                        let r = rel[i].clone();
                        let (dir, name) = match r.rsplit_once('/') {
                            Some((d, n)) => (d.to_string(), n.to_string()),
                            None => (String::new(), r.clone()),
                        };
                        CommandItem::new().label(r).child(move |_, _| {
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(div().flex_none().child(name.clone()))
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(dir.clone()),
                                )
                        })
                    })
                    .collect();
                let (rel_q, order_q, order_c, paths_c) =
                    (rel.clone(), order.clone(), order.clone(), paths.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder(if rel.is_empty() {
                            "No other files opened yet"
                        } else {
                            "Recent files"
                        })
                        .max_h(px(460.))
                        .filterable(false)
                        .items(items)
                        .on_query(move |q, window, _| {
                            // Keep recency order among matches.
                            let mut hits = crate::goto_file::rank(&rel_q, q, rel_q.len());
                            if !q.trim().is_empty() {
                                hits.sort();
                            }
                            *order_q.borrow_mut() = if q.trim().is_empty() {
                                (0..rel_q.len()).collect()
                            } else {
                                hits
                            };
                            window.refresh();
                        })
                        .on_confirm(move |ix, window, cx| {
                            let path = order_c.borrow().get(ix.row).map(|&i| paths_c[i].clone());
                            window.close_dialog(cx);
                            if let Some(path) = path {
                                pide_core::opener::open_file(path, window, cx);
                            }
                        }),
                )
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn most_recent_first_without_duplicates() {
        let mut l = Vec::new();
        for p in ["a", "b", "c", "a"] {
            touch(&mut l, PathBuf::from(p));
        }
        assert_eq!(l, ["a", "c", "b"].map(PathBuf::from));
    }
}
