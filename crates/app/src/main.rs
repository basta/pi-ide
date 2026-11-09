//! pide: window, dock layout, keymap, wiring. `pide [dir]`.

mod palette;
mod shell;
mod theme;

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::Assets;
use gpui_kit::*;
use pide_agent_pi::PiBackend;
use pide_core::{CommandRegistry, CommandSpec, Services, Sessions, Workspace, keymap, paths};

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
}

/// Process start, for the "cold start to interactive window" measurement (M1: < 1 s).
pub static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn main() {
    STARTED.get_or_init(std::time::Instant::now);
    init_logging();
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    log::info!("pide starting in {}", root.display());
    pide_runtime::init();

    gpui_kit::application().with_assets(Assets).run(move |cx| {
        gpui_kit::init(cx);
        let settings = pide_core::settings::load();
        theme::apply(settings.theme, None, cx);
        cx.set_global(settings);

        let workspace = cx.new(|cx| Workspace::new(root.clone(), cx));
        let factory_root = root.clone();
        let factory: pide_core::sessions::BackendFactory =
            Rc::new(move |resume| Box::new(PiBackend::new(factory_root.clone(), resume)) as Box<dyn pide_core::AgentBackend>);
        let sessions = cx.new(|cx| Sessions::new(factory, cx));
        cx.set_global(Services { workspace, sessions });

        pide_editor::init(cx);
        pide_file_tree::init(cx);
        pide_agent_chat::init(cx);
        cx.set_global(pide_core::opener::Openers {
            file: Rc::new(pide_editor::open_path),
            diff: Rc::new(pide_editor::diff_panel::open_diff),
        });
        register_app_commands(cx);
        theme::register_commands(cx);

        let keymap_errors = keymap::init(cx);
        pide_runtime::monitor::start(cx).detach();

        let title = format!(
            "pide — {}",
            root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        );
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1400.), px(900.)), cx))),
            titlebar: Some(TitlebarOptions { title: Some(title.into()), ..Default::default() }),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, move |window, cx| cx.new(|cx| shell::Shell::new(keymap_errors, window, cx)))
            .expect("open window");
        cx.activate(true);
    });
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
