//! Dark by default; light; or follow macOS (spec M2).

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use pide_core::settings::{self, ThemeSetting};
use pide_core::{CommandRegistry, CommandSpec};

pub struct CurrentTheme(pub ThemeSetting);
impl Global for CurrentTheme {}

pub fn apply(setting: ThemeSetting, window: Option<&mut Window>, cx: &mut App) {
    cx.set_global(CurrentTheme(setting));
    match setting {
        ThemeSetting::Dark => Theme::change(ThemeMode::Dark, window, cx),
        ThemeSetting::Light => Theme::change(ThemeMode::Light, window, cx),
        ThemeSetting::System => Theme::sync_system_appearance(window, cx),
    }
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
            "View: Theme: Follow macOS",
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
