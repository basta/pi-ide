//! Dark by default; light; or follow macOS (spec M2).
//!
//! pide's own look ("Two voices", docs/DESIGN.md): the pide Dark and pide Light
//! themes in `themes/pide.json`, and fonts bundled into the binary so Linux
//! and macOS draw the same thing: Atkinson Hyperlegible Next for the UI,
//! Atkinson Hyperlegible Mono for code, Newsreader for what pi says
//! (`pide_core::colors::VOICE`).

use std::borrow::Cow;
use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeMode, ThemeSet};
use gpui_kit::*;
use pide_core::settings::{self, ThemeSetting};
use pide_core::{CommandRegistry, CommandSpec};

pub struct CurrentTheme(pub ThemeSetting);
impl Global for CurrentTheme {}

const PIDE_THEMES: &str = include_str!("../themes/pide.json");

const FONTS: &[&[u8]] = &[
    include_bytes!("../fonts/AtkinsonHyperlegibleNext-Regular.ttf"),
    include_bytes!("../fonts/AtkinsonHyperlegibleNext-Medium.ttf"),
    include_bytes!("../fonts/AtkinsonHyperlegibleNext-SemiBold.ttf"),
    include_bytes!("../fonts/AtkinsonHyperlegibleNext-Italic.ttf"),
    include_bytes!("../fonts/AtkinsonHyperlegibleMono-Regular.ttf"),
    include_bytes!("../fonts/AtkinsonHyperlegibleMono-SemiBold.ttf"),
    include_bytes!("../fonts/AtkinsonHyperlegibleMono-Italic.ttf"),
    include_bytes!("../fonts/Newsreader-Regular.ttf"),
    include_bytes!("../fonts/Newsreader-SemiBold.ttf"),
    include_bytes!("../fonts/Newsreader-Italic.ttf"),
];

/// Registers the bundled fonts and makes pide Dark / pide Light the themes
/// the dark and light modes load. Call once, before [`apply`].
pub fn install(cx: &mut App) {
    let fonts = FONTS.iter().map(|b| Cow::Borrowed(*b)).collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        log::error!("bundled fonts: {e}");
    }
    let set: ThemeSet = match serde_json::from_str(PIDE_THEMES) {
        Ok(set) => set,
        Err(e) => {
            log::error!("themes/pide.json: {e}");
            return;
        }
    };
    // `Theme::change` creates the global from the registry's defaults the
    // first time; make sure it exists, then swap in ours.
    Theme::change(ThemeMode::Dark, None, cx);
    for config in set.themes {
        let config = Rc::new(config);
        Theme::update(cx, |t| {
            if config.mode.is_dark() {
                t.dark_theme = config.clone();
            } else {
                t.light_theme = config.clone();
            }
        });
    }
}

pub fn apply(setting: ThemeSetting, window: Option<&mut Window>, cx: &mut App) {
    cx.set_global(CurrentTheme(setting));
    match setting {
        ThemeSetting::Dark => Theme::change(ThemeMode::Dark, window, cx),
        ThemeSetting::Light => Theme::change(ThemeMode::Light, window, cx),
        ThemeSetting::System => Theme::sync_system_appearance(window, cx),
    }
}

/// The caret blinks while you work and then stays on: each blink redraws the
/// window, which kept an idle pide at 25-50% CPU on software rendering (and
/// drains a laptop battery). 10 s after the last edit, move or focus it stops.
pub fn calm_caret(cx: &mut App) {
    Theme::update(cx, |t| {
        t.cursor_blink = gpui_kit::component::input::CursorBlink::blinking(
            std::time::Duration::from_millis(500),
        )
        .with_idle_timeout(std::time::Duration::from_secs(10));
    });
}

/// The window's appearance changed (macOS light/dark switch).
pub fn on_system_appearance_changed(window: &mut Window, cx: &mut App) {
    if cx
        .try_global::<CurrentTheme>()
        .is_some_and(|t| t.0 == ThemeSetting::System)
    {
        Theme::sync_system_appearance(Some(window), cx);
    }
}

/// The "follow the OS theme" choice, named for this platform.
pub const FOLLOW_SYSTEM: &str = if cfg!(target_os = "macos") {
    "Follow macOS"
} else {
    "Follow system"
};

pub fn register_commands(cx: &mut App) {
    for (id, title, setting) in [
        ("view.theme_dark", "View: Theme: Dark", ThemeSetting::Dark),
        (
            "view.theme_light",
            "View: Theme: Light",
            ThemeSetting::Light,
        ),
        (
            "view.theme_system",
            if cfg!(target_os = "macos") {
                "View: Theme: Follow macOS"
            } else {
                "View: Theme: Follow system"
            },
            ThemeSetting::System,
        ),
    ] {
        CommandRegistry::register(cx, CommandSpec::new(id, title), move |window, cx| {
            apply(setting, Some(window), cx);
            let value = serde_json::to_value(setting).unwrap_or_default();
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = settings::save_key("theme", value) {
                        log::error!("save settings: {e}");
                    }
                })
                .detach();
        });
    }
}
