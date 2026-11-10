//! Find Action (Cmd+Shift+A): every registered command, ranked by nucleo.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme, WindowExt as _, h_flex};
use gpui_kit::*;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use pide_core::{CommandRegistry, CommandSpec, keymap};

/// Recently run actions (ids, newest first), shown first with an empty query.
#[derive(Default)]
struct RecentActions(Vec<String>);
impl Global for RecentActions {}

const RECENT_KEEP: usize = 8;

fn recent_file() -> std::path::PathBuf {
    pide_core::paths::state_dir().join("recent-actions.json")
}

/// Load the recent actions (off the UI thread). Call once at startup.
pub fn init(cx: &mut App) {
    let load = cx.background_executor().spawn(async {
        std::fs::read_to_string(recent_file())
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
            .unwrap_or_default()
    });
    cx.spawn(async move |cx| {
        let list = load.await;
        let _ = cx.update(|cx| cx.set_global(RecentActions(list)));
    })
    .detach();
}

fn remember(id: &str, cx: &mut App) {
    let list = &mut cx.default_global::<RecentActions>().0;
    list.retain(|x| x != id);
    list.insert(0, id.to_string());
    list.truncate(RECENT_KEEP);
    let snapshot = list.clone();
    cx.background_executor()
        .spawn(async move {
            let _ = std::fs::create_dir_all(pide_core::paths::state_dir());
            if let Ok(text) = serde_json::to_string(&snapshot) {
                let _ = std::fs::write(recent_file(), text);
            }
        })
        .detach();
}

/// Empty query: recent actions first, then the rest in list order.
fn initial_order(rows: &[Row], recent: &[String]) -> Vec<usize> {
    let mut order: Vec<usize> = recent
        .iter()
        .filter_map(|id| rows.iter().position(|r| r.id == id))
        .collect();
    let rest: Vec<usize> = (0..rows.len()).filter(|i| !order.contains(i)).collect();
    order.extend(rest);
    order
}

struct Row {
    id: &'static str,
    title: SharedString,
    keys: Option<String>,
}

fn rank(all: &[Row], query: &str) -> Vec<usize> {
    let hays: Vec<String> = all
        .iter()
        .map(|r| format!("{} {}", r.title, r.id))
        .collect();
    rank_strings(&hays, query)
}

/// Indices of `haystacks` matching `query`, best first (ties keep list order).
/// An empty query keeps everything in list order.
pub fn rank_strings(haystacks: &[String], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..haystacks.len()).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(usize, u32)> = haystacks
        .iter()
        .enumerate()
        .filter_map(|(i, h)| {
            let mut buf = Vec::new();
            pattern
                .score(nucleo_matcher::Utf32Str::new(h, &mut buf), &mut matcher)
                .map(|s| (i, s))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}

pub fn open(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let rows: Rc<Vec<Row>> = Rc::new(
        CommandRegistry::all(cx)
            .into_iter()
            .map(|CommandSpec { id, title, .. }| Row {
                id,
                title,
                keys: keymap::keys_for(cx, id),
            })
            .collect(),
    );
    let recent: Rc<Vec<String>> = Rc::new(
        cx.try_global::<RecentActions>()
            .map(|r| r.0.clone())
            .unwrap_or_default(),
    );
    let order = Rc::new(RefCell::new(initial_order(&rows, &recent)));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(std::cell::Cell::new(true));

    window.open_dialog(cx, move |dialog, _, _| {
        let recent = recent.clone();
        let rows = rows.clone();
        let order = order.clone();
        let state = state.clone();
        let focus_once = focus_once.clone();
        dialog
            .text_sm()
            .close_button(false)
            .p_0()
            .content(move |content, window, cx| {
                if focus_once.replace(false) {
                    let state = state.clone();
                    window.defer(cx, move |window, cx| {
                        state.read(cx).focus_handle(cx).focus(window, cx)
                    });
                }
                let items: Vec<CommandItem> = order
                    .borrow()
                    .iter()
                    .map(|&i| {
                        let row = &rows[i];
                        let title = row.title.clone();
                        let keys = row.keys.clone();
                        CommandItem::new().label(title.clone()).child(move |_, cx| {
                            h_flex()
                                .w_full()
                                .justify_between()
                                .child(title.clone())
                                .children(keys.clone().map(|k| {
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(k)
                                }))
                        })
                    })
                    .collect();
                let (rows_q, order_q, recent_q) = (rows.clone(), order.clone(), recent.clone());
                let (rows_c, order_c) = (rows.clone(), order.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder("Find action…")
                        .max_h(px(420.))
                        .filterable(false)
                        .items(items)
                        .on_query(move |q, window, _| {
                            *order_q.borrow_mut() = if q.trim().is_empty() {
                                initial_order(&rows_q, &recent_q)
                            } else {
                                rank(&rows_q, q)
                            };
                            window.refresh();
                        })
                        .on_confirm(move |ix, window, cx| {
                            let id = order_c.borrow().get(ix.row).map(|&i| rows_c[i].id);
                            window.close_dialog(cx);
                            if let Some(id) = id {
                                remember(id, cx);
                                CommandRegistry::run(id, window, cx);
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
    fn recent_actions_come_first() {
        let rows: Vec<Row> = ["a.one", "b.two", "c.three"]
            .into_iter()
            .map(|id| Row {
                id,
                title: id.into(),
                keys: None,
            })
            .collect();
        assert_eq!(
            initial_order(&rows, &["c.three".into(), "gone".into()]),
            vec![2, 0, 1]
        );
        assert_eq!(initial_order(&rows, &[]), vec![0, 1, 2]);
    }

    #[::core::prelude::v1::test]
    fn fuzzy_ranks_best_match_first() {
        let rows = vec![
            Row {
                id: "agent.abort",
                title: "Agent: Stop".into(),
                keys: None,
            },
            Row {
                id: "editor.save",
                title: "File: Save".into(),
                keys: None,
            },
            Row {
                id: "palette.open",
                title: "Find Action".into(),
                keys: None,
            },
        ];
        assert_eq!(rank(&rows, "").len(), 3);
        assert_eq!(rank(&rows, "fsave")[0], 1);
        assert_eq!(rank(&rows, "stop")[0], 0);
        assert!(rank(&rows, "zzzz").is_empty());
    }
}
