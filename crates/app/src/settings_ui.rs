//! Settings: pide… — pide's own settings as switches (no hand-editing of
//! settings.json; it's written for you, and applies at once).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::CommandRegistry;
use pide_core::settings::{self, Settings, ThemeSetting};

/// A switch row: (settings.json key, title, explanation, read, write).
type Row = (
    &'static str,
    &'static str,
    &'static str,
    fn(&Settings) -> bool,
    fn(&mut Settings, bool),
);

const ROWS: &[Row] = &[
    (
        "agent.send_context",
        "Send editor context",
        "New prompts start with the open file and your selection.",
        |s| s.send_context,
        |s, v| s.send_context = v,
    ),
    (
        "agent.save_before_send",
        "Save files before sending to pi",
        "pi reads files from disk, so unsaved edits would be invisible to it.",
        |s| s.save_before_send,
        |s, v| s.save_before_send = v,
    ),
    (
        "agent.collab",
        "pi's collaborator tools",
        "pi can pin code, explain its changes, give tours and see your editor's errors. Applies to pi sessions started after the change.",
        |s| s.collab,
        |s, v| s.collab = v,
    ),
    (
        "agent.notify",
        "Notify when pi finishes",
        if cfg!(target_os = "macos") {
            "A macOS notification when pi is done or asks something while you're in another app."
        } else {
            "A desktop notification (notify-send) when pi is done or asks something while you're in another app."
        },
        |s| s.notify,
        |s, v| s.notify = v,
    ),
    (
        "editor.format_on_save",
        "Reformat on save",
        "Format through the language server before each save.",
        |s| s.format_on_save,
        |s, v| s.format_on_save = v,
    ),
];

pub fn open(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    window.open_dialog(cx, move |dialog, _, cx| {
        let s = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let theme = cx
            .try_global::<crate::theme::CurrentTheme>()
            .map(|t| t.0)
            .unwrap_or(s.theme);
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let theme_button = |id: &'static str,
                            label: &'static str,
                            setting: ThemeSetting,
                            command: &'static str| {
            Button::new(id)
                .small()
                .label(label)
                .map(|b| pide_core::colors::toggle(b, theme == setting, cx))
                .on_click(move |_, window, cx| {
                    CommandRegistry::run(command, window, cx);
                    window.refresh();
                })
        };
        dialog.text_sm().title("pide settings").w(px(520.)).child(
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(div().font_weight(FontWeight::MEDIUM).child("Theme"))
                        .child(
                            h_flex()
                                .gap_1()
                                .child(theme_button(
                                    "theme-dark",
                                    "Dark",
                                    ThemeSetting::Dark,
                                    "view.theme_dark",
                                ))
                                .child(theme_button(
                                    "theme-light",
                                    "Light",
                                    ThemeSetting::Light,
                                    "view.theme_light",
                                ))
                                .child(theme_button(
                                    "theme-system",
                                    crate::theme::FOLLOW_SYSTEM,
                                    ThemeSetting::System,
                                    "view.theme_system",
                                )),
                        ),
                )
                .children(ROWS.iter().map(|&(key, title, about, get, set)| {
                    let on = get(&s);
                    h_flex()
                        .justify_between()
                        .items_center()
                        .gap_4()
                        .pt_2()
                        .border_t_1()
                        .border_color(border)
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                                .child(div().text_xs().text_color(muted).child(about)),
                        )
                        .child(Switch::new(key).checked(on).on_click(
                            move |checked: &bool, window, cx| {
                                let v = *checked;
                                if cx.has_global::<Settings>() {
                                    set(cx.global_mut::<Settings>(), v);
                                }
                                cx.background_executor()
                                    .spawn(async move {
                                        if let Err(e) =
                                            settings::save_key(key, serde_json::json!(v))
                                        {
                                            log::error!("save settings: {e}");
                                        }
                                    })
                                    .detach();
                                window.refresh();
                            },
                        ))
                }))
                .child(
                    h_flex()
                        .gap_2()
                        .pt_2()
                        .border_t_1()
                        .border_color(border)
                        .child(
                            Button::new("open-settings-json")
                                .small()
                                .ghost()
                                .label("Open settings.json")
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                    CommandRegistry::run("settings.open", window, cx);
                                }),
                        )
                        .child(
                            Button::new("open-keymap")
                                .small()
                                .ghost()
                                .label("Keyboard shortcuts…")
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                    window.defer(cx, |window, cx| {
                                        CommandRegistry::run("settings.keymap", window, cx);
                                    });
                                }),
                        ),
                ),
        )
    });
}
