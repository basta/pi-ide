//! pide: windows (one per project), dock layout, keymap, wiring. `pide [dir]`.

mod collab_queries;
#[cfg(feature = "devctl")]
mod devctl;
mod goto_file;
mod keymap_ui;
mod menus;
mod palette;
mod projects;
mod recent_files;
mod settings_ui;
mod shell;
mod theme;
mod tool_windows;
mod unsaved;

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::Assets;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use pide_core::{CommandRegistry, CommandSpec, fswatch, keymap, paths};

fn init_logging() {
    let dir = paths::state_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,naga=warn,wgpu_core=warn,wgpu_hal=warn"),
    );
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("pide.log"))
    {
        builder.target(env_logger::Target::Pipe(Box::new(file)));
    }
    builder.init();

    // Panics in GPUI's macOS event callbacks abort the process, and the message
    // only reaches stderr. Put it (with a backtrace) in pide.log too.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!(
            "panic: {info}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        log::logger().flush();
        default_hook(info);
    }));
}

/// Process start, for the "cold start to interactive window" measurement (M1: < 1 s).
pub static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn main() {
    STARTED.get_or_init(std::time::Instant::now);
    init_logging();
    // `pide <dir>` opens that folder; plain `pide` reopens the last project
    // (or asks for a folder when there is none).
    let arg = std::env::args_os().nth(1).map(PathBuf::from);
    log::info!(
        "pide starting{}",
        arg.as_ref()
            .map(|p| format!(" with {}", p.display()))
            .unwrap_or_default()
    );
    pide_runtime::init();

    gpui_kit::application().with_assets(Assets).run(move |cx| {
        gpui_kit::init(cx);
        let settings = pide_core::settings::load();
        theme::apply(settings.theme, None, cx);
        cx.set_global(settings);

        pide_editor::init(cx);
        pide_file_tree::init(cx);
        pide_run::init(cx);
        pide_git::init(cx);
        palette::init(cx);
        pide_agent_chat::init(cx);
        pide_language_servers::init(cx);
        pide_problems::init(cx);
        pide_pi_settings::init(cx);
        pide_search::init(cx);
        pide_preview::init(cx);
        pide_pdf::init(cx);
        tool_windows::init(cx);
        cx.set_global(pide_core::opener::Openers {
            // PDFs open in the PDF panel; everything else in an editor tab.
            file: Rc::new(|path, window, cx| {
                if pide_pdf::is_pdf(&path) {
                    shell::open_pdf(path, window, cx)
                } else if pide_preview::image::is_image(&path) {
                    shell::open_image(path, window, cx)
                } else {
                    pide_editor::open_path(path, window, cx)
                }
            }),
            file_at: Rc::new(pide_editor::open_path_at),
            diff: Rc::new(pide_editor::diff_panel::open_diff),
            close: Rc::new(pide_editor::close_path),
            ask: Rc::new(shell::ask_pi_with),
            panel: Rc::new(shell::open_center_panel),
            pdf: Rc::new(shell::show_pdf),
            locations: Rc::new(shell::show_locations),
        });
        register_app_commands(cx);
        theme::register_commands(cx);

        let keymap_errors = keymap::init(cx);
        watch_keymap(cx);
        pide_runtime::monitor::start(cx).detach();
        projects::init(keymap_errors, cx);
        menus::init(cx); // after every command is registered
        #[cfg(feature = "devctl")]
        devctl::start(cx);

        match arg.or_else(|| projects::last_project(cx)) {
            Some(root) => projects::open(root, cx),
            None => projects::prompt_open(cx),
        }
    });
}

/// Keeps the keymap watcher alive for the app's lifetime.
struct KeymapWatch(
    #[allow(dead_code)] fswatch::Watcher,
    #[allow(dead_code)] Task<()>,
);
impl Global for KeymapWatch {}

/// Hot-reload `keymap.json`; the result shows in the frontmost window.
fn watch_keymap(cx: &mut App) {
    let dir = paths::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    match fswatch::watch(&dir, false) {
        Ok((watcher, mut rx)) => {
            let file = paths::keymap_file();
            let settings_file = pide_core::settings::settings_file();
            let task = cx.spawn(async move |cx| {
                while let Some(changes) = rx.recv().await {
                    // settings.json: apply without a restart.
                    if changes
                        .paths
                        .iter()
                        .any(|p| p.file_name() == settings_file.file_name())
                    {
                        let fresh = cx
                            .background_executor()
                            .spawn(async { pide_core::settings::load() })
                            .await;
                        cx.update(|cx| {
                            let theme_changed = cx
                                .try_global::<pide_core::settings::Settings>()
                                .is_none_or(|old| old.theme != fresh.theme);
                            let theme = fresh.theme;
                            cx.set_global(fresh);
                            if theme_changed {
                                for w in cx.windows() {
                                    let _ = w.update(cx, |_, window, cx| {
                                        theme::apply(theme, Some(window), cx)
                                    });
                                }
                            }
                        });
                    }
                    if !changes
                        .paths
                        .iter()
                        .any(|p| p.file_name() == file.file_name())
                    {
                        continue;
                    }
                    cx.update(|cx| {
                        let errors = keymap::apply(cx);
                        let note = if errors.is_empty() {
                            Notification::info("Keymap reloaded.")
                        } else {
                            Notification::error(errors.join("\n"))
                        };
                        if let Some(window) = cx
                            .active_window()
                            .or_else(|| cx.windows().into_iter().next())
                        {
                            let _ = window
                                .update(cx, |_, window, cx| window.push_notification(note, cx));
                        }
                    });
                }
            });
            cx.set_global(KeymapWatch(watcher, task));
        }
        Err(e) => log::warn!("can't watch {}: {e:#}", dir.display()),
    }
}

fn register_app_commands(cx: &mut App) {
    CommandRegistry::register(
        cx,
        CommandSpec::new("palette.open", "Find Action…").key("cmd-shift-a"),
        palette::open,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("find.in_files", "Edit: Find in Files…").key("cmd-shift-f"),
        shell::show_find_in_files,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("view.markdown_preview", "View: Markdown Preview"),
        shell::show_markdown_preview,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("file.open_in_browser", "File: Open in Browser"),
        |window, cx| {
            let Some(path) = shell::active_path(window, cx) else {
                return;
            };
            if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("html" | "htm" | "svg")
            ) {
                cx.open_url(&format!("file://{}", path.display()));
            } else {
                window.push_notification(
                    gpui_kit::component::notification::Notification::warning(
                        "Open an HTML file first.",
                    ),
                    cx,
                );
            }
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("file.goto", "Navigate: Go to File…").key("cmd-shift-o"),
        goto_file::open,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("settings.keymap", "Settings: Keymap…"),
        keymap_ui::open,
    );
    // pide.log (and pi's raw RPC log next to it), selected in Finder.
    CommandRegistry::register(
        cx,
        CommandSpec::new("app.show_log", "Help: Show Log in Finder"),
        |_, _| {
            let log = paths::state_dir().join("pide.log");
            let opener = if cfg!(target_os = "macos") {
                "/usr/bin/open"
            } else {
                "xdg-open"
            };
            let mut cmd = std::process::Command::new(opener);
            if cfg!(target_os = "macos") {
                cmd.arg("-R").arg(&log);
            } else {
                cmd.arg(paths::state_dir());
            }
            match cmd.spawn() {
                Ok(mut child) => {
                    std::thread::spawn(move || child.wait());
                }
                Err(e) => log::warn!("show log: {e}"),
            }
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("settings.pide", "Settings: pide…").key("cmd-,"),
        settings_ui::open,
    );

    CommandRegistry::register(
        cx,
        CommandSpec::new("run.command", "Run: Run Command…").key("ctrl-alt-r"),
        shell::run_command,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("project.select_opened", "Navigate: Select Opened File").key("alt-f1"),
        shell::select_opened_file,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("file.recent", "Navigate: Recent Files…").key("cmd-e"),
        recent_files::open,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("app.quit", "Quit pide").key("cmd-q"),
        unsaved::quit,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("keymap.open", "Preferences: Open Keymap File"),
        |window, cx| {
            let path = paths::keymap_file();
            let create = cx.background_executor().spawn({
                let path = path.clone();
                async move {
                    if !path.exists() {
                        std::fs::create_dir_all(path.parent().unwrap())?;
                        std::fs::write(&path, "{\n}\n")?;
                    }
                    anyhow::Ok(())
                }
            });
            window
                .spawn(cx, async move |cx| {
                    if let Err(e) = create.await {
                        log::error!("create keymap file: {e:#}");
                    }
                    let _ = cx.update(|window, cx| pide_editor::open_path(path, window, cx));
                })
                .detach();
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("pi.settings", "Settings: pi…"),
        shell::show_pi_settings,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("lsp.servers", "Settings: Language Servers…"),
        shell::show_language_servers,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("settings.open", "Settings: Open settings.json"),
        |window, cx| pide_editor::open_path(pide_core::settings::settings_file(), window, cx),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("keymap.reload", "Preferences: Reload Keymap"),
        |_, cx| {
            keymap::apply(cx);
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("agent.ask", "pi: Ask pi").key("cmd-shift-i"),
        shell::ask_pi,
    );
}
