//! pi Settings tab (spec M3.5): see and change pi's setup without editing its
//! files. Six sections, each for **Everyone** (`~/.pi/agent`) or **This
//! project** (`.pi/`). File reads and writes run off the UI thread; pi's own
//! commands (`pi install` …) stream their output into the tab.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_agent_pi::config::{
    self, InstructionFile, McpServer, Model, Package, PiCommand, PiConfig, Scope,
};
use pide_core::{DockPosition, Panel, Services, Sessions};
use serde_json::{Map, Value};

pub const PANEL_NAME: &str = "pi_settings";

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| PiSettings::new(window, cx)))
    });
}

#[derive(Default)]
struct Instances(std::collections::HashMap<WindowId, WeakEntity<PiSettings>>);
impl Global for Instances {}

/// This window's open pi Settings tab, if any.
pub fn existing(window: &Window, cx: &App) -> Option<Entity<PiSettings>> {
    cx.try_global::<Instances>()?
        .0
        .get(&window.window_handle().window_id())?
        .upgrade()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Packages,
    Tools,
    Skills,
    Mcp,
    Model,
    Instructions,
}

impl Section {
    const ALL: [Section; 6] = [
        Self::Packages,
        Self::Tools,
        Self::Skills,
        Self::Mcp,
        Self::Model,
        Self::Instructions,
    ];
    fn title(self) -> &'static str {
        match self {
            Self::Packages => "Packages",
            Self::Tools => "Tools & extensions",
            Self::Skills => "Skills & prompts",
            Self::Mcp => "MCP servers",
            Self::Model => "Model",
            Self::Instructions => "Instructions",
        }
    }
}

/// Everything read from disk for both scopes.
#[derive(Default)]
struct Snapshot {
    user: Map<String, Value>,
    project: Map<String, Value>,
    user_mcp: Map<String, Value>,
    project_mcp: Map<String, Value>,
    packages: Vec<Package>,
    local_extensions: Vec<(Scope, PathBuf)>,
    instructions: Vec<InstructionFile>,
    /// A file that couldn't be read (shown, never overwritten).
    errors: Vec<String>,
}

impl Snapshot {
    fn settings(&self, scope: Scope) -> &Map<String, Value> {
        match scope {
            Scope::User => &self.user,
            Scope::Project => &self.project,
        }
    }
    fn list(&self, scope: Scope, key: &str) -> Option<Vec<String>> {
        config::list_setting(self.settings(scope), key)
    }
    fn tools(&self) -> Vec<String> {
        config::resolve_tools(
            self.list(Scope::User, "defaultTools").as_deref(),
            self.list(Scope::Project, "defaultTools").as_deref(),
        )
    }
    fn mcp(&self) -> Vec<McpServer> {
        let mut v = config::mcp_servers(Scope::User, &self.user_mcp);
        v.extend(config::mcp_servers(Scope::Project, &self.project_mcp));
        v
    }
    fn string(&self, key: &str) -> (Option<String>, Option<Scope>) {
        for scope in [Scope::Project, Scope::User] {
            if let Some(s) = self.settings(scope).get(key).and_then(Value::as_str) {
                return (Some(s.to_string()), Some(scope));
            }
        }
        (None, None)
    }
}

/// A pi command running for the tab (`pi install …`).
struct Run {
    title: String,
    lines: Vec<String>,
    done: Option<(bool, String)>,
}

pub struct PiSettings {
    cfg: PiConfig,
    sessions: Entity<Sessions>,
    scope: Scope,
    section: Section,
    snap: Snapshot,
    loaded: bool,
    models: Option<Result<Vec<Model>, String>>,
    commands: Option<Result<Vec<PiCommand>, String>>,
    run: Option<Run>,
    /// Settings changed since the sessions last restarted.
    changed: bool,
    package_input: Entity<InputState>,
    mcp_name: Entity<InputState>,
    mcp_target: Entity<InputState>,
    model_filter: Entity<InputState>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for PiSettings {}

impl PiSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Services {
            workspace,
            sessions,
        } = Services::of(window, cx);
        let root = workspace.read(cx).root().to_path_buf();
        let me = cx.weak_entity();
        cx.default_global::<Instances>()
            .0
            .insert(window.window_handle().window_id(), me);
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let model_filter = input("Filter models…", window, cx);
        let mut this = Self {
            cfg: PiConfig::new(&root),
            sessions,
            scope: Scope::User,
            section: Section::Packages,
            snap: Snapshot::default(),
            loaded: false,
            models: None,
            commands: None,
            run: None,
            changed: false,
            package_input: input(
                "npm:package-name, git:github.com/user/repo, or a folder",
                window,
                cx,
            ),
            mcp_name: input("name (letters, digits, - and _)", window, cx),
            mcp_target: input("command and arguments, or https:// URL", window, cx),
            _subs: vec![cx.observe(&model_filter, |_, _, cx| cx.notify())],
            model_filter,
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
        };
        this.reload(cx);
        this
    }

    /// Re-read everything from disk (off the UI thread).
    fn reload(&mut self, cx: &mut Context<Self>) {
        let cfg = self.cfg.clone();
        let task = cx.background_executor().spawn(async move {
            let mut s = Snapshot::default();
            let mut read = |path: PathBuf| match config::read_json(&path) {
                Ok(m) => m,
                Err(e) => {
                    s.errors.push(format!("{e:#}"));
                    Map::new()
                }
            };
            let user = read(cfg.settings_path(Scope::User));
            let project = read(cfg.settings_path(Scope::Project));
            let user_mcp = read(cfg.mcp_path(Scope::User));
            let project_mcp = read(cfg.mcp_path(Scope::Project));
            s.packages = config::packages(&cfg, Scope::User, &user);
            s.packages
                .extend(config::packages(&cfg, Scope::Project, &project));
            for scope in [Scope::User, Scope::Project] {
                s.local_extensions.extend(
                    config::local_extensions(&cfg, scope)
                        .into_iter()
                        .map(|p| (scope, p)),
                );
            }
            s.instructions = config::instruction_files(&cfg);
            (s.user, s.project, s.user_mcp, s.project_mcp) = (user, project, user_mcp, project_mcp);
            s
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let snap = task.await;
            let _ = this.update(cx, |this, cx| {
                this.snap = snap;
                this.loaded = true;
                cx.notify();
            });
        }));
    }

    fn load_models(&mut self, cx: &mut Context<Self>) {
        if self.models.is_some() {
            return;
        }
        self.models = Some(Ok(vec![]));
        let root = self.cfg.root.clone();
        let task = cx
            .background_executor()
            .spawn(async move { config::list_models(&root).map_err(|e| format!("{e:#}")) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.models = Some(r);
                cx.notify();
            });
        }));
    }

    fn load_commands(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.commands.is_some() && !force {
            return;
        }
        self.commands = None;
        let root = self.cfg.root.clone();
        let task = cx
            .background_executor()
            .spawn(async move { config::list_commands(&root).map_err(|e| format!("{e:#}")) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.commands = Some(r);
                cx.notify();
            });
        }));
    }

    fn show(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        match section {
            Section::Model => self.load_models(cx),
            Section::Skills => self.load_commands(false, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Change the current scope's `settings.json`, then reload.
    fn edit_settings(
        &mut self,
        f: impl FnOnce(&mut Map<String, Value>) + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (cfg, scope) = (self.cfg.clone(), self.scope);
        self.write(move || cfg.update_settings(scope, f), window, cx);
    }

    fn edit_mcp(
        &mut self,
        scope: Scope,
        f: impl FnOnce(&mut Map<String, Value>) + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cfg = self.cfg.clone();
        self.write(move || cfg.update_mcp(scope, f), window, cx);
    }

    fn write(
        &mut self,
        op: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = cx.background_executor().spawn(async move { op() });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match r {
                    Ok(()) => this.changed = true,
                    Err(e) => window.push_notification(
                        Notification::error(format!("Couldn't save: {e:#}")),
                        cx,
                    ),
                }
                this.reload(cx);
            });
        }));
    }

    /// Run `pi <args>` in the project root, output streamed into the tab.
    fn run_pi(&mut self, title: String, args: Vec<String>, cx: &mut Context<Self>) {
        let Some(pi) = config::find_pi() else {
            self.run = Some(Run {
                title,
                lines: vec![],
                done: Some((false, "pi isn't installed.".into())),
            });
            cx.notify();
            return;
        };
        let mut command = vec![pi.display().to_string()];
        command.extend(args.iter().cloned());
        self.run = Some(Run {
            title,
            lines: vec![format!("$ pi {}", args.join(" "))],
            done: None,
        });
        let mut run = pide_runtime::process::run_streaming(command, Some(self.cfg.root.clone()));
        self._tasks.push(cx.spawn(async move |this, cx| {
            while let Some(line) = run.lines.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(r) = this.run.as_mut() {
                        r.lines.push(line);
                        cx.notify();
                    }
                });
            }
            let result = run.done.await.unwrap_or(Err("stopped".into()));
            let _ = this.update(cx, |this, cx| {
                let done = match result {
                    Ok(Some(0)) => (true, "Done.".to_string()),
                    Ok(Some(c)) => (false, format!("Failed (exit code {c}).")),
                    Ok(None) => (false, "Stopped.".to_string()),
                    Err(e) => (false, format!("Couldn't run pi: {e}")),
                };
                if done.0 {
                    this.changed = true;
                    this.commands = None; // packages bring skills and commands
                }
                if let Some(r) = this.run.as_mut() {
                    r.done = Some(done);
                }
                this.reload(cx);
            });
        }));
        cx.notify();
    }

    fn local_flag(&self) -> Vec<String> {
        if self.scope == Scope::Project {
            vec!["--local".into()]
        } else {
            vec![]
        }
    }

    fn open(path: PathBuf, window: &mut Window, cx: &mut App) {
        pide_core::opener::open_file(path, window, cx);
    }
}

// ---------------------------------------------------------------------------
// Rendering

/// `/Users/ja/x` -> `~/x`, for display.
fn tilde(path: &std::path::Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap().display())
        }
        _ => path.display().to_string(),
    }
}

/// The command name pi gives a skill (its folder) or prompt (its file name).
fn resource_name(kind: &str, path: &std::path::Path) -> String {
    let stem = |p: &std::path::Path| {
        p.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    match kind {
        "skill" if path.file_name().is_some_and(|n| n == "SKILL.md") => {
            format!("skill:{}", path.parent().map(&stem).unwrap_or_default())
        }
        "skill" => format!("skill:{}", stem(path)),
        _ => stem(path),
    }
}

fn section_box(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .gap_2()
        .p_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
}

fn muted(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn mono(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .font_family(cx.theme().mono_font_family.clone())
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

impl PiSettings {
    fn render_packages(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.scope;
        let pkgs: Vec<Package> = self
            .snap
            .packages
            .iter()
            .filter(|p| p.scope == scope)
            .cloned()
            .collect();
        let other = self
            .snap
            .packages
            .iter()
            .filter(|p| p.scope != scope)
            .count();
        let busy = self.run.as_ref().is_some_and(|r| r.done.is_none());
        let mut col = v_flex().gap_3();
        col = col.child(muted(
            "Packages bundle extensions, skills, prompt templates and themes. pide runs pi's own install, update and remove.",
            cx,
        ));
        for p in pkgs {
            let (src_u, src_r) = (p.source.clone(), p.source.clone());
            let summary = {
                let mut counts: Vec<String> = Vec::new();
                for kind in ["extensions", "skills", "prompts", "themes"] {
                    let n = p.resources.iter().filter(|(k, _)| *k == kind).count();
                    if n > 0 {
                        counts.push(format!(
                            "{n} {}",
                            if n == 1 {
                                kind.trim_end_matches('s')
                            } else {
                                kind
                            }
                        ));
                    }
                }
                counts.join(" · ")
            };
            let local = self.local_flag();
            let local2 = local.clone();
            col = col.child(
                section_box(cx)
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(p.display.clone()))
                                    .child(muted(format!("{} {}", p.kind, p.version.clone().unwrap_or_default()), cx)),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .when(p.kind != "local", |this| {
                                        this.child(Button::new(SharedString::from(format!("upd-{}", p.source))).small().label("Update").disabled(busy).on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.run_pi(format!("Updating {src_u}"), vec!["update".into(), src_u.clone()], cx)
                                            }),
                                        ))
                                    })
                                    .child(Button::new(SharedString::from(format!("rm-{}", p.source))).small().label("Remove").disabled(busy).on_click({
                                        let me = cx.entity().downgrade();
                                        let shown = p.display.clone();
                                        move |_, window, cx| {
                                            let (me, src, local, shown) = (me.clone(), src_r.clone(), local2.clone(), shown.clone());
                                            window.open_alert_dialog(cx, move |alert, _, _| {
                                                let (me, src, local) = (me.clone(), src.clone(), local.clone());
                                                alert
                                                    .title(format!("Remove {shown}?"))
                                                    .description("pi uninstalls it and removes it from settings.json. Its extensions, skills and prompts stop loading in new sessions.")
                                                    .confirm()
                                                    .ok_text("Remove")
                                                    .on_ok(move |_, _, cx| {
                                                        let mut args = vec!["remove".into(), src.clone()];
                                                        args.extend(local.clone());
                                                        let src = src.clone();
                                                        let _ = me.update(cx, |this, cx| this.run_pi(format!("Removing {src}"), args, cx));
                                                        true
                                                    })
                                            });
                                        }
                                    })),
                            ),
                    )
                    .when_some(p.description.clone(), |this, d| this.child(muted(d, cx).line_clamp(2)))
                    .child(mono(
                        if p.installed_at.is_some() { if summary.is_empty() { "no resources found".into() } else { summary } } else { "not installed yet (pi installs it on next start)".to_string() },
                        cx,
                    ))
                    .when(!p.resources.is_empty(), |this| {
                        this.child(mono(p.resources.iter().map(|(_, r)| r.as_str()).collect::<Vec<_>>().join("   "), cx))
                    })
                    .into_any_element(),
            );
            let _ = local;
        }
        if other > 0 {
            col = col.child(muted(
                format!(
                    "{other} more package{} under {}.",
                    if other == 1 { "" } else { "s" },
                    match scope {
                        Scope::User => Scope::Project.label(),
                        Scope::Project => Scope::User.label(),
                    }
                ),
                cx,
            ));
        }
        let input = self.package_input.clone();
        col.child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(&self.package_input).small()),
                )
                .child(
                    Button::new("install-pkg")
                        .small()
                        .primary()
                        .label("Install")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let src = input.read(cx).value().trim().to_string();
                            if src.is_empty() {
                                return;
                            }
                            let mut args = vec!["install".into(), src.clone()];
                            args.extend(this.local_flag());
                            this.run_pi(format!("Installing {src}"), args, cx);
                            input.update(cx, |i, cx| i.set_value("", window, cx));
                        })),
                ),
        )
        .child(muted(
            "Only install packages you trust: they run code in pi.",
            cx,
        ))
        .into_any_element()
    }

    fn render_tools(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.scope;
        let on = self.snap.tools();
        let here = self.snap.list(scope, "defaultTools").unwrap_or_default();
        let theme = cx.theme().clone();
        let mut tools = v_flex().gap_1();
        for (name, what) in config::BUILTIN_TOOLS {
            let enabled = on.iter().any(|t| t == name);
            let set_here = here
                .iter()
                .any(|e| e.trim_start_matches(['+', '-']) == *name);
            let name_s = name.to_string();
            let this = cx.entity().downgrade();
            tools = tools.child(
                h_flex()
                    .gap_3()
                    .py_0p5()
                    .child(
                        Switch::new(SharedString::from(format!("tool-{name}")))
                            .checked(enabled)
                            .on_click(move |checked, window, cx| {
                                let (name, checked) = (name_s.clone(), *checked);
                                let _ = this.update(cx, |this, cx| {
                                    let other = match this.scope {
                                        Scope::User => None,
                                        Scope::Project => {
                                            this.snap.list(Scope::User, "defaultTools")
                                        }
                                    };
                                    let inherited = config::resolve_tools(other.as_deref(), None);
                                    let current = this.snap.list(this.scope, "defaultTools");
                                    let next = config::set_tool(
                                        current.as_deref(),
                                        &inherited,
                                        &name,
                                        checked,
                                    );
                                    this.edit_settings(
                                        move |m| config::put_list(m, "defaultTools", next),
                                        window,
                                        cx,
                                    );
                                });
                            }),
                    )
                    .child(
                        div()
                            .w(px(130.))
                            .flex_none()
                            .font_family(theme.mono_font_family.clone())
                            .text_sm()
                            .child(name.to_string()),
                    )
                    .child(muted(what.to_string(), cx).flex_1())
                    .when(set_here, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(pide_core::colors::accent(cx))
                                .child(format!("set for {}", scope.label().to_lowercase())),
                        )
                    }),
            );
        }
        let mut exts = v_flex().gap_1();
        for (name, what) in config::BUILTIN_EXTENSIONS {
            let user = self.snap.list(Scope::User, "extensions");
            let project = self.snap.list(Scope::Project, "extensions");
            let enabled = config::builtin_extension_on(name, user.as_deref(), project.as_deref());
            let inherited_on = match scope {
                Scope::User => true,
                Scope::Project => config::builtin_extension_on(name, user.as_deref(), None),
            };
            let name_s = name.to_string();
            let this = cx.entity().downgrade();
            exts = exts.child(
                h_flex()
                    .gap_3()
                    .py_0p5()
                    .child(
                        Switch::new(SharedString::from(format!("ext-{name}")))
                            .checked(enabled)
                            .on_click(move |checked, window, cx| {
                                let (name, checked) = (name_s.clone(), *checked);
                                let _ = this.update(cx, |this, cx| {
                                    let current = this.snap.list(this.scope, "extensions");
                                    let next = config::set_builtin_extension(
                                        current.as_deref(),
                                        &name,
                                        checked,
                                        inherited_on,
                                    );
                                    this.edit_settings(
                                        move |m| config::put_list(m, "extensions", next),
                                        window,
                                        cx,
                                    );
                                });
                            }),
                    )
                    .child(
                        div()
                            .w(px(130.))
                            .flex_none()
                            .font_family(theme.mono_font_family.clone())
                            .text_sm()
                            .child(name.to_string()),
                    )
                    .child(muted(what.to_string(), cx).flex_1()),
            );
        }
        let mut local = v_flex().gap_1();
        let locals: Vec<(Scope, PathBuf)> = self.snap.local_extensions.clone();
        for (s, path) in locals {
            let p = path.clone();
            local = local.child(
                h_flex()
                    .gap_2()
                    .child(mono(
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        cx,
                    ))
                    .child(muted(s.label(), cx))
                    .child(
                        Button::new(SharedString::from(format!("open-{}", path.display())))
                            .xsmall()
                            .ghost()
                            .label("Open")
                            .on_click(move |_, window, cx| Self::open(p.clone(), window, cx)),
                    ),
            );
        }
        let has_local = !self.snap.local_extensions.is_empty();
        v_flex()
            .gap_3()
            .child(muted(
                "Tools pi may use at the start of a session, and pi's built-in extensions. Package extensions come and go with their package.",
                cx,
            ))
            .child(section_box(cx).child(div().font_weight(FontWeight::SEMIBOLD).child("Built-in tools")).child(tools))
            .child(section_box(cx).child(div().font_weight(FontWeight::SEMIBOLD).child("Built-in extensions")).child(exts))
            .when(has_local, |this| {
                this.child(section_box(cx).child(div().font_weight(FontWeight::SEMIBOLD).child("Your extension files")).child(local))
            })
            .into_any_element()
    }

    fn render_skills(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.scope;
        let mut col = v_flex().gap_3().child(
            h_flex()
                .justify_between()
                .gap_3()
                .child(muted("What pi loads in this project: skills, prompt templates (slash commands) and extension commands.", cx).flex_1().min_w_0())
                .child(Button::new("refresh-cmds").small().label("Refresh").on_click(cx.listener(|this, _, _, cx| this.load_commands(true, cx)))),
        );
        let cmds = match &self.commands {
            None => {
                return col
                    .child(muted("Asking pi… (it starts once to list them)", cx))
                    .into_any_element();
            }
            Some(Err(e)) => {
                return col
                    .child(muted(format!("Couldn't ask pi: {e}"), cx))
                    .into_any_element();
            }
            Some(Ok(c)) => c.clone(),
        };
        for (kind, title, key) in [
            ("skill", "Skills", "skills"),
            ("prompt", "Prompt templates", "prompts"),
            ("extension", "Extension commands", ""),
        ] {
            let items: Vec<&PiCommand> = cmds.iter().filter(|c| c.source == kind).collect();
            let disabled = if key.is_empty() {
                vec![]
            } else {
                config::disabled_resources(self.snap.list(scope, key).as_deref())
            };
            let has_disabled = !disabled.is_empty();
            let mut rows = v_flex().gap_1();
            for c in &items {
                let path = c.source_info.path.clone();
                let from = match (c.source_info.origin.as_str(), c.source_info.scope.as_str()) {
                    ("package", _) => c.source_info.source.clone(),
                    (_, "temporary") => "built-in".to_string(),
                    (_, scope) => scope.to_string(),
                };
                let mut row = h_flex().gap_3().py_0p5();
                if !key.is_empty() {
                    let this = cx.entity().downgrade();
                    let (p, key) = (path.clone(), key.to_string());
                    row = row.child(
                        Switch::new(SharedString::from(format!("res-{}", path.display())))
                            .checked(true)
                            .on_click(move |checked, window, cx| {
                                let (p, key, on) = (p.clone(), key.clone(), *checked);
                                let _ = this.update(cx, |this, cx| {
                                    let next = config::set_resource(
                                        this.snap.list(this.scope, &key).as_deref(),
                                        &p,
                                        on,
                                    );
                                    this.edit_settings(
                                        move |m| config::put_list(m, &key, next),
                                        window,
                                        cx,
                                    );
                                    this.commands = None;
                                    this.load_commands(true, cx);
                                });
                            }),
                    );
                }
                let p2 = path.clone();
                rows = rows.child(
                    row.child(
                        div()
                            .w(px(180.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_sm()
                            .child(format!("/{}", c.name)),
                    )
                    .child(
                        muted(c.description.clone().unwrap_or_default(), cx)
                            .flex_1()
                            .line_clamp(1),
                    )
                    .child(mono(from, cx))
                    .child(
                        Button::new(SharedString::from(format!(
                            "o-{}-{}",
                            c.name,
                            path.display()
                        )))
                        .xsmall()
                        .ghost()
                        .label("Open")
                        .on_click(move |_, window, cx| Self::open(p2.clone(), window, cx)),
                    ),
                );
            }
            for path in disabled {
                let this = cx.entity().downgrade();
                let (p, key_s) = (path.clone(), key.to_string());
                rows = rows.child(
                    h_flex()
                        .gap_3()
                        .py_0p5()
                        .child(
                            Switch::new(SharedString::from(format!("off-{}", path.display())))
                                .checked(false)
                                .on_click(move |checked, window, cx| {
                                    let (p, key, on) = (p.clone(), key_s.clone(), *checked);
                                    let _ = this.update(cx, |this, cx| {
                                        let next = config::set_resource(
                                            this.snap.list(this.scope, &key).as_deref(),
                                            &p,
                                            on,
                                        );
                                        this.edit_settings(
                                            move |m| config::put_list(m, &key, next),
                                            window,
                                            cx,
                                        );
                                        this.load_commands(true, cx);
                                    });
                                }),
                        )
                        .child(
                            div()
                                .w(px(180.))
                                .flex_none()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("/{}", resource_name(kind, &path))),
                        )
                        .child(
                            muted(format!("off for {}", scope.label().to_lowercase()), cx).flex_1(),
                        )
                        .child(
                            mono(tilde(&path), cx)
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap(),
                        ),
                );
            }
            col = col.child(
                section_box(cx)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(muted(items.len().to_string(), cx)),
                    )
                    .when(items.is_empty() && !has_disabled, |this| {
                        this.child(muted(
                            match kind {
                                "skill" => format!(
                                    "None. Put skills in {}/skills/ or install a package.",
                                    tilde(&self.cfg.dir(scope))
                                ),
                                "prompt" => format!(
                                    "None. Put Markdown files in {}/prompts/.",
                                    tilde(&self.cfg.dir(scope))
                                ),
                                _ => "None.".into(),
                            },
                            cx,
                        ))
                    })
                    .child(rows),
            );
        }
        col.into_any_element()
    }

    fn render_mcp(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let servers = self.snap.mcp();
        let mcp_off = !config::builtin_extension_on(
            "mcp",
            self.snap.list(Scope::User, "extensions").as_deref(),
            self.snap.list(Scope::Project, "extensions").as_deref(),
        );
        let mut col = v_flex().gap_3().child(muted(
            "MCP servers give pi more tools. Keep servers with credentials under Everyone; a project entry with the same name replaces it.",
            cx,
        ));
        if mcp_off {
            col = col.child(
                muted(
                    "The built-in MCP extension is off (Tools & extensions), so pi ignores these.",
                    cx,
                )
                .text_color(cx.theme().warning),
            );
        }
        let mut rows = v_flex().gap_1();
        for s in &servers {
            let this = cx.entity().downgrade();
            let this2 = this.clone();
            let (name, scope) = (s.name.clone(), s.scope);
            let name2 = name.clone();
            rows = rows.child(
                h_flex()
                    .gap_3()
                    .py_1()
                    .child(
                        Switch::new(SharedString::from(format!("mcp-{:?}-{}", s.scope, s.name)))
                            .checked(s.enabled)
                            .on_click(move |checked, window, cx| {
                                let (name, on) = (name.clone(), *checked);
                                let _ = this.update(cx, |this, cx| {
                                    this.edit_mcp(
                                        scope,
                                        move |servers| {
                                            if let Some(Value::Object(o)) = servers.get_mut(&name) {
                                                if on {
                                                    o.remove("enabled");
                                                } else {
                                                    o.insert("enabled".into(), Value::Bool(false));
                                                }
                                            }
                                        },
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_sm()
                                            .child(s.name.clone()),
                                    )
                                    .child(muted(
                                        format!(
                                            "{} · {}",
                                            if s.http { "http" } else { "stdio" },
                                            s.scope.label()
                                        ),
                                        cx,
                                    )),
                            )
                            .child(mono(s.target.clone(), cx))
                            .when_some(s.description.clone(), |t, d| t.child(muted(d, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "mcp-rm-{:?}-{}",
                            s.scope, s.name
                        )))
                        .xsmall()
                        .ghost()
                        .label("Remove")
                        .on_click(move |_, window, cx| {
                            let name = name2.clone();
                            let this = this2.clone();
                            window.open_alert_dialog(cx, move |alert, _, _| {
                                let (name, this) = (name.clone(), this.clone());
                                alert
                                    .title(format!("Remove MCP server {name}?"))
                                    .description(format!(
                                        "It's deleted from {}'s mcp.json.",
                                        scope.label()
                                    ))
                                    .confirm()
                                    .ok_text("Remove")
                                    .on_ok(move |_, window, cx| {
                                        let name = name.clone();
                                        let _ = this.update(cx, |this, cx| {
                                            this.edit_mcp(
                                                scope,
                                                move |s| {
                                                    s.remove(&name);
                                                },
                                                window,
                                                cx,
                                            )
                                        });
                                        true
                                    })
                            });
                        }),
                    ),
            );
        }
        if servers.is_empty() {
            rows = rows.child(muted("No MCP servers yet.", cx));
        }
        let (name_in, target_in) = (self.mcp_name.clone(), self.mcp_target.clone());
        col.child(section_box(cx).child(rows))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().w(px(180.)).child(Input::new(&self.mcp_name).small()))
                    .child(div().flex_1().child(Input::new(&self.mcp_target).small()))
                    .child(Button::new("mcp-add").small().primary().label(format!("Add to {}", self.scope.label())).on_click(cx.listener(
                        move |this, _, window, cx| {
                            let name = name_in.read(cx).value().trim().to_string();
                            let target = target_in.read(cx).value().trim().to_string();
                            if !config::valid_mcp_name(&name) || target.is_empty() {
                                window.push_notification(
                                    Notification::warning("Give the server a name (letters, digits, - and _) and a command or URL."),
                                    cx,
                                );
                                return;
                            }
                            let entry = config::mcp_entry(&target);
                            let scope = this.scope;
                            this.edit_mcp(scope, move |s| {
                                s.insert(name, entry);
                            }, window, cx);
                            name_in.update(cx, |i, cx| i.set_value("", window, cx));
                            target_in.update(cx, |i, cx| i.set_value("", window, cx));
                        },
                    ))),
            )
            .child(
                h_flex().gap_2().child(Button::new("open-mcp").small().ghost().label("Open mcp.json").on_click({
                    let path = self.cfg.mcp_path(self.scope);
                    move |_, window, cx| Self::open(path.clone(), window, cx)
                })),
            )
            .into_any_element()
    }

    fn render_model(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (provider, p_scope) = self.snap.string("defaultProvider");
        let (model, _) = self.snap.string("defaultModel");
        let (thinking, _) = self.snap.string("defaultThinkingLevel");
        let current = match (&provider, &model) {
            (Some(p), Some(m)) => format!("{p} / {m}"),
            (None, Some(m)) => m.clone(),
            _ => "pi chooses (no default set)".into(),
        };
        let thinking = thinking.unwrap_or_else(|| "medium".into());
        let theme = cx.theme().clone();
        let mut levels = h_flex().gap_1();
        for level in config::THINKING_LEVELS {
            let l = level.to_string();
            levels = levels.child(
                Button::new(SharedString::from(format!("think-{level}")))
                    .small()
                    .label(level.to_string())
                    .map(|b| pide_core::colors::toggle(b, *level == thinking, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let l = l.clone();
                        this.edit_settings(
                            move |m| {
                                m.insert("defaultThinkingLevel".into(), Value::String(l));
                            },
                            window,
                            cx,
                        )
                    })),
            );
        }
        let filter = self.model_filter.read(cx).value().to_lowercase();
        let mut list = v_flex().gap_0p5();
        match &self.models {
            None | Some(Ok(_))
                if self
                    .models
                    .as_ref()
                    .is_none_or(|m| m.as_ref().is_ok_and(|v| v.is_empty())) =>
            {
                list = list.child(muted("Asking pi for its models…", cx));
            }
            Some(Err(e)) => list = list.child(muted(format!("Couldn't list models: {e}"), cx)),
            Some(Ok(models)) => {
                let words: Vec<&str> = filter.split_whitespace().collect();
                let matches: Vec<&Model> = models
                    .iter()
                    .filter(|m| {
                        let hay = format!("{} {}", m.provider, m.id).to_lowercase();
                        words.iter().all(|w| hay.contains(w))
                    })
                    .collect();
                let shown = matches.len().min(60);
                // One header per provider (in pi's order of first appearance),
                // its models under it, so the name gets the row's whole width.
                let mut order: Vec<&str> = Vec::new();
                for m in &matches[..shown] {
                    if !order.contains(&m.provider.as_str()) {
                        order.push(m.provider.as_str());
                    }
                }
                let grouped: Vec<&Model> = order
                    .iter()
                    .flat_map(|p| {
                        matches[..shown]
                            .iter()
                            .copied()
                            .filter(move |m| m.provider == *p)
                    })
                    .collect();
                let mut last_provider: Option<&str> = None;
                for m in grouped {
                    if last_provider != Some(m.provider.as_str()) {
                        last_provider = Some(m.provider.as_str());
                        list = list.child(
                            div()
                                .pt_2()
                                .px_2()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.muted_foreground)
                                .child(m.provider.clone()),
                        );
                    }
                    let (p, id) = (m.provider.clone(), m.id.clone());
                    let is_current = provider.as_deref() == Some(m.provider.as_str())
                        && model.as_deref() == Some(m.id.as_str());
                    list = list.child(
                        h_flex()
                            .id(SharedString::from(format!("m-{}-{}", m.provider, m.id)))
                            .gap_3()
                            .px_2()
                            .py_0p5()
                            .rounded(theme.radius)
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.list_hover))
                            .when(is_current, |s| s.bg(theme.list_active))
                            .pl_4()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    // Model ids differ at both ends
                                    // (anthropic.claude-…-sonnet-4-5).
                                    .text_ellipsis_middle()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(m.id.clone()),
                            )
                            .when_some(m.note.clone(), |t, n| {
                                t.child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(theme.warning)
                                        .child(n),
                                )
                            })
                            .child(
                                mono(
                                    format!(
                                        "{}{}",
                                        m.context,
                                        if m.thinking { " · thinking" } else { "" }
                                    ),
                                    cx,
                                )
                                .flex_none(),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let (p, id) = (p.clone(), id.clone());
                                this.edit_settings(
                                    move |m| {
                                        m.insert("defaultProvider".into(), Value::String(p));
                                        m.insert("defaultModel".into(), Value::String(id));
                                    },
                                    window,
                                    cx,
                                )
                            })),
                    );
                }
                if matches.len() > shown {
                    list = list.child(muted(
                        format!("{} more; type to narrow.", matches.len() - shown),
                        cx,
                    ));
                }
            }
            _ => {}
        }
        v_flex()
            .gap_3()
            .child(
                section_box(cx)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Default model"),
                            )
                            .child(muted(current, cx)),
                    )
                    .when_some(p_scope, |this, s| {
                        this.child(muted(format!("Set for {}.", s.label().to_lowercase()), cx))
                    })
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .pt_2()
                            .child("Thinking"),
                    )
                    .child(levels),
            )
            .child(Input::new(&self.model_filter).small())
            .child(list)
            .into_any_element()
    }

    fn render_instructions(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.scope;
        let files: Vec<InstructionFile> = self
            .snap
            .instructions
            .iter()
            .filter(|f| f.scope == scope)
            .cloned()
            .collect();
        let mut col = v_flex().gap_3().child(muted(
            "Instructions pi reads with every prompt. AGENTS.md files in parent folders of the project apply too.",
            cx,
        ));
        for f in files {
            let path = f.path.clone();
            let exists = f.exists;
            col = col.child(
                section_box(cx).child(
                    h_flex()
                        .justify_between()
                        .gap_3()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div().font_weight(FontWeight::SEMIBOLD).child(
                                        f.path
                                            .file_name()
                                            .map(|n| n.to_string_lossy().to_string())
                                            .unwrap_or_default(),
                                    ),
                                )
                                .child(muted(f.role, cx))
                                .child(
                                    mono(tilde(&f.path), cx)
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap(),
                                ),
                        )
                        .child(
                            Button::new(SharedString::from(format!("i-{}", f.path.display())))
                                .small()
                                .label(if exists { "Open" } else { "Create" })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let path = path.clone();
                                    if exists {
                                        Self::open(path, window, cx);
                                        return;
                                    }
                                    let task = cx.background_executor().spawn({
                                        let path = path.clone();
                                        async move {
                                            if let Some(d) = path.parent() {
                                                std::fs::create_dir_all(d)?;
                                            }
                                            std::fs::write(&path, "")
                                        }
                                    });
                                    this._tasks.push(cx.spawn_in(window, async move |this, cx| {
                                        let r = task.await;
                                        let _ = this.update_in(cx, |this, window, cx| {
                                            match r {
                                                Ok(()) => Self::open(path, window, cx),
                                                Err(e) => window.push_notification(
                                                    Notification::error(format!(
                                                        "Couldn't create it: {e}"
                                                    )),
                                                    cx,
                                                ),
                                            }
                                            this.reload(cx);
                                        });
                                    }));
                                })),
                        ),
                ),
            );
        }
        col.into_any_element()
    }
}

impl Focusable for PiSettings {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for PiSettings {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for PiSettings {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for PiSettings {
    fn title_text(&self, _: &App) -> SharedString {
        "pi Settings".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

impl Render for PiSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let scope = self.scope;
        let mut nav = v_flex()
            .gap_0p5()
            .w(px(190.))
            .h_full()
            .flex_none()
            .p_2()
            .border_r_1()
            .border_color(theme.border);
        for s in Section::ALL {
            let count = match s {
                Section::Packages => Some(
                    self.snap
                        .packages
                        .iter()
                        .filter(|p| p.scope == scope)
                        .count(),
                ),
                Section::Mcp => Some(self.snap.mcp().len()),
                _ => None,
            };
            nav = nav.child(
                h_flex()
                    .id(SharedString::from(format!("nav-{}", s.title())))
                    .justify_between()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .text_sm()
                    .hover(|st| st.bg(theme.list_hover))
                    .when(self.section == s, |st| {
                        st.bg(theme.list_active).font_weight(FontWeight::SEMIBOLD)
                    })
                    .child(s.title())
                    .when_some(count.filter(|c| *c > 0), |t, c| {
                        t.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(c.to_string()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.show(s, cx))),
            );
        }
        let body = if !self.loaded {
            muted("Reading pi's settings…", cx).into_any_element()
        } else {
            match self.section {
                Section::Packages => self.render_packages(cx),
                Section::Tools => self.render_tools(cx),
                Section::Skills => self.render_skills(cx),
                Section::Mcp => self.render_mcp(cx),
                Section::Model => self.render_model(cx),
                Section::Instructions => self.render_instructions(cx),
            }
        };
        let errors = self.snap.errors.clone();
        let settings_path = self.cfg.settings_path(scope);

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child("pi Settings"))
                    .child(
                        h_flex()
                            .gap_1()
                            .children([Scope::User, Scope::Project].map(|s| {
                                Button::new(SharedString::from(format!("scope-{}", s.label())))
                                    .small()
                                    .label(s.label())
                                    .map(|b| pide_core::colors::toggle(b, scope == s, cx))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.scope = s;
                                        cx.notify();
                                    }))
                            }))
                            .child(Button::new("open-settings-json").small().ghost().label("settings.json").on_click(move |_, window, cx| {
                                Self::open(settings_path.clone(), window, cx)
                            })),
                    ),
            )
            .when(self.changed, |this| {
                this.child(
                    h_flex()
                        .gap_3()
                        .px_4()
                        .py_1p5()
                        .bg(theme.secondary)
                        .border_b_1()
                        .border_color(theme.border)
                        .text_sm()
                        .child(div().flex_1().child("Saved. pi reads these when a session starts; restart the sessions to use them now."))
                        .child(Button::new("restart-sessions").small().primary().label("Restart sessions").on_click(cx.listener(
                            |this, _, window, cx| {
                                this.sessions.update(cx, |s, cx| s.reload_config(cx));
                                this.changed = false;
                                window.push_notification(Notification::info("Sessions restart now (working ones when they finish)."), cx);
                                cx.notify();
                            },
                        ))),
                )
            })
            .when(scope == Scope::Project, |this| {
                this.child(div().px_4().pt_2().child(muted(
                    format!("Project settings live in {}/.pi and apply once you trust the project in pi.", tilde(&self.cfg.root)),
                    cx,
                )))
            })
            .children(errors.into_iter().map(|e| div().px_4().pt_2().text_sm().text_color(theme.danger).child(e)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(nav)
                    .child(
                        v_flex()
                            .id("pi-settings-body")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .p_4()
                            .gap_3()
                            .child(body)
                            .when_some(self.run.as_ref(), |this, run| {
                                let tail: Vec<String> = run.lines.iter().rev().take(14).rev().cloned().collect();
                                this.child(
                                    v_flex()
                                        .gap_1()
                                        .p_2()
                                        .rounded(theme.radius)
                                        .bg(theme.secondary)
                                        .child(
                                            h_flex()
                                                .justify_between()
                                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(run.title.clone()))
                                                .when(run.done.is_some(), |t| {
                                                    t.child(Button::new("run-close").xsmall().ghost().label("Close").on_click(cx.listener(|this, _, _, cx| {
                                                        this.run = None;
                                                        cx.notify();
                                                    })))
                                                }),
                                        )
                                        .child(mono(tail.join("\n"), cx))
                                        .when_some(run.done.clone(), |t, (ok, msg)| {
                                            t.child(div().text_sm().text_color(if ok { theme.success } else { theme.danger }).child(msg))
                                        }),
                                )
                            }),
                    ),
            )
    }
}
