//! Unsaved changes on quit and when closing a project window: ask Save All /
//! Don't Save / Cancel instead of losing edits silently.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogClose, DialogFooter};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, WindowExt as _, v_flex};
use gpui_kit::*;
use pide_core::{Buffer, Services, Workspace};

/// Dirty, writable buffers of one project.
fn dirty(workspace: &Entity<Workspace>, cx: &App) -> Vec<Entity<Buffer>> {
    workspace
        .read(cx)
        .buffers()
        .iter()
        .filter(|b| {
            let b = b.read(cx);
            b.is_dirty() && b.read_only.is_none()
        })
        .cloned()
        .collect()
}

/// Every project window's workspace.
fn all_workspaces(cx: &App) -> Vec<Entity<Workspace>> {
    Services::open_projects(cx).into_iter().filter_map(|(id, _)| Services::for_window(id, cx).map(|s| s.workspace)).collect()
}

/// Ask about `workspaces`' unsaved files (in `window`), then run `proceed`
/// unless the user cancels. Runs `proceed` at once when nothing is unsaved.
pub fn confirm(workspaces: Vec<Entity<Workspace>>, window: &mut Window, cx: &mut App, proceed: Rc<dyn Fn(&mut Window, &mut App)>) {
    let files: Vec<(Entity<Workspace>, Entity<Buffer>)> =
        workspaces.iter().flat_map(|w| dirty(w, cx).into_iter().map(move |b| (w.clone(), b))).collect();
    if files.is_empty() {
        proceed(window, cx);
        return;
    }
    let names: Vec<String> = files.iter().map(|(_, b)| b.read(cx).file_name().to_string()).collect();
    let n = names.len();
    let files = Rc::new(files);
    window.open_dialog(cx, move |dialog, _, cx| {
        let (files, proceed, names) = (files.clone(), proceed.clone(), names.clone());
        let (files2, proceed2) = (files.clone(), proceed.clone());
        let muted = cx.theme().muted_foreground;
        dialog
            .title(if n == 1 { format!("Save changes to {}?", names[0]) } else { format!("Save changes to {n} files?") })
            .w(px(440.))
            .child(
                v_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(muted)
                    .child("Your changes are lost if you don't save them.")
                    .when(n > 1, |this| this.child(names.join(", "))),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("unsaved-cancel").label("Cancel").outline()))
                    .child(Button::new("unsaved-discard").label("Don't Save").outline().on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        proceed2(window, cx);
                    }))
                    .child(Button::new("unsaved-save").label(if n == 1 { "Save" } else { "Save All" }).primary().on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        save_all(&files2, window, cx, proceed.clone());
                    })),
            )
    });
}

use gpui_kit::prelude::FluentBuilder as _;

fn save_all(files: &[(Entity<Workspace>, Entity<Buffer>)], window: &mut Window, cx: &mut App, proceed: Rc<dyn Fn(&mut Window, &mut App)>) {
    let tasks: Vec<Task<anyhow::Result<()>>> = files.iter().map(|(w, b)| w.update(cx, |w, cx| w.save(b, cx))).collect();
    window
        .spawn(cx, async move |cx| {
            let mut errors = Vec::new();
            for t in tasks {
                if let Err(e) = t.await {
                    errors.push(format!("{e:#}"));
                }
            }
            let _ = cx.update(|window, cx| {
                if errors.is_empty() {
                    proceed(window, cx);
                } else {
                    // Don't close or quit over a failed save.
                    window.push_notification(Notification::error(format!("Not saved: {}", errors.join("; "))), cx);
                }
            });
        })
        .detach();
}

/// Cmd+Q: ask about unsaved files in every project, then quit.
pub fn quit(window: &mut Window, cx: &mut App) {
    confirm(all_workspaces(cx), window, cx, Rc::new(|_, cx| cx.quit()));
}

/// Close this project's window, asking about its unsaved files first.
pub fn close_window(window: &mut Window, cx: &mut App) {
    let Some(s) = Services::for_window(window.window_handle().window_id(), cx) else {
        window.remove_window();
        return;
    };
    confirm(vec![s.workspace], window, cx, Rc::new(|window, _| window.remove_window()));
}

/// The window's own close button goes through the same question.
pub fn guard_window(window: &mut Window, cx: &mut App) {
    // Set once the user has answered, so the second close request goes through.
    let answered = Rc::new(Cell::new(false));
    window.on_window_should_close(cx, move |window, cx| {
        if answered.get() {
            return true;
        }
        let Some(s) = Services::for_window(window.window_handle().window_id(), cx) else { return true };
        if dirty(&s.workspace, cx).is_empty() {
            return true;
        }
        let answered = answered.clone();
        confirm(
            vec![s.workspace],
            window,
            cx,
            Rc::new(move |window, _| {
                answered.set(true);
                window.remove_window();
            }),
        );
        false
    });
}
