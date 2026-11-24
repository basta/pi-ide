//! Projects (spec M2.5): one window per project, recent projects, Open Folder…,
//! and the Recent Projects switcher.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{
    ActiveTheme, Sizable as _, WindowExt as _, button::Button, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_agent_pi::PiBackend;
use pide_core::recent::{self, Recent};
use pide_core::{CommandRegistry, CommandSpec, Services, Sessions, Workspace};

use crate::shell::Shell;

/// The recent-projects list, loaded once at startup.
struct RecentProjects(Recent);
impl Global for RecentProjects {}

/// Keymap errors from startup, shown in the first window that opens.
struct StartupErrors(Vec<String>);
impl Global for StartupErrors {}

pub fn init(keymap_errors: Vec<String>, cx: &mut App) {
    // A few hundred bytes, read once at startup (like the saved layout).
    cx.set_global(RecentProjects(Recent::load_from(&recent::file())));
    cx.set_global(StartupErrors(keymap_errors));

    // A closed window takes its project with it: dropping its Services stops its
    // pi processes. pide is a bare binary with no menu bar to reopen from, so the
    // last window closing quits.
    cx.on_window_closed(|cx, id| {
        Services::unregister(id, cx);
        pide_editor::remove_host(id, cx);
        crate::shell::forget(id, cx);
        crate::tool_windows::forget(id, cx);
        pide_core::settings_page::forget_window(id, cx);
        crate::recent_files::forget(id, cx);
        pide_scripts::forget_window(id, cx);
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();

    CommandRegistry::register(
        cx,
        CommandSpec::new("project.open", "File: Open Project…"),
        |_, cx| prompt_open(cx),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("project.recent", "File: Recent Projects…"),
        open_switcher,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("project.close", "File: Close Project"),
        crate::unsaved::close_window,
    );
}

/// The folder to open at startup when none is given: the last project that still exists.
pub fn last_project(cx: &App) -> Option<PathBuf> {
    cx.global::<RecentProjects>()
        .0
        .last_existing()
        .map(Path::to_path_buf)
}

/// Open `root` in a new window, or bring its window to the front if it's already open.
/// Deferred: callers are often inside a window's update (a palette, a menu), and
/// that window can't be read then (its bounds place the new window).
pub fn open(root: PathBuf, cx: &mut App) {
    cx.defer(move |cx| open_now(root, cx));
}

fn open_now(root: PathBuf, cx: &mut App) {
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    remember(&root, cx);

    if let Some(handle) = window_of(&root, cx) {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        return;
    }

    log::info!("opening project {}", root.display());
    // Before the window restores its layout, so saved script panels come back.
    pide_scripts::open_project(&root, cx);
    pide_core::toolwindow::load_project(&root, cx);
    let title = format!(
        "pide — {}",
        root.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    );
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(new_window_bounds(cx))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        // Under test control (devctl) windows stay hidden and never take focus.
        show: !controlled(),
        focus: !controlled(),
        ..Default::default()
    };
    let errors = cx
        .try_global::<StartupErrors>()
        .map(|e| e.0.clone())
        .unwrap_or_default();
    cx.set_global(StartupErrors(Vec::new()));
    let shown = root.display().to_string();
    let result = gpui_kit::open_window(options, cx, move |window, cx| {
        let workspace = cx.new(|cx| Workspace::new(root.clone(), cx));
        let backend_root = root.clone();
        let queries = crate::collab_queries::handler(workspace.downgrade());
        let factory: pide_core::sessions::BackendFactory = Rc::new(move |resume| {
            Box::new(PiBackend::new(backend_root.clone(), resume).with_queries(queries.clone()))
                as Box<dyn pide_core::AgentBackend>
        });
        let sessions = cx.new(|cx| Sessions::new(factory, cx));
        // Before any panel is built: panels find their project through the window.
        // Language servers start as files of their language open (spec M3).
        pide_lsp::LspStore::for_workspace(&workspace, cx);
        Services::register(
            window.window_handle().window_id(),
            Services {
                workspace,
                sessions,
            },
            cx,
        );
        cx.new(|cx| Shell::new(errors, window, cx))
    });
    if let Err(e) = result {
        log::error!("open window for {shown}: {e:#}");
    }
    if !controlled() {
        cx.activate(true);
    }
}

/// Driven by a test over the devctl socket (`--features devctl`, `PIDE_CTL`).
fn controlled() -> bool {
    cfg!(feature = "devctl") && std::env::var_os("PIDE_CTL").is_some()
}

/// Centered for the first window; later ones cascade down and right from the
/// frontmost window, so a new project doesn't open exactly on top of the last one.
fn new_window_bounds(cx: &mut App) -> Bounds<Pixels> {
    let default = Bounds::centered(None, size(px(1400.), px(900.)), cx);
    let Some(front) = cx
        .active_window()
        .or_else(|| cx.windows().into_iter().last())
    else {
        return default;
    };
    let Ok((b, screen)) = front.update(cx, |_, window, cx| {
        (
            window.bounds(),
            window.display(cx).map(|d| d.visible_bounds()),
        )
    }) else {
        return default;
    };
    // Only the position cascades: `bounds()` includes the title bar but a new
    // window's size doesn't, so reusing it would grow each window by a title bar.
    let next = Bounds {
        origin: point(b.origin.x + px(32.), b.origin.y + px(32.)),
        size: default.size,
    };
    // Off the bottom or right of the screen (under the Dock): start again at the top left.
    match screen {
        Some(screen) if next.bottom() > screen.bottom() || next.right() > screen.right() => {
            Bounds {
                origin: screen.origin,
                size: next.size.min(&screen.size),
            }
        }
        _ => next,
    }
}

/// The window that has `root` open, if any.
fn window_of(root: &Path, cx: &App) -> Option<AnyWindowHandle> {
    let (id, _) = Services::open_projects(cx)
        .into_iter()
        .find(|(_, r)| r == root)?;
    cx.windows().into_iter().find(|w| w.window_id() == id)
}

/// Move `root` to the front of the recent list and save it off the UI thread.
fn remember(root: &Path, cx: &mut App) {
    let recent = &mut cx.global_mut::<RecentProjects>().0;
    recent.touch(root, recent::now());
    save_recent(cx);
}

fn forget(root: &Path, cx: &mut App) {
    cx.global_mut::<RecentProjects>().0.remove(root);
    save_recent(cx);
}

fn save_recent(cx: &mut App) {
    let recent = cx.global::<RecentProjects>().0.clone();
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = recent.save_to(&recent::file()) {
                log::error!("save recent projects: {e:#}");
            }
        })
        .detach();
}

/// Native macOS folder picker; opens the chosen folder. With no window open
/// (first start, nothing recent) cancelling quits.
pub fn prompt_open(cx: &mut App) {
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Open Project".into()),
    });
    cx.spawn(async move |cx| {
        let folder = match picked.await {
            Ok(Ok(Some(paths))) => paths.into_iter().next(),
            Ok(Err(e)) => {
                log::error!("folder picker: {e:#}");
                None
            }
            _ => None,
        };
        cx.update(|cx| match folder {
            Some(folder) => open(folder, cx),
            None if cx.windows().is_empty() => cx.quit(),
            None => {}
        });
    })
    .detach();
}

struct Row {
    path: PathBuf,
    name: String,
    detail: String,
    open: bool,
}

fn rows(cx: &App) -> Vec<Row> {
    let open: Vec<PathBuf> = Services::open_projects(cx)
        .into_iter()
        .map(|(_, r)| r)
        .collect();
    let now = recent::now();
    cx.global::<RecentProjects>()
        .0
        .projects
        .iter()
        .map(|p| {
            let is_open = open.contains(&p.path);
            let missing = !p.path.is_dir();
            let when = if is_open {
                "open".to_string()
            } else {
                recent::ago(p.opened, now)
            };
            Row {
                name: p.name(),
                detail: format!(
                    "{} · {}{}",
                    tilde(&p.path),
                    when,
                    if missing { " · missing" } else { "" }
                ),
                path: p.path.clone(),
                open: is_open,
            }
        })
        .collect()
}

/// `/Users/ja/Projects/x` -> `~/Projects/x`.
fn tilde(path: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap().display())
        }
        _ => path.display().to_string(),
    }
}

/// Recent Projects: fuzzy search over the recent list (newest first). Enter opens
/// the project or focuses its window; × removes it from the list.
pub fn open_switcher(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let rows = Rc::new(RefCell::new(rows(cx)));
    let query = Rc::new(RefCell::new(String::new()));
    let order = Rc::new(RefCell::new(rank(&rows.borrow(), "")));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(std::cell::Cell::new(true));

    window.open_dialog(cx, move |dialog, _, _| {
        let (rows, order, query, state, focus_once) = (
            rows.clone(),
            order.clone(),
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
                let theme = cx.theme().clone();
                let items: Vec<CommandItem> =
                    order
                        .borrow()
                        .iter()
                        .map(|&i| {
                            let row = &rows.borrow()[i];
                            let (name, detail, path, open) = (
                                row.name.clone(),
                                row.detail.clone(),
                                row.path.clone(),
                                row.open,
                            );
                            let (rows, order, query) = (rows.clone(), order.clone(), query.clone());
                            let muted = theme.muted_foreground;
                            CommandItem::new().label(name.clone()).child(move |_, _| {
                                let (rows, order, query, path) =
                                    (rows.clone(), order.clone(), query.clone(), path.clone());
                                h_flex()
                                    .w_full()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        v_flex().min_w_0().child(div().child(name.clone())).child(
                                            div().text_xs().text_color(muted).child(detail.clone()),
                                        ),
                                    )
                                    .when(!open, |this| {
                                        this.child(
                                            Button::new(SharedString::from(format!(
                                                "forget-{}",
                                                path.display()
                                            )))
                                            .xsmall()
                                            .label("×")
                                            .tooltip("Remove from recent projects")
                                            .on_click(move |_, window, cx| {
                                                cx.stop_propagation();
                                                forget(&path, cx);
                                                rows.borrow_mut().retain(|r| r.path != path);
                                                *order.borrow_mut() =
                                                    rank(&rows.borrow(), &query.borrow());
                                                window.refresh();
                                            }),
                                        )
                                    })
                            })
                        })
                        .collect();
                let (rows_q, order_q, query_q) = (rows.clone(), order.clone(), query.clone());
                let (rows_c, order_c) = (rows.clone(), order.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(theme.radius_lg)
                        .placeholder(if rows.borrow().is_empty() {
                            "No recent projects — use File: Open Project…"
                        } else {
                            "Open recent project…"
                        })
                        .max_h(px(420.))
                        .filterable(false)
                        .items(items)
                        .on_query(move |q, window, _| {
                            *query_q.borrow_mut() = q.to_string();
                            *order_q.borrow_mut() = rank(&rows_q.borrow(), q);
                            window.refresh();
                        })
                        .on_confirm(move |ix, window, cx| {
                            let path = order_c
                                .borrow()
                                .get(ix.row)
                                .map(|&i| rows_c.borrow()[i].path.clone());
                            window.close_dialog(cx);
                            if let Some(path) = path {
                                if path.is_dir() {
                                    open(path, cx);
                                } else {
                                    window.push_notification(
                                        gpui_kit::component::notification::Notification::error(
                                            format!("{} no longer exists.", path.display()),
                                        ),
                                        cx,
                                    );
                                }
                            }
                        }),
                )
            })
    });
}

fn rank(rows: &[Row], query: &str) -> Vec<usize> {
    let hays: Vec<String> = rows
        .iter()
        .map(|r| format!("{} {}", r.name, r.path.display()))
        .collect();
    crate::palette::rank_strings(&hays, query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn switcher_ranks_by_name_and_keeps_recency_order_when_empty() {
        let row = |p: &str| Row {
            path: p.into(),
            name: Path::new(p).file_name().unwrap().to_string_lossy().into(),
            detail: String::new(),
            open: false,
        };
        let rows = vec![
            row("/Users/ja/Projects/pi-ide"),
            row("/Users/ja/Projects/blog"),
            row("/Users/ja/work/pide-plugins"),
        ];
        assert_eq!(rank(&rows, ""), vec![0, 1, 2]);
        assert_eq!(rank(&rows, "blog")[0], 1);
        assert_eq!(rank(&rows, "work")[0], 2);
    }
}
