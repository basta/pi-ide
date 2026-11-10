//! The session's model picker: a search popup over the models pi can use;
//! Enter switches this session (pi's `set_model`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme, WindowExt as _, h_flex};
use gpui_kit::*;
use pide_core::{Agent, ModelInfo};

/// Models matching every word of `query` (in provider/id/name), current first.
pub(crate) fn filter(models: &[ModelInfo], current: Option<&ModelInfo>, query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut out: Vec<usize> = models
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            let hay = format!("{}/{} {}", m.provider, m.id, m.name).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .map(|(i, _)| i)
        .collect();
    if let Some(c) = current {
        out.sort_by_key(|&i| !(models[i].provider == c.provider && models[i].id == c.id));
    }
    out
}

/// Short label for the header button.
pub(crate) fn short_label(m: &ModelInfo) -> String {
    if m.name.is_empty() {
        // Only the id is known (from the session file): "moonshotai/kimi-k2.6" -> "kimi-k2.6".
        return m.id.rsplit('/').next().unwrap_or(&m.id).to_string();
    }
    // "MoonshotAI: Kimi K2.6" -> "Kimi K2.6"
    m.name.rsplit(": ").next().unwrap_or(&m.name).to_string()
}

pub(crate) fn open(agent: Entity<Agent>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let query = Rc::new(RefCell::new(String::new()));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(Cell::new(true));
    window.open_dialog(cx, move |dialog, _, _| {
        let (agent, query, state, focus_once) = (
            agent.clone(),
            query.clone(),
            state.clone(),
            focus_once.clone(),
        );
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
                let a = agent.read(cx);
                let models = a.models.clone();
                let current = a.model.clone();
                let order = filter(&models, current.as_ref(), &query.borrow());
                let muted = cx.theme().muted_foreground;
                let rows: Vec<CommandItem> = order
                    .iter()
                    .map(|&i| {
                        let m = models[i].clone();
                        let is_current = current
                            .as_ref()
                            .is_some_and(|c| c.provider == m.provider && c.id == m.id);
                        CommandItem::new().label(m.name.clone()).child(move |_, _| {
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(div().w(px(14.)).flex_none().child(if is_current {
                                    "✓"
                                } else {
                                    ""
                                }))
                                .child(div().flex_none().child(if m.name.is_empty() {
                                    m.id.clone()
                                } else {
                                    m.name.clone()
                                }))
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(format!("{}/{}", m.provider, m.id)),
                                )
                        })
                    })
                    .collect();
                let placeholder = if models.is_empty() {
                    "Loading models…"
                } else {
                    "Pick a model for this session"
                };
                let (query_q, order_c, agent_c) = (query.clone(), order.clone(), agent.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder(placeholder)
                        .max_h(px(460.))
                        .filterable(false)
                        .items(rows)
                        .on_query(move |q, window, _| {
                            *query_q.borrow_mut() = q.to_string();
                            window.refresh();
                        })
                        .on_confirm(move |ix, window, cx| {
                            let pick = order_c.get(ix.row).map(|&i| models[i].clone());
                            window.close_dialog(cx);
                            if let Some(m) = pick {
                                let agent = agent_c.clone();
                                // Not inside anyone's update: defer.
                                window.defer(cx, move |_, cx| {
                                    agent.update(cx, |a, cx| a.set_model(m.provider, m.id, cx))
                                });
                            }
                        }),
                )
            })
    });
}

/// The thinking-level picker: the levels the model supports, current ticked.
pub(crate) fn open_thinking(agent: Entity<Agent>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(Cell::new(true));
    window.open_dialog(cx, move |dialog, _, _| {
        let (agent, state, focus_once) = (agent.clone(), state.clone(), focus_once.clone());
        dialog
            .text_sm()
            .close_button(false)
            .p_0()
            .w(px(320.))
            .content(move |content, window, cx| {
                if focus_once.replace(false) {
                    let state = state.clone();
                    window.defer(cx, move |window, cx| {
                        state.read(cx).focus_handle(cx).focus(window, cx)
                    });
                }
                let a = agent.read(cx);
                let current = a.thinking.clone().unwrap_or_default();
                let levels: Vec<String> = if a.thinking_levels.is_empty() {
                    vec![current.clone()]
                } else {
                    a.thinking_levels.clone()
                };
                let rows: Vec<CommandItem> = levels
                    .iter()
                    .map(|l| {
                        let (l, tick) = (l.clone(), *l == current);
                        CommandItem::new().label(l.clone()).child(move |_, _| {
                            h_flex()
                                .gap_2()
                                .child(div().w(px(14.)).flex_none().child(if tick {
                                    "✓"
                                } else {
                                    ""
                                }))
                                .child(l.clone())
                        })
                    })
                    .collect();
                let (levels_c, agent_c) = (levels.clone(), agent.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder("Thinking level")
                        .max_h(px(320.))
                        .filterable(false)
                        .items(rows)
                        .on_confirm(move |ix, window, cx| {
                            let pick = levels_c.get(ix.row).cloned();
                            window.close_dialog(cx);
                            if let Some(level) = pick {
                                let agent = agent_c.clone();
                                window.defer(cx, move |_, cx| {
                                    agent.update(cx, |a, cx| a.set_thinking(level, cx))
                                });
                            }
                        }),
                )
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, id: &str, name: &str) -> ModelInfo {
        ModelInfo {
            provider: p.into(),
            id: id.into(),
            name: name.into(),
            reasoning: false,
        }
    }

    #[::core::prelude::v1::test]
    fn filters_by_words_and_puts_current_first() {
        let list = vec![
            m("openrouter", "a/kimi", "Kimi"),
            m("anthropic", "claude-haiku", "Haiku"),
            m("openrouter", "x/haiku", "Haiku OR"),
        ];
        assert_eq!(filter(&list, None, "haiku"), vec![1, 2]);
        assert_eq!(filter(&list, None, "openrouter haiku"), vec![2]);
        assert_eq!(filter(&list, Some(&list[2]), ""), vec![2, 0, 1]);
        assert_eq!(
            short_label(&m("o", "x", "MoonshotAI: Kimi K2.6")),
            "Kimi K2.6"
        );
    }
}
