//! Settings › Script Panels: every script panel this window can show (shipped,
//! yours, this project's), what it asks for and whether you allowed it, on/off
//! (per scope, the same switch as Tool Windows), and the files behind it.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::settings_page::{self, Scope, SettingsPage};
use pide_core::{Services, toolwindow};

use crate::manifest::{self, Found};
use std::collections::HashMap;

use crate::{Panels, granted_in, load_grants, project_dir, revoke, user_dir};

pub fn init(cx: &mut App) {
    settings_page::register(
        cx,
        SettingsPage {
            id: "script_panels",
            group: None,
            title: "Script Panels",
            keywords: &["JavaScript", "Permissions", "Revoke", "Allow", "panel.json", "main.js"],
            scoped: true,
            order: 45,
            create: Rc::new(|window, cx| cx.new(|cx| ScriptPanels::new(window, cx)).into()),
        },
    );
}

pub struct ScriptPanels {
    root: PathBuf,
    /// What you allowed, read when the page opens and on Look Again.
    grants: HashMap<String, serde_json::Value>,
}

impl ScriptPanels {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = Services::of(window, cx).workspace.read(cx).root().to_path_buf();
        // New folders and deleted ones show up when the page opens.
        crate::rescan(&root, cx);
        Self {
            root,
            grants: load_grants(),
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        crate::rescan(&self.root, cx);
        self.grants = load_grants();
        cx.notify();
    }

    /// The panels this window has: shipped, yours, and this project's.
    fn panels(&self, cx: &App) -> Vec<Found> {
        let mut v: Vec<Found> = cx
            .try_global::<Panels>()
            .map(|p| p.0.values().cloned().collect())
            .unwrap_or_default();
        v.retain(|f| f.project.as_deref().is_none_or(|r| r == self.root));
        let rank = |f: &Found| match f.scope() {
            "builtin" => 0,
            "user" => 1,
            _ => 2,
        };
        v.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.name.cmp(&b.name)));
        v
    }

    fn render_panel(&self, f: Found, scope: Scope, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let id: &'static str = toolwindow::get(&f.id(), cx).map(|s| s.id).unwrap_or("");
        let everyone = toolwindow::enabled_in(id, None, cx);
        let here = toolwindow::project_override(id, &self.root, cx);
        let on = match scope {
            Scope::Everyone => everyone,
            Scope::Project => here.unwrap_or(everyone),
        };
        let source = match f.scope() {
            "builtin" => "Shipped with pide".to_string(),
            "user" => format!("Yours · {}", tilde(&f.dir)),
            _ => format!(
                "This project · {}",
                f.dir.strip_prefix(&self.root).unwrap_or(&f.dir).display()
            ),
        };
        let allowed = granted_in(&f, &self.grants);
        let asked_before = !f.builtin && self.grants.contains_key(&f.dir.display().to_string());
        let (status, color) = if let Some(e) = &f.error {
            (format!("Doesn't load: {e}"), theme.danger)
        } else if f.builtin {
            ("Trusted".into(), theme.muted_foreground)
        } else if allowed {
            ("Allowed".into(), theme.success)
        } else if asked_before {
            ("Asks for more than you allowed: asks again when shown".into(), theme.warning)
        } else {
            ("Not allowed yet: asks when first shown".into(), theme.warning)
        };
        let dir = f.dir.clone();
        let entry = f.dir.join(manifest::ENTRY);
        let fix = f.error.clone().map(|e| (e, f.dir.clone()));
        let key = SharedString::from(f.id());
        v_flex()
            .gap_1()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .child(div().font_weight(FontWeight::MEDIUM).child(f.title()))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(source)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .when(scope == Scope::Project && here.is_some(), |t| {
                                t.child(
                                    Button::new(SharedString::from(format!("sp-reset-{key}")))
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
                                Switch::new(SharedString::from(format!("sp-on-{key}")))
                                    .checked(on)
                                    .on_click(move |checked: &bool, window, cx| {
                                        toolwindow::set_enabled(id, scope, Some(*checked), window, cx)
                                    }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_3()
                    .text_xs()
                    .children(f.manifest.permissions.describe().into_iter().map(|(asks, line)| {
                        h_flex()
                            .gap_1()
                            .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(if asks {
                                theme.warning
                            } else {
                                theme.success
                            }))
                            .child(line)
                    })),
            )
            .child(div().text_xs().text_color(color).child(status))
            .child(
                h_flex()
                    .gap_1()
                    .pt_1()
                    .child(
                        Button::new(SharedString::from(format!("sp-show-{key}")))
                            .xsmall()
                            .label("Show")
                            .disabled(!on)
                            .on_click(move |_, window, cx| toolwindow::toggle(id, window, cx)),
                    )
                    .child(
                        Button::new(SharedString::from(format!("sp-code-{key}")))
                            .xsmall()
                            .ghost()
                            .label("Open main.js")
                            .on_click(move |_, window, cx| {
                                pide_core::opener::open_file(entry.clone(), window, cx)
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("sp-reveal-{key}")))
                            .xsmall()
                            .ghost()
                            .label("Reveal Folder")
                            .on_click(move |_, _, cx| cx.reveal_path(&dir)),
                    )
                    .when(allowed && !f.builtin, |t| {
                        let f = f.clone();
                        t.child(
                            Button::new(SharedString::from(format!("sp-revoke-{key}")))
                                .xsmall()
                                .ghost()
                                .label("Revoke")
                                .tooltip("Close it and ask again before it runs")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    revoke(&f);
                                    toolwindow::close(id, window, cx);
                                    this.grants = load_grants();
                                    cx.notify();
                                })),
                        )
                    })
                    .when_some(fix, |t, (err, dir)| {
                        t.child(
                            Button::new(SharedString::from(format!("sp-fix-{key}")))
                                .xsmall()
                                .ghost()
                                .label("Ask pi to Fix")
                                .on_click(move |_, window, cx| {
                                    let text = format!(
                                        "The script panel in {} fails to load:\n\n```\n{err}\n```\n\nFix it.",
                                        dir.display()
                                    );
                                    pide_core::opener::ask_agent(text, window, cx)
                                }),
                        )
                    }),
            )
    }
}

fn tilde(p: &std::path::Path) -> String {
    match std::env::var_os("HOME") {
        Some(h) if p.starts_with(&h) => {
            format!("~/{}", p.strip_prefix(&h).unwrap_or(p).display())
        }
        _ => p.display().to_string(),
    }
}

impl Render for ScriptPanels {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let scope = settings_page::scope(window, cx);
        let panels = self.panels(cx);
        let root = self.root.clone();
        let note = match scope {
            Scope::Everyone => "Tool windows written in JavaScript. pide asks before one runs for the first time, and again when it asks for more.",
            Scope::Project => "On or off only in this project (kept in .pide/settings.json). Unset ones follow Everyone.",
        };
        let user = user_dir();
        let project = project_dir(&root);
        v_flex()
            .max_w(px(760.))
            .gap_2()
            .child(div().text_xs().text_color(theme.muted_foreground).child(note))
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(
                        Button::new("sp-new")
                            .small()
                            .label("New Panel with pi…")
                            .on_click(|_, window, cx| {
                                pide_core::opener::ask_agent(
                                    "Write a script panel for pide that ".into(),
                                    window,
                                    cx,
                                )
                            }),
                    )
                    .child(
                        Button::new("sp-rescan")
                            .small()
                            .ghost()
                            .label("Look Again")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(
                        Button::new("sp-user-dir")
                            .small()
                            .ghost()
                            .label("Your Panels Folder")
                            .on_click(move |_, _, cx| {
                                let _ = std::fs::create_dir_all(&user);
                                cx.reveal_path(&user)
                            }),
                    )
                    .child(
                        Button::new("sp-project-dir")
                            .small()
                            .ghost()
                            .label("This Project's Panels Folder")
                            .on_click(move |_, _, cx| {
                                let _ = std::fs::create_dir_all(&project);
                                cx.reveal_path(&project)
                            }),
                    ),
            )
            .when(panels.is_empty(), |t| {
                t.child(div().text_color(theme.muted_foreground).child("No script panels yet."))
            })
            .children(
                panels
                    .into_iter()
                    .map(|f| self.render_panel(f, scope, cx).into_any_element())
                    .collect::<Vec<_>>(),
            )
    }
}
