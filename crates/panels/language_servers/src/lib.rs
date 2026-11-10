//! Language Servers tab (spec M3): every server pide knows, whether it's
//! installed, its state in this project, the install command with an Install
//! button (output streams in), and Restart.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{DockPosition, Panel, Services};
use pide_lsp::registry::{self, Installed, ServerSpec};
use pide_lsp::{LspStore, ServerState};

pub const PANEL_NAME: &str = "language_servers";

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| LanguageServers::new(window, cx)))
    });
}

enum InstallRun {
    Running {
        lines: Vec<String>,
    },
    Done {
        ok: bool,
        summary: String,
        lines: Vec<String>,
    },
}

/// The open instance per window (restored from a layout or opened by command),
/// so the command focuses it instead of adding a second one.
#[derive(Default)]
struct Instances(std::collections::HashMap<WindowId, WeakEntity<LanguageServers>>);
impl Global for Instances {}

/// This window's open LanguageServers panel, if any.
pub fn existing(window: &Window, cx: &App) -> Option<Entity<LanguageServers>> {
    cx.try_global::<Instances>()?
        .0
        .get(&window.window_handle().window_id())?
        .upgrade()
}

pub struct LanguageServers {
    store: Entity<LspStore>,
    /// Fresh `--version` checks, for servers this project hasn't started.
    detected: HashMap<&'static str, Installed>,
    installs: HashMap<&'static str, InstallRun>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<PanelEvent> for LanguageServers {}

impl LanguageServers {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = Services::of(window, cx).workspace;
        let store = LspStore::for_workspace(&workspace, cx);
        let me = cx.weak_entity();
        cx.default_global::<Instances>()
            .0
            .insert(window.window_handle().window_id(), me);
        let mut this = Self {
            _subs: vec![cx.observe(&store, |_, _, cx| cx.notify())],
            store,
            detected: HashMap::new(),
            installs: HashMap::new(),
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
        };
        this.detect_all(cx);
        this
    }

    fn detect_all(&mut self, cx: &mut Context<Self>) {
        let specs = self.store.read(cx).specs().to_vec();
        let task = cx.background_executor().spawn(async move {
            specs
                .iter()
                .map(|s| (s.id, registry::detect(s)))
                .collect::<Vec<_>>()
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |this, cx| {
                this.detected = found.into_iter().collect();
                cx.notify();
            });
        }));
    }

    fn install(&mut self, spec: &ServerSpec, cx: &mut Context<Self>) {
        let Some(command) = spec.install.clone() else {
            return;
        };
        let id = spec.id;
        log::info!("installing {id}: {}", command.join(" "));
        self.installs.insert(
            id,
            InstallRun::Running {
                lines: vec![format!("$ {}", command.join(" "))],
            },
        );
        let mut run = pide_lsp::install::run(command);
        self._tasks.push(cx.spawn(async move |this, cx| {
            while let Some(line) = run.lines.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(InstallRun::Running { lines }) = this.installs.get_mut(id) {
                        lines.push(line);
                        cx.notify();
                    }
                });
            }
            let result = run.done.await.unwrap_or(Err("install stopped".into()));
            let _ = this.update(cx, |this, cx| {
                let lines = match this.installs.remove(id) {
                    Some(InstallRun::Running { lines }) => lines,
                    _ => vec![],
                };
                let (ok, summary) = match result {
                    Ok(Some(0)) => (true, "Installed.".to_string()),
                    Ok(Some(code)) => (false, format!("Failed (exit code {code}).")),
                    Ok(None) => (false, "Stopped.".to_string()),
                    Err(e) => (false, format!("Couldn't run it: {e}")),
                };
                this.installs
                    .insert(id, InstallRun::Done { ok, summary, lines });
                this.detect_all(cx);
                // Start it for open files of its language.
                this.store.update(cx, |s, cx| s.redetect(cx));
                cx.notify();
            });
        }));
    }

    fn render_server(&self, spec: &ServerSpec, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        let server = store.server(spec.id);
        let installed = server
            .and_then(|s| s.installed.clone())
            .or_else(|| self.detected.get(spec.id).cloned());
        let (dot, status) = match (server.map(|s| &s.state), &installed) {
            (Some(ServerState::Running), _) => (
                theme.success,
                format!("running · {}", server.unwrap().status_text()),
            ),
            (Some(ServerState::Starting), _) => (theme.warning, "starting".to_string()),
            (Some(ServerState::Crashed { message, .. }), _) => {
                (theme.danger, format!("crashed: {message}"))
            }
            (_, Some(Installed::Yes { .. })) => (
                theme.muted_foreground,
                format!(
                    "installed · starts when you open a .{} file",
                    spec.extensions[0]
                ),
            ),
            (_, Some(Installed::No { .. })) => (theme.danger, "not installed".to_string()),
            (_, None) => (theme.muted_foreground, "checking…".to_string()),
        };
        let detail = match &installed {
            Some(Installed::Yes { path, version }) => format!("{} · {version}", path.display()),
            Some(Installed::No { reason }) => reason.clone(),
            None => String::new(),
        };
        let crash_tail: Vec<String> = match server.map(|s| &s.state) {
            Some(ServerState::Crashed { stderr_tail, .. }) => {
                stderr_tail.iter().rev().take(6).rev().cloned().collect()
            }
            _ => vec![],
        };
        let can_restart = matches!(
            server.map(|s| &s.state),
            Some(ServerState::Running | ServerState::Crashed { .. } | ServerState::Starting)
        );
        let missing = matches!(installed, Some(Installed::No { .. }));
        let installing = matches!(self.installs.get(spec.id), Some(InstallRun::Running { .. }));
        let install_cmd = spec.install.as_ref().map(|c| c.join(" "));
        let id = spec.id;
        let spec_for_install = spec.clone();

        v_flex()
            .gap_2()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(spec.language))
                            .child(div().text_color(theme.muted_foreground).child(spec.id))
                            .when(spec.overridden, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("(from settings.json)"),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().size(px(8.)).rounded_full().bg(dot))
                            .child(div().text_sm().child(status)),
                    ),
            )
            .when(!detail.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(detail),
                )
            })
            .when(!crash_tail.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(crash_tail.join("\n")),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when_some(
                        install_cmd.filter(|_| missing || installing),
                        |this, cmd| {
                            this.child(div().text_sm().child("Install with"))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_family(theme.mono_font_family.clone())
                                        .child(cmd),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("install-{id}")))
                                        .small()
                                        .primary()
                                        .label(if installing {
                                            "Installing…"
                                        } else {
                                            "Install"
                                        })
                                        .disabled(installing)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.install(&spec_for_install, cx)
                                        })),
                                )
                        },
                    )
                    .when(can_restart, |this| {
                        this.child(
                            Button::new(SharedString::from(format!("restart-{id}")))
                                .small()
                                .label("Restart")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.store.update(cx, |s, cx| s.restart(id, cx))
                                })),
                        )
                    }),
            )
            .when_some(self.installs.get(spec.id), |this, run| {
                let (lines, summary, ok) = match run {
                    InstallRun::Running { lines } => (lines, None, true),
                    InstallRun::Done { ok, summary, lines } => (lines, Some(summary.clone()), *ok),
                };
                let tail: Vec<String> = lines.iter().rev().take(12).rev().cloned().collect();
                this.child(
                    v_flex()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.secondary)
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .child(tail.join("\n"))
                        .when_some(summary, |this, s| {
                            this.child(
                                div()
                                    .pt_1()
                                    .text_color(if ok { theme.success } else { theme.danger })
                                    .child(s),
                            )
                        }),
                )
            })
    }
}

impl Focusable for LanguageServers {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for LanguageServers {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for LanguageServers {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for LanguageServers {
    fn title_text(&self, _: &App) -> SharedString {
        "Language Servers".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

impl Render for LanguageServers {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let specs = self.store.read(cx).specs().to_vec();
        let settings = pide_core::settings::settings_file();
        v_flex()
            .id("language-servers")
            .size_full()
            .overflow_y_scroll()
            .track_focus(&self.focus)
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child("Language Servers"))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("refresh").small().label("Check again").on_click(cx.listener(|this, _, _, cx| this.detect_all(cx))))
                            .child(Button::new("settings").small().label("Open settings.json").on_click(move |_, window, cx| {
                                pide_core::opener::open_file(settings.clone(), window, cx)
                            })),
                    ),
            )
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Language servers give pide errors, completion, hover and go to definition. \
                 pide starts one when you open a file of its language. Override a server's command in \
                 settings.json: \"lsp\": { \"rust-analyzer\": { \"command\": \"…\", \"args\": [] } }",
            ))
            .children(specs.iter().map(|s| self.render_server(s, cx).into_any_element()).collect::<Vec<_>>())
    }
}
