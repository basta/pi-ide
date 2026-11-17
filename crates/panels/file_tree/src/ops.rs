//! File operations from the tree's context menu: New File / New Folder,
//! Rename, Delete (to the Trash, so Finder's Put Back works). Disk work runs
//! off the UI thread; the tree refreshes from the file watcher.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::*;

/// A name typed by the user: one path component, or a relative path for New
/// (`src/new.rs`), never escaping the folder.
pub(crate) fn valid_name(name: &str, allow_subdirs: bool) -> Result<(), &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Type a name.");
    }
    if name.starts_with('/') {
        return Err("Use a name relative to the folder.");
    }
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err("That name isn't allowed.");
        }
    }
    if !allow_subdirs && name.contains('/') {
        return Err("A name can't contain /.");
    }
    Ok(())
}

/// Ask for a name; `on_ok` gets it (trimmed) when the user confirms.
fn prompt_name(
    title: String,
    default: String,
    select_stem: bool,
    on_ok: Rc<dyn Fn(String, &mut Window, &mut App)>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }
    let input = cx.new(|cx| InputState::new(window, cx).default_value(default.clone()));
    let focus_once = Rc::new(Cell::new(true));
    let run = Rc::new({
        let input = input.clone();
        move |window: &mut Window, cx: &mut App| {
            let name = input.read(cx).value().trim().to_string();
            on_ok(name, window, cx);
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
    // The part before the extension, for Rename ("main" of "main.rs").
    let stem_len = Path::new(&default)
        .file_stem()
        .map(|s| s.len())
        .unwrap_or(default.len());
    window.open_dialog(cx, move |dialog, window, cx| {
        let (input, run, sub) = (input.clone(), run.clone(), sub.clone());
        if focus_once.replace(false) {
            let input = input.clone();
            window.defer(cx, move |window, cx| {
                input.update(cx, |i, cx| {
                    i.focus(window, cx);
                    if select_stem && stem_len > 0 {
                        i.set_selected_range(0..stem_len, cx);
                    } else {
                        i.select_all(window, cx);
                    }
                })
            });
        }
        dialog
            .text_sm()
            .title(title.clone())
            .w(px(420.))
            .child(Input::new(&input))
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new()
                            .child(Button::new("name-cancel").label("Cancel").outline()),
                    )
                    .child(DialogAction::new().child(Button::new("name-ok").label("OK").primary())),
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

/// Run blocking disk work off the UI thread; report an error, or call `done`.
fn disk<R: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<R> + Send + 'static,
    done: impl FnOnce(R, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let task = cx.background_executor().spawn(async move { work() });
    window
        .spawn(cx, async move |cx| {
            let r = task.await;
            let _ = cx.update(|window, cx| match r {
                Ok(v) => done(v, window, cx),
                Err(e) => window.push_notification(Notification::error(format!("{e}")), cx),
            });
        })
        .detach();
}

pub(crate) fn new_entry(dir: PathBuf, folder: bool, window: &mut Window, cx: &mut App) {
    let title = if folder {
        "New folder"
    } else {
        "New file (a/b.rs makes folders too)"
    };
    prompt_name(
        title.into(),
        String::new(),
        false,
        Rc::new(move |name, window, cx| {
            if let Err(e) = valid_name(&name, true) {
                window.push_notification(Notification::warning(e), cx);
                return;
            }
            let path = dir.join(&name);
            let target = path.clone();
            disk(
                move || {
                    if target.exists() {
                        return Err(std::io::Error::other(format!(
                            "{} already exists",
                            target.display()
                        )));
                    }
                    if folder {
                        std::fs::create_dir_all(&target)
                    } else {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&target, "")
                    }
                },
                move |_, window, cx| {
                    // Show where it went: its folder opens and it's selected.
                    crate::reveal(path.clone(), window, cx);
                    if !folder {
                        pide_core::opener::open_file(path, window, cx);
                    }
                },
                window,
                cx,
            );
        }),
        window,
        cx,
    );
}

pub(crate) fn rename(path: PathBuf, window: &mut Window, cx: &mut App) {
    let current = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    prompt_name(
        format!("Rename {current}"),
        current.clone(),
        true,
        Rc::new(move |name, window, cx| {
            if name == current {
                return;
            }
            if let Err(e) = valid_name(&name, false) {
                window.push_notification(Notification::warning(e), cx);
                return;
            }
            let (from, to) = (path.clone(), path.with_file_name(&name));
            let open_after = to.clone();
            let was_file = from.is_file();
            let closed = from.clone();
            disk(
                move || {
                    if to.exists() {
                        return Err(std::io::Error::other(format!(
                            "{} already exists",
                            to.display()
                        )));
                    }
                    std::fs::rename(&from, &to)
                },
                move |_, window, cx| {
                    crate::reveal(open_after.clone(), window, cx);
                    // The old tab now points at nothing: close it, open the new name.
                    pide_core::opener::close_file(closed, window, cx);
                    if was_file {
                        pide_core::opener::open_file(open_after, window, cx);
                    }
                },
                window,
                cx,
            );
        }),
        window,
        cx,
    );
}

/// Move to the Trash: Finder on macOS (Put Back works), `gio trash` elsewhere.
fn trash(path: &Path) -> std::io::Result<()> {
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("/usr/bin/osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "tell application \"Finder\" to delete (POSIX file (item 1 of argv) as alias)",
                "-e",
                "end run",
            ])
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()?
    } else {
        std::process::Command::new("gio")
            .arg("trash")
            .arg(path)
            .output()?
    };
    if status.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "couldn't move {} to the Trash: {}",
            path.display(),
            String::from_utf8_lossy(&status.stderr).trim()
        )))
    }
}

pub(crate) fn delete(path: PathBuf, window: &mut Window, cx: &mut App) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let what = if path.is_dir() { "folder" } else { "file" };
    window.open_alert_dialog(cx, move |alert, _, _| {
        let path = path.clone();
        alert
            .title(format!("Move {name} to the Trash?"))
            .description(format!(
                "The {what} goes to the Trash; you can put it back from there."
            ))
            .confirm()
            .ok_text("Move to Trash")
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                let p = path.clone();
                let closed = path.clone();
                disk(
                    move || trash(&p),
                    move |_, window, cx| pide_core::opener::close_file(closed, window, cx),
                    window,
                    cx,
                );
                true
            })
    });
}

/// Show the file in Finder (selected), or open its folder elsewhere.
pub(crate) fn reveal(path: &Path) {
    let r = if cfg!(target_os = "macos") {
        std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(path)
            .spawn()
    } else {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        std::process::Command::new("xdg-open").arg(dir).spawn()
    };
    match r {
        // Reap the child so it doesn't linger as a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => log::warn!("reveal {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn names_stay_inside_the_folder() {
        assert!(valid_name("a.rs", false).is_ok());
        assert!(valid_name("src/a.rs", true).is_ok());
        assert!(valid_name("src/a.rs", false).is_err());
        assert!(valid_name("../x", true).is_err());
        assert!(valid_name("/etc/x", true).is_err());
        assert!(valid_name("a//b", true).is_err());
        assert!(valid_name("  ", true).is_err());
    }
}
