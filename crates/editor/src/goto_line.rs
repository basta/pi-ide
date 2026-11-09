//! Go to Line (Cmd+L, JetBrains): `line` or `line:column`, 1-based.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState, Position, RopeExt as _};
use gpui_kit::*;

/// "12" or "12:5" (1-based) -> 0-based (line, column).
pub(crate) fn parse(s: &str) -> Option<(u32, u32)> {
    let mut parts = s.trim().splitn(2, [':', ',']);
    let line: u32 = parts.next()?.trim().parse().ok()?;
    let col: u32 = match parts.next() {
        Some(c) if !c.trim().is_empty() => c.trim().parse().ok()?,
        _ => 1,
    };
    Some((line.max(1) - 1, col.max(1) - 1))
}

pub(crate) fn open(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let Some(panel) = crate::active_panel(window, cx) else {
        return;
    };
    let editor = panel.read(cx).editor.clone();
    let (now, lines) = {
        let e = editor.read(cx);
        let p = e.cursor_position();
        (
            format!("{}:{}", p.line + 1, p.character + 1),
            e.text().lines_len(),
        )
    };
    let input = cx.new(|cx| InputState::new(window, cx).default_value(now));
    let focus_once = Rc::new(Cell::new(true));
    let run = Rc::new({
        let (input, editor) = (input.clone(), editor.clone());
        move |window: &mut Window, cx: &mut App| {
            let Some((line, col)) = parse(&input.read(cx).value()) else {
                return;
            };
            let editor = editor.clone();
            window.defer(cx, move |window, cx| {
                editor.update(cx, |e, cx| {
                    e.set_cursor_position(Position::new(line, col), window, cx);
                    e.focus(window, cx);
                })
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
            .title(format!("Go to line (1–{lines}), optionally :column"))
            .w(px(360.))
            .child(Input::new(&input))
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new()
                            .child(Button::new("goto-line-cancel").label("Cancel").outline()),
                    )
                    .child(
                        DialogAction::new()
                            .child(Button::new("goto-line-ok").label("Go").primary()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn parses_line_and_column() {
        assert_eq!(parse("12"), Some((11, 0)));
        assert_eq!(parse(" 12:5 "), Some((11, 4)));
        assert_eq!(parse("3,2"), Some((2, 1)));
        assert_eq!(parse("0"), Some((0, 0)));
        assert_eq!(parse("x"), None);
        assert_eq!(parse("4:"), Some((3, 0)));
    }
}
