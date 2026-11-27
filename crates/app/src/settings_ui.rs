//! pide's own pages in the Settings tab: Appearance, Editor, Keymap, Tool
//! Windows, Typst, pi › General and Files. Switches write settings.json for
//! you and apply at once.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::settings::{self, Settings, ThemeSetting};
use pide_core::settings_page::{self, Scope, SettingsPage};
use pide_core::{CommandRegistry, DockPosition, Services, toolwindow};

/// A switch row: (settings.json key, title, explanation, read, write).
type Row = (
    &'static str,
    &'static str,
    &'static str,
    fn(&Settings) -> bool,
    fn(&mut Settings, bool),
);

const EDITOR: &[Row] = &[
    (
        "editor.format_on_save",
        "Reformat on save",
        "Format through the language server before each save.",
        |s| s.format_on_save,
        |s, v| s.format_on_save = v,
    ),
    (
        "spellcheck.enabled",
        "Spell check",
        "Underlines spelling in prose (LaTeX, Typst, Markdown) and in code comments, strings and the names you define, with fixes on Alt+Enter. Needs Codebook: install it from Languages › Language Servers (Spelling).",
        |s| s.spellcheck,
        |s, v| s.spellcheck = v,
    ),
];

const TYPST: &[Row] = &[
    (
        "typst.pdf_on_type",
        "PDF as you type (experimental)",
        "Re-export the PDF after every change, without saving. Off: only on save.",
        |s| s.typst_pdf_on_type,
        |s, v| s.typst_pdf_on_type = v,
    ),
    (
        "typst.pdf_follow_edits",
        "PDF follows your edits",
        "After an export, scroll the PDF to the text you changed.",
        |s| s.typst_pdf_follow,
        |s, v| s.typst_pdf_follow = v,
    ),
];

const PI: &[Row] = &[
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
];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Appearance,
    Rows(&'static [Row]),
    Keymap,
    ToolWindows,
    Files,
}

struct Page {
    kind: Kind,
}

fn page(
    id: &'static str,
    group: Option<&'static str>,
    title: &'static str,
    keywords: &'static [&'static str],
    order: u32,
    kind: Kind,
) -> SettingsPage {
    SettingsPage {
        id,
        group,
        title,
        keywords,
        scoped: kind == Kind::ToolWindows,
        order,
        create: Rc::new(move |_, cx| cx.new(|_| Page { kind }).into()),
    }
}

pub fn init(cx: &mut App) {
    for p in [
        page(
            "appearance",
            None,
            "Appearance",
            &["Theme", "Dark", "Light"],
            10,
            Kind::Appearance,
        ),
        page(
            "editor",
            None,
            "Editor",
            &["Reformat on save", "Format", "Spell check", "Spelling"],
            20,
            Kind::Rows(EDITOR),
        ),
        page(
            "keymap",
            None,
            "Keymap",
            &["Keyboard shortcuts", "keymap.json", "Keys"],
            30,
            Kind::Keymap,
        ),
        page(
            "tool_windows",
            None,
            "Tool Windows",
            &["Panels", "Stripe", "Hide", "Turn off"],
            40,
            Kind::ToolWindows,
        ),
        page(
            "typst",
            Some("Languages"),
            "Typst",
            &["PDF as you type", "PDF follows your edits", "Export"],
            55,
            Kind::Rows(TYPST),
        ),
        page(
            "pi.general",
            Some("pi"),
            "General",
            &[
                "Send editor context",
                "Save files before sending to pi",
                "Collaborator tools",
                "Notify when pi finishes",
                "Notifications",
            ],
            60,
            Kind::Rows(PI),
        ),
        page(
            "files",
            None,
            "Settings Files",
            &["settings.json", "keymap.json", ".pide"],
            90,
            Kind::Files,
        ),
    ] {
        settings_page::register(cx, p);
    }
}

fn save(key: &'static str, value: serde_json::Value, cx: &mut App) {
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = settings::save_key(key, value) {
                log::error!("save settings: {e}");
            }
        })
        .detach();
}

/// A setting with its explanation on the left and its control on the right.
fn row(title: impl IntoElement, about: impl IntoElement, control: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .justify_between()
        .items_center()
        .gap_4()
        .py_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            v_flex()
                .min_w_0()
                .flex_1()
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(about)),
        )
        .child(div().flex_none().child(control))
}

impl Page {
    fn appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let theme = cx
            .try_global::<crate::theme::CurrentTheme>()
            .map(|t| t.0)
            .unwrap_or(s.theme);
        let button = |id: &'static str, label: &'static str, setting: ThemeSetting, command: &'static str| {
            Button::new(id)
                .small()
                .label(label)
                .map(|b| pide_core::colors::toggle(b, theme == setting, cx))
                .on_click(move |_, window, cx| {
                    CommandRegistry::run(command, window, cx);
                    window.refresh();
                })
        };
        row(
            "Theme",
            "pide's own Dark and Light, or the one macOS uses.",
            h_flex()
                .gap_1()
                .child(button("theme-dark", "Dark", ThemeSetting::Dark, "view.theme_dark"))
                .child(button("theme-light", "Light", ThemeSetting::Light, "view.theme_light"))
                .child(button(
                    "theme-system",
                    crate::theme::FOLLOW_SYSTEM,
                    ThemeSetting::System,
                    "view.theme_system",
                )),
            cx,
        )
        .into_any_element()
    }

    fn rows(&self, rows: &'static [Row], cx: &mut Context<Self>) -> AnyElement {
        let s = cx.try_global::<Settings>().cloned().unwrap_or_default();
        v_flex()
            .children(rows.iter().map(|&(key, title, about, get, set)| {
                let switch = Switch::new(key)
                    .checked(get(&s))
                    .on_click(move |checked: &bool, window, cx| {
                        let v = *checked;
                        set(cx.default_global::<Settings>(), v);
                        save(key, v.into(), cx);
                        window.refresh();
                    });
                row(title, about, switch, cx)
            }))
            .into_any_element()
    }

    fn keymap(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .child(row(
                "Keyboard shortcuts",
                "Every command and its keys. Pick one and press Enter to change it.",
                Button::new("edit-keys")
                    .small()
                    .label("Edit Shortcuts…")
                    .on_click(|_, window, cx| {
                        CommandRegistry::run("settings.keymap", window, cx);
                    }),
                cx,
            ))
            .child(row(
                "keymap.json",
                "Your shortcuts as a file. Saving it applies at once.",
                Button::new("open-keymap")
                    .small()
                    .label("Open keymap.json")
                    .on_click(|_, window, cx| {
                        CommandRegistry::run("keymap.open", window, cx);
                    }),
                cx,
            ))
            .into_any_element()
    }

    fn files(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let root = Services::of(window, cx).workspace.read(cx).root().to_path_buf();
        let project = settings::project_file(&root);
        v_flex()
            .child(row(
                "settings.json",
                settings::settings_file().display().to_string(),
                Button::new("open-settings-json")
                    .small()
                    .label("Open")
                    .on_click(|_, window, cx| {
                        CommandRegistry::run("settings.open", window, cx);
                    }),
                cx,
            ))
            .child(row(
                "This project",
                format!(
                    "{} (which tool windows are on here; written once you change one)",
                    project.display()
                ),
                Button::new("open-project-settings")
                    .small()
                    .label("Open")
                    .disabled(!project.exists())
                    .on_click(move |_, window, cx| {
                        pide_core::opener::open_file(project.clone(), window, cx)
                    }),
                cx,
            ))
            .into_any_element()
    }

    fn tool_windows(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let scope = settings_page::scope(window, cx);
        let root = Services::of(window, cx).workspace.read(cx).root().to_path_buf();
        let theme = cx.theme().clone();
        let mut specs = toolwindow::all(cx);
        // Built in first, script panels after.
        specs.sort_by_key(|s| s.id.starts_with("script:"));
        let note = match scope {
            Scope::Everyone => "Turned off, a tool window loses its stripe button and its key, and closes.",
            Scope::Project => "Only in this project, kept in .pide/settings.json. Unset ones follow Everyone.",
        };
        v_flex()
            .child(div().pb_2().text_xs().text_color(theme.muted_foreground).child(note))
            .children(specs.into_iter().map(|spec| {
                let id = spec.id;
                let everyone = toolwindow::enabled_in(id, None, cx);
                let here = toolwindow::project_override(id, &root, cx);
                let on = match scope {
                    Scope::Everyone => everyone,
                    Scope::Project => here.unwrap_or(everyone),
                };
                let dock = match spec.dock {
                    DockPosition::Left => "left",
                    DockPosition::Right => "right",
                    _ => "bottom",
                };
                let mut about = format!("Opens on the {dock}");
                if let Some(k) = pide_core::keymap::keys_for(cx, spec.command) {
                    about.push_str(&format!(" · {k}"));
                }
                if id.starts_with("script:") {
                    about.push_str(" · script panel");
                }
                match (scope, here) {
                    (Scope::Project, Some(_)) => about.push_str(" · set for this project"),
                    (Scope::Project, None) => about.push_str(if everyone {
                        " · on for everyone"
                    } else {
                        " · off for everyone"
                    }),
                    (Scope::Everyone, Some(h)) if h != everyone => about.push_str(if h {
                        " · this project turns it on"
                    } else {
                        " · this project turns it off"
                    }),
                    _ => {}
                }
                let title = h_flex()
                    .gap_2()
                    .child(gpui_kit::component::Icon::new(spec.icon.clone()).small())
                    .child(spec.title);
                let control = h_flex()
                    .gap_2()
                    .when(scope == Scope::Project && here.is_some(), |t| {
                        t.child(
                            Button::new(SharedString::from(format!("tw-reset-{id}")))
                                .xsmall()
                                .ghost()
                                .label("Reset")
                                .tooltip("Follow Everyone again")
                                .on_click(move |_, window, cx| {
                                    toolwindow::set_enabled(id, Scope::Project, None, window, cx)
                                }),
                        )
                    })
                    .child(
                        Switch::new(SharedString::from(format!("tw-{id}")))
                            .checked(on)
                            .on_click(move |checked: &bool, window, cx| {
                                toolwindow::set_enabled(id, scope, Some(*checked), window, cx)
                            }),
                    );
                row(title, about, control, cx)
            }))
            .into_any_element()
    }
}

impl Render for Page {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.kind {
            Kind::Appearance => self.appearance(cx),
            Kind::Rows(rows) => self.rows(rows, cx),
            Kind::Keymap => self.keymap(cx),
            Kind::ToolWindows => self.tool_windows(window, cx),
            Kind::Files => self.files(window, cx),
        };
        div().max_w(px(760.)).child(body)
    }
}
