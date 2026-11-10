//! Rename the active pi session (Agent: Rename Session…).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Sizable as _, WindowExt as _};
use gpui_kit::*;
use pide_core::Agent;

pub(crate) fn open(agent: Entity<Agent>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let current = agent.read(cx).name.to_string();
    let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
    // Focus once the dialog exists (opening it takes focus for itself).
    let focus_once = Rc::new(Cell::new(true));
    let run = Rc::new({
        let (input, agent) = (input.clone(), agent.clone());
        move |window: &mut Window, cx: &mut App| {
            let name = input.read(cx).value().to_string();
            let agent = agent.clone();
            window.defer(cx, move |_, cx| {
                agent.update(cx, |a, cx| a.rename(name, cx))
            });
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
            .title("Rename session")
            .w(px(420.))
            .child(Input::new(&input).large())
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new().child(
                            Button::new("rename-session-cancel")
                                .label("Cancel")
                                .outline(),
                        ),
                    )
                    .child(
                        DialogAction::new()
                            .child(Button::new("rename-session-ok").label("Rename").primary()),
                    ),
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
