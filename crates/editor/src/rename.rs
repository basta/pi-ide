//! Rename symbol (Shift+F6, spec M3). GPUI Kit has no rename UI: a small dialog
//! asks for the new name, the server computes the edit, and it is applied to
//! every file (open editors stay undoable; other files are edited and saved).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState, RopeExt as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Sizable as _, WindowExt as _};
use gpui_kit::*;
use lsp_types::{PrepareRenameResponse, WorkspaceEdit};
use pide_lsp::convert::{offset_to_server, range_from_server};
use pide_lsp::process::file_uri;
use serde_json::json;

use crate::EditorPanel;

pub(crate) fn rename_symbol(window: &mut Window, cx: &mut App) {
    let Some(panel) = crate::active_panel(window, cx) else {
        return;
    };
    let (path, editor, store) = {
        let p = panel.read(cx);
        (p.path.clone(), p.editor.clone(), p.lsp.clone())
    };
    let Some(store) = store else { return };
    let Some((enc, prepare)) = store.read(cx).server_for(&path).map(|s| {
        let prepare = matches!(
            s.capabilities
                .as_ref()
                .and_then(|c| c.rename_provider.as_ref()),
            Some(lsp_types::OneOf::Right(lsp_types::RenameOptions {
                prepare_provider: Some(true),
                ..
            }))
        );
        (s.encoding, prepare)
    }) else {
        window.push_notification(
            Notification::warning("No language server is running for this file."),
            cx,
        );
        return;
    };
    let (rope, offset) = {
        let e = editor.read(cx);
        (e.text().clone(), e.cursor())
    };
    let position = offset_to_server(&rope, offset, enc);
    // The word under the cursor, in case the server doesn't do prepareRename.
    let fallback = rope
        .word_range(offset)
        .map(|r| rope.slice(r).to_string())
        .unwrap_or_default();

    let prepare_task = prepare.then(|| {
        store.update(cx, |s, cx| {
            s.request(
                &path,
                Some(&rope),
                "textDocument/prepareRename",
                json!({ "textDocument": { "uri": file_uri(&path) }, "position": position }),
                cx,
            )
        })
    });

    window
        .spawn(cx, async move |cx| {
            let current = match prepare_task {
                None => Some(fallback),
                Some(task) => match task.await {
                    Ok(v) if v.is_null() => None,
                    Ok(v) => match serde_json::from_value::<PrepareRenameResponse>(v) {
                        Ok(PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. }) => {
                            Some(placeholder)
                        }
                        Ok(PrepareRenameResponse::Range(r)) => {
                            let r = range_from_server(&rope, r, enc);
                            let (a, b) = (
                                rope.position_to_offset(&r.start),
                                rope.position_to_offset(&r.end),
                            );
                            Some(rope.slice(a..b).to_string())
                        }
                        Ok(PrepareRenameResponse::DefaultBehavior { .. }) => Some(fallback),
                        Err(_) => Some(fallback),
                    },
                    Err(e) => {
                        log::info!("prepareRename: {e:#}");
                        None
                    }
                },
            };
            let _ = cx.update(|window, cx| match current.filter(|c| !c.is_empty()) {
                None => window.push_notification(
                    Notification::warning("There's nothing to rename here."),
                    cx,
                ),
                Some(current) => open_dialog(panel, current, position, enc, window, cx),
            });
        })
        .detach();
}

fn open_dialog(
    panel: Entity<EditorPanel>,
    current: String,
    position: lsp_types::Position,
    enc: pide_lsp::convert::Encoding,
    window: &mut Window,
    cx: &mut App,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(current.clone()));
    // Focus once the dialog exists (opening it takes focus for itself).
    let focus_once = std::rc::Rc::new(std::cell::Cell::new(true));

    let run = std::rc::Rc::new({
        let (input, current) = (input.clone(), current.clone());
        move |window: &mut Window, cx: &mut App| {
            let new_name = input.read(cx).value().trim().to_string();
            if new_name.is_empty() || new_name == current {
                return;
            }
            perform(
                panel.clone(),
                current.clone(),
                new_name,
                position,
                enc,
                window,
                cx,
            );
        }
    });

    let enter = run.clone();
    let sub = window.subscribe(&input, cx, move |_, ev: &InputEvent, window, cx| {
        if let InputEvent::PressEnter { .. } = ev {
            window.close_dialog(cx);
            enter(window, cx);
        }
    });
    let sub = std::rc::Rc::new(std::cell::RefCell::new(Some(sub)));

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
            .title(format!("Rename {current}"))
            .w(px(420.))
            .child(Input::new(&input).large())
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new()
                            .child(Button::new("rename-cancel").label("Cancel").outline()),
                    )
                    .child(
                        DialogAction::new()
                            .child(Button::new("rename-ok").label("Rename").primary()),
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

fn perform(
    panel: Entity<EditorPanel>,
    old: String,
    new_name: String,
    position: lsp_types::Position,
    enc: pide_lsp::convert::Encoding,
    window: &mut Window,
    cx: &mut App,
) {
    let (path, store) = {
        let p = panel.read(cx);
        (p.path.clone(), p.lsp.clone())
    };
    let Some(store) = store else { return };
    let task = store.update(cx, |s, cx| {
        s.request(
            &path,
            None,
            "textDocument/rename",
            json!({ "textDocument": { "uri": file_uri(&path) }, "position": position, "newName": new_name }),
            cx,
        )
    });
    window
        .spawn(cx, async move |cx| {
            let result = task
                .await
                .and_then(|v| Ok(serde_json::from_value::<Option<WorkspaceEdit>>(v)?));
            let _ = cx.update(|window, cx| match result {
                Ok(Some(edit)) => {
                    let files = edit.changes.as_ref().map(|c| c.len()).unwrap_or(0)
                        + match &edit.document_changes {
                            Some(lsp_types::DocumentChanges::Edits(e)) => e.len(),
                            Some(lsp_types::DocumentChanges::Operations(o)) => o.len(),
                            None => 0,
                        };
                    crate::apply_workspace_edit(edit, enc, window, cx);
                    let note = format!(
                        "Renamed {old} to {new_name} in {files} file{}.",
                        if files == 1 { "" } else { "s" }
                    );
                    window.push_notification(Notification::info(note), cx);
                }
                Ok(None) => window.push_notification(
                    Notification::warning("The language server made no changes."),
                    cx,
                ),
                Err(e) => window
                    .push_notification(Notification::error(format!("Rename failed: {e:#}")), cx),
            });
        })
        .detach();
}
