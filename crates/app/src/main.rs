//! pide: windows (one per project), dock layout, keymap, wiring. `pide [dir]`.

mod menus;
mod palette;
mod projects;
mod shell;
mod theme;

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
    if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("pide.log")) {
        builder.target(env_logger::Target::Pipe(Box::new(file)));
    }
    builder.init();

    // Panics in GPUI's macOS event callbacks abort the process, and the message
    // only reaches stderr. Put it (with a backtrace) in pide.log too.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
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
    log::info!("pide starting{}", arg.as_ref().map(|p| format!(" with {}", p.display())).unwrap_or_default());
    pide_runtime::init();

    gpui_kit::application().with_assets(Assets).run(move |cx| {
        gpui_kit::init(cx);
        let settings = pide_core::settings::load();
        theme::apply(settings.theme, None, cx);
        cx.set_global(settings);

        pide_editor::init(cx);
        pide_file_tree::init(cx);
        pide_agent_chat::init(cx);
        pide_language_servers::init(cx);
        cx.set_global(pide_core::opener::Openers {
            file: Rc::new(pide_editor::open_path),
            file_at: Rc::new(pide_editor::open_path_at),
            diff: Rc::new(pide_editor::diff_panel::open_diff),
        });
        register_app_commands(cx);
        theme::register_commands(cx);

        let keymap_errors = keymap::init(cx);
        watch_keymap(cx);
        pide_runtime::monitor::start(cx).detach();
        projects::init(keymap_errors, cx);
        menus::init(cx); // after every command is registered

        match arg.or_else(|| projects::last_project(cx)) {
            Some(root) => projects::open(root, cx),
            None => projects::prompt_open(cx),
        }
    });
}

/// Keeps the keymap watcher alive for the app's lifetime.
struct KeymapWatch(#[allow(dead_code)] fswatch::Watcher, #[allow(dead_code)] Task<()>);
impl Global for KeymapWatch {}

/// Hot-reload `keymap.json`; the result shows in the frontmost window.
fn watch_keymap(cx: &mut App) {
    let dir = paths::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    match fswatch::watch(&dir, false) {
        Ok((watcher, mut rx)) => {
            let file = paths::keymap_file();
            let task = cx.spawn(async move |cx| {
                while let Some(changes) = rx.recv().await {
                    if !changes.paths.iter().any(|p| p.file_name() == file.file_name()) {
                        continue;
                    }
                    let _ = cx.update(|cx| {
                        let errors = keymap::apply(cx);
                        let note = if errors.is_empty() {
                            Notification::info("Keymap reloaded.")
                        } else {
                            Notification::error(errors.join("\n"))
                        };
                        if let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) {
                            let _ = window.update(cx, |_, window, cx| window.push_notification(note, cx));
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
    CommandRegistry::register(cx, CommandSpec::new("palette.open", "Find Action…").key("cmd-shift-a"), palette::open);
    CommandRegistry::register(cx, CommandSpec::new("app.quit", "Quit pide").key("cmd-q"), |_, cx| cx.quit());
    CommandRegistry::register(cx, CommandSpec::new("keymap.open", "Preferences: Open Keymap File"), |window, cx| {
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
    });
    CommandRegistry::register(cx, CommandSpec::new("lsp.servers", "Settings: Language Servers…"), shell::show_language_servers);
    CommandRegistry::register(cx, CommandSpec::new("settings.open", "Settings: Open settings.json"), |window, cx| {
        pide_editor::open_path(pide_core::settings::settings_file(), window, cx)
    });
    CommandRegistry::register(cx, CommandSpec::new("keymap.reload", "Preferences: Reload Keymap"), |_, cx| {
        keymap::apply(cx);
    });
    CommandRegistry::register(cx, CommandSpec::new("view.toggle_project", "View: Toggle Project Panel").key("cmd-1"), |window, cx| {
        shell::toggle_dock(gpui_kit::component::dock::DockPlacement::Left, window, cx)
    });
    CommandRegistry::register(cx, CommandSpec::new("view.toggle_agent", "View: Toggle pi Panel"), |window, cx| {
        shell::toggle_dock(gpui_kit::component::dock::DockPlacement::Right, window, cx)
    });
}
