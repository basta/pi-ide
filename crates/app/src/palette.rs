//! Find Action (Cmd+Shift+A): every registered command, ranked by nucleo.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme, WindowExt as _, h_flex};
use gpui_kit::*;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use pide_core::{CommandRegistry, CommandSpec, keymap};

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
    let order = Rc::new(RefCell::new(rank(&rows, "")));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(std::cell::Cell::new(true));

    window.open_dialog(cx, move |dialog, _, _| {
        let rows = rows.clone();
        let order = order.clone();
        let state = state.clone();
        let focus_once = focus_once.clone();
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
                let (rows_q, order_q) = (rows.clone(), order.clone());
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
                            *order_q.borrow_mut() = rank(&rows_q, q);
                            window.refresh();
                        })
                        .on_confirm(move |ix, window, cx| {
                            let id = order_c.borrow().get(ix.row).map(|&i| rows_c[i].id);
                            window.close_dialog(cx);
                            if let Some(id) = id {
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
