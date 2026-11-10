//! Settings: Keymap… — every command with its shortcut; Enter changes one
//! (written to keymap.json for you, which reloads at once).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use pide_core::{CommandRegistry, keymap};

pub fn open(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let mut specs = CommandRegistry::all(cx);
    specs.sort_by(|a, b| a.title.cmp(&b.title));
    let rows: Rc<Vec<(String, String, String)>> = Rc::new(
        specs
            .iter()
            .map(|s| {
                (
                    s.id.to_string(),
                    s.title.to_string(),
                    keymap::keys_for(cx, s.id).unwrap_or_default(),
                )
            })
            .collect(),
    );
    let titles: Rc<Vec<String>> =
        Rc::new(rows.iter().map(|r| format!("{} {}", r.1, r.2)).collect());
    let order = Rc::new(RefCell::new((0..rows.len()).collect::<Vec<_>>()));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(Cell::new(true));
    window.open_dialog(cx, move |dialog, _, _| {
        let (rows, titles, order, state, focus_once) = (
            rows.clone(),
            titles.clone(),
            order.clone(),
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
                let muted = cx.theme().muted_foreground;
                let items: Vec<CommandItem> = order
                    .borrow()
                    .iter()
                    .map(|&i| {
                        let (_, title, keys) = rows[i].clone();
                        CommandItem::new().label(title.clone()).child(move |_, _| {
                            h_flex()
                                .w_full()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .child(title.clone()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(keys.clone()),
                                )
                        })
                    })
                    .collect();
                let (titles_q, order_q, order_c, rows_c) =
                    (titles.clone(), order.clone(), order.clone(), rows.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder("Keymap — pick a command, Enter to change its shortcut")
                        .max_h(px(460.))
                        .filterable(false)
                        .items(items)
                        .on_query(move |q, window, _| {
                            *order_q.borrow_mut() = if q.trim().is_empty() {
                                (0..titles_q.len()).collect()
                            } else {
                                crate::palette::rank_strings(&titles_q, q)
                            };
                            window.refresh();
                        })
                        .on_confirm(move |ix, window, cx| {
                            let Some((id, title, _)) =
                                order_c.borrow().get(ix.row).map(|&i| rows_c[i].clone())
                            else {
                                return;
                            };
                            window.close_dialog(cx);
                            let defaults = keymap::defaults(cx);
                            let id2 = id.clone();
                            let current = cx
                                .background_executor()
                                .spawn(async move { keymap::key_of(defaults, &id2) });
                            window
                                .spawn(cx, async move |cx| {
                                    let current = current.await;
                                    let _ = cx.update(|window, cx| {
                                        ask_key(id, title, current, window, cx)
                                    });
                                })
                                .detach();
                        }),
                )
            })
    });
}

/// Ask for the new shortcut of one command.
fn ask_key(id: String, title: String, current: Option<String>, window: &mut Window, cx: &mut App) {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("e.g. cmd-shift-k, or cmd-k cmd-s; empty removes it")
            .default_value(current.clone().unwrap_or_default())
    });
    let focus_once = Rc::new(Cell::new(true));
    let run = Rc::new({
        let input = input.clone();
        move |window: &mut Window, cx: &mut App| {
            let typed = input.read(cx).value().trim().to_string();
            let new = (!typed.is_empty()).then_some(typed);
            if let Some(k) = &new
                && !keymap::valid_keys(k)
            {
                window.push_notification(
                    Notification::warning(format!(
                        "\u{201c}{k}\u{201d} isn't a shortcut pide understands."
                    )),
                    cx,
                );
                return;
            }
            if new == current {
                return;
            }
            let (id, old, n) = (id.clone(), current.clone(), new.clone());
            let save = cx
                .background_executor()
                .spawn(async move { keymap::save_rebind(id, old, n) });
            window
                .spawn(cx, async move |cx| {
                    if let Err(e) = save.await {
                        let _ = cx.update(|window, cx| {
                            window.push_notification(
                                Notification::error(format!("Couldn't save keymap.json: {e}")),
                                cx,
                            )
                        });
                    }
                })
                .detach();
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
        let muted = cx.theme().muted_foreground;
        dialog
            .text_sm()
            .title(format!("Shortcut for {title}"))
            .w(px(460.))
            .child(
                v_flex()
                    .gap_2()
                    .child(Input::new(&input))
                    .child(div().text_xs().text_color(muted).child("Modifiers: cmd, alt, ctrl, shift. Keys: a–z, 0–9, f1–f12, enter, escape, tab, up, down, [, ], …")),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("key-cancel").label("Cancel").outline()))
                    .child(DialogAction::new().child(Button::new("key-ok").label("Save").primary())),
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
