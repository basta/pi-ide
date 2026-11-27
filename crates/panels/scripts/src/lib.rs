//! Script panels: tool windows written in JavaScript and run by gpui-shell
//! (QuickJS). A script describes GPUI elements; Rust draws them, so a script
//! panel is native like the rest of pide. See docs/SCRIPT-PANELS.md.
//!
//! Panels live in folders: `~/.config/pide/panels/<name>/` (yours, every
//! project) and `<project>/.pide/panels/<name>/` (the project's). Each is a
//! tool window with a stripe button and a `view.script.<name>` command. The
//! first time a panel runs, it shows what it asks for and waits for Allow.
//! Saving its files reloads it; a broken save keeps the last working version.

pub mod host;
pub mod image;
pub mod manage;
pub mod pi;
pub mod manifest;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::{ActiveTheme, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_shell::{Capabilities, ExecuteGrant, ShellRoot, ShellRuntime, Watcher};
use pide_core::{CommandRegistry, CommandSpec, DockPosition, Panel, Services};

use manifest::Found;

/// One JavaScript VM for every script panel; each panel has its own policy.
/// A GPUI global, not a thread-local: it must go before gpui-shell's own
/// thread-locals do, or quitting aborts.
struct Runtime(Rc<ShellRuntime>);
impl Global for Runtime {}

fn runtime(cx: &mut App) -> anyhow::Result<Rc<ShellRuntime>> {
    if let Some(rt) = cx.try_global::<Runtime>() {
        return Ok(rt.0.clone());
    }
    let mut registry = gpui_shell::components::registry()?;
    image::register(&mut registry)?;
    let components = registry.freeze()?;
    let rt = ShellRuntime::new_with_components(cx, components)?;
    cx.set_global(Runtime(rt.clone()));
    Ok(rt)
}

/// Every panel found so far, by tool window id.
#[derive(Default)]
struct Panels(HashMap<String, Found>);
impl Global for Panels {}

pub fn init(cx: &mut App) {
    gpui_shell::init(cx);
    if let Err(e) = gpui_shell::export_module(host::module()) {
        log::error!("script panels: the pide module didn't register: {e}");
    }
    for found in builtin_panels()
        .into_iter()
        .chain(manifest::scan(&user_dir(), None))
    {
        register(found, cx);
    }
    manage::init(cx);
    CommandRegistry::register(
        cx,
        CommandSpec::new("panels.rescan", "Script Panels: Look for New Panels"),
        |window, cx| {
            let root = Services::of(window, cx)
                .workspace
                .read(cx)
                .root()
                .to_path_buf();
            rescan(&root, cx);
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("panels.manage", "Settings: Script Panels…"),
        |window, cx| pide_core::settings_page::open(Some("script_panels"), window, cx),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new(
            "panels.open_folder",
            "Script Panels: Open This Project's Panels Folder",
        ),
        |window, cx| {
            let root = Services::of(window, cx)
                .workspace
                .read(cx)
                .root()
                .to_path_buf();
            let dir = project_dir(&root);
            let _ = std::fs::create_dir_all(&dir);
            cx.reveal_path(&dir);
        },
    );
}

/// Panels shipped with pide, by folder name: (file, contents).
const BUILTIN: &[(&str, &[(&str, &str)])] = &[(
    "problems",
    &[
        ("panel.json", include_str!("../builtin/problems/panel.json")),
        ("main.js", include_str!("../builtin/problems/main.js")),
    ],
)];

/// Write the shipped panels to the state dir (only what changed) and list them.
pub fn builtin_panels() -> Vec<Found> {
    let dir = pide_core::paths::state_dir().join("builtin-panels");
    for (name, files) in BUILTIN {
        let panel = dir.join(name);
        let _ = std::fs::create_dir_all(&panel);
        for (file, text) in *files {
            let path = panel.join(file);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(*text) {
                if let Err(e) = std::fs::write(&path, text) {
                    log::warn!("couldn't write {}: {e}", path.display());
                }
            }
        }
    }
    // The guide pi reads before writing a panel (the system prompt points here).
    let guide = dir.join("GUIDE.md");
    let text = include_str!("../../../../docs/SCRIPT-PANELS.md");
    if std::fs::read_to_string(&guide).ok().as_deref() != Some(text) {
        let _ = std::fs::write(&guide, text);
    }
    manifest::scan(&dir, None)
        .into_iter()
        .map(|mut f| {
            f.builtin = true;
            f
        })
        .collect()
}

/// `~/.config/pide/panels`: panels for every project.
pub fn user_dir() -> PathBuf {
    pide_core::paths::config_dir().join("panels")
}

/// `<project>/.pide/panels`.
pub fn project_dir(root: &Path) -> PathBuf {
    root.join(".pide").join("panels")
}

/// Register a project's panels. The app calls this before the project's window
/// restores its layout, so saved script panels come back.
pub fn open_project(root: &Path, cx: &mut App) {
    let dir = project_dir(root);
    // gpui-shell writes editor type files beside each script; keep them out of git.
    let ignore = dir.join(".gitignore");
    if dir.is_dir() && !ignore.exists() {
        let _ = std::fs::write(
            &ignore,
            "gpui-kit.d.ts\njsconfig.json\n*/gpui-kit.d.ts\n*/jsconfig.json\n",
        );
    }
    for found in manifest::scan(&dir, Some(root)) {
        register(found, cx);
    }
}

/// Look for new panels (yours and `root`'s), and forget ones whose folder is gone.
pub fn rescan(root: &Path, cx: &mut App) {
    open_project(root, cx);
    for found in manifest::scan(&user_dir(), None) {
        register(found, cx);
    }
    let gone: Vec<String> = cx
        .try_global::<Panels>()
        .map(|p| {
            p.0.iter()
                .filter(|(_, f)| !f.builtin && !f.dir.join(manifest::ENTRY).is_file())
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default();
    if gone.is_empty() {
        return;
    }
    for id in &gone {
        log::info!("script panel {id} is gone");
        cx.default_global::<Panels>().0.remove(id);
    }
    // Close them first (closing needs the tool window's spec), then forget them.
    cx.defer(move |cx| {
        for w in cx.windows() {
            let _ = w.update(cx, |_, window, cx| {
                for id in &gone {
                    pide_core::toolwindow::close(id, window, cx);
                }
            });
        }
        for id in &gone {
            pide_core::toolwindow::unregister(cx, id);
        }
        cx.refresh_windows();
    });
}

/// Called when a project window closes.
pub fn forget_window(id: WindowId, cx: &mut App) {
    host::forget_window(id, cx);
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn dock_of(found: &Found) -> DockPosition {
    match found.manifest.dock.as_deref() {
        Some("left") => DockPosition::Left,
        Some("bottom") => DockPosition::Bottom,
        _ => DockPosition::Right,
    }
}

fn icon_of(found: &Found) -> IconName {
    match found.manifest.icon.as_deref() {
        Some("triangle-alert") => IconName::TriangleAlert,
        Some("map") => IconName::Map,
        Some("search") => IconName::Search,
        Some("terminal") => IconName::SquareTerminal,
        Some("folder") => IconName::Folder,
        Some("bot") => IconName::Bot,
        Some("github") => IconName::Github,
        Some("refresh") => IconName::RefreshCw,
        _ => IconName::File,
    }
}

fn register(found: Found, cx: &mut App) {
    let id = found.id();
    let unchanged = cx
        .try_global::<Panels>()
        .and_then(|p| p.0.get(&id))
        .is_some_and(|old| old == &found);
    if unchanged {
        return;
    }
    let scope = found.scope();
    let command = leak(format!(
        "view.script.{scope}.{}",
        found.name.replace('-', "_")
    ));
    let title = leak(found.title());
    let key = found.manifest.key.clone().map(leak);
    let id_static: &'static str = leak(id.clone());
    let make = {
        let found = found.clone();
        move |window: &mut Window, cx: &mut App| {
            let found = found.clone();
            panel_handle(cx.new(|cx| ScriptPanel::new(found, id_static, window, cx)))
        }
    };
    let restore = make.clone();
    register_panel(cx, id_static, move |_, window, cx| restore(window, cx));
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: id_static,
            title,
            icon: icon_of(&found),
            dock: dock_of(&found),
            command,
            key,
            create: Rc::new(make),
            remove: pide_core::toolwindow::remover::<ScriptPanel>(),
        },
    );
    cx.default_global::<Panels>().0.insert(id, found);
    cx.refresh_windows();
}

// ---------------------------------------------------------------------------
// Grants: what you allowed, per panel folder. A panel that asks for more asks again.

fn grants_file() -> PathBuf {
    pide_core::paths::state_dir().join("panel-grants.json")
}

fn load_grants() -> HashMap<String, serde_json::Value> {
    std::fs::read_to_string(grants_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn granted(found: &Found) -> bool {
    granted_in(found, &load_grants())
}

fn granted_in(found: &Found, grants: &HashMap<String, serde_json::Value>) -> bool {
    if found.builtin {
        return true;
    }
    let want = serde_json::to_value(&found.manifest.permissions).unwrap_or_default();
    grants.get(&found.dir.display().to_string()) == Some(&want)
}

fn grant(found: &Found) {
    let mut all = load_grants();
    all.insert(
        found.dir.display().to_string(),
        serde_json::to_value(&found.manifest.permissions).unwrap_or_default(),
    );
    save_grants(&all);
}

fn save_grants(all: &HashMap<String, serde_json::Value>) {
    let path = grants_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(
        &path,
        serde_json::to_string_pretty(all).unwrap_or_default(),
    ) {
        log::warn!("couldn't save panel grants to {}: {e}", path.display());
    }
}

/// Forget what you allowed a panel: it asks again the next time it's shown.
fn revoke(found: &Found) {
    let mut all = load_grants();
    if all.remove(&found.dir.display().to_string()).is_some() {
        save_grants(&all);
    }
}

fn capabilities(found: &Found, project: &Path) -> Capabilities {
    let p = &found.manifest.permissions;
    let mut caps = Capabilities::new()
        .read_roots([project.to_path_buf(), found.dir.clone()])
        .storage(true)
        .clipboard_write(true);
    if p.write {
        caps = caps.write_roots([project.to_path_buf()]);
    }
    if p.run.iter().any(|c| c == "*") {
        caps = caps.execute(ExecuteGrant::Unrestricted);
    } else if !p.run.is_empty() {
        caps = caps.execute(ExecuteGrant::Allowed(p.run.clone()));
    }
    if !p.network.is_empty() {
        caps = caps.network_hosts(p.network.clone());
    }
    caps
}

/// A bundle id gpui-shell accepts (a-z, 0-9, `.`, `-`, `_`): names the panel's storage.
fn bundle_id(found: &Found) -> String {
    let base = match &found.project {
        Some(root) => format!("project.{}", gpui_shell::bundle_id_for_path(root)),
        None => "user".to_string(),
    };
    format!("pide.{base}.{}", found.name)
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '.' | '-' | '_' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The panel.

enum State {
    /// Waiting for Allow on the consent card.
    Consent,
    Running {
        root: Entity<ShellRoot>,
        _watch: Option<Watcher>,
    },
    Failed(String),
}

pub struct ScriptPanel {
    name: &'static str,
    found: Found,
    project: PathBuf,
    state: State,
    focus: FocusHandle,
}

impl EventEmitter<PanelEvent> for ScriptPanel {}

impl ScriptPanel {
    pub fn new(
        found: Found,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        host::watch_window(window, cx);
        let project = Services::of(window, cx)
            .workspace
            .read(cx)
            .root()
            .to_path_buf();
        let mut this = Self {
            name,
            found,
            project,
            state: State::Consent,
            focus: cx.focus_handle(),
        };
        if let Some(err) = this.found.error.clone() {
            this.state = State::Failed(err);
        } else if granted(&this.found) {
            this.start(window, cx);
        }
        this
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state = match self.load(window, cx) {
            Ok(state) => state,
            Err(e) => State::Failed(format!("{e:#}")),
        };
        cx.notify();
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<State> {
        let rt = runtime(cx)?;
        // try_load copies the default policy, so each panel keeps its own grant.
        gpui_shell::set_bundle_id(&bundle_id(&self.found))?;
        gpui_shell::set_capabilities(capabilities(&self.found, &self.project));
        image::remember(&self.found.dir, &self.project);
        pi::remember(&self.found.dir, self.found.manifest.permissions.pi);
        let root = rt.try_load(&self.found.dir, window, cx)?;
        let watch = match rt.watch(&root, window, cx) {
            Ok(w) => Some(w),
            Err(e) => {
                log::warn!("script panel {}: no hot reload: {e:#}", self.found.name);
                None
            }
        };
        Ok(State::Running {
            root,
            _watch: watch,
        })
    }

    fn allow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        grant(&self.found);
        self.start(window, cx);
    }

    fn show_code(&self, window: &mut Window, cx: &mut App) {
        let path = self.found.dir.join(manifest::ENTRY);
        window.defer(cx, move |window, cx| {
            pide_core::opener::open_file(path, window, cx)
        });
    }

    fn ask_pi_to_fix(&self, error: &str, window: &mut Window, cx: &mut App) {
        let text = format!(
            "The script panel in {} fails to load:\n\n```\n{error}\n```\n\nFix it.",
            self.found.dir.display()
        );
        window.defer(cx, move |window, cx| {
            pide_core::opener::ask_agent(text, window, cx)
        });
    }

    fn consent(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let source = match &self.found.project {
            Some(_) => format!("This project · {}", rel_dir(&self.found.dir, &self.project)),
            None => format!("Yours · {}", self.found.dir.display()),
        };
        let lines = self.found.manifest.permissions.describe();
        v_flex()
            .p_4()
            .gap_3()
            .max_w(px(460.))
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Run “{}”?", self.found.title())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("A script panel. {source}")),
            )
            .child(
                v_flex()
                    .gap_1()
                    .children(lines.into_iter().map(|(asks, line)| {
                        h_flex()
                            .gap_2()
                            .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(if asks {
                                theme.warning
                            } else {
                                theme.success
                            }))
                            .child(line)
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Everything else is denied. A version that asks for more asks again."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("show-code")
                            .small()
                            .label("Show Code")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.show_code(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("allow")
                            .small()
                            .primary()
                            .label("Allow and Run")
                            .on_click(cx.listener(|this, _, window, cx| this.allow(window, cx))),
                    ),
            )
    }

    fn failure(&self, error: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let err = error.to_string();
        v_flex()
            .p_4()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.warning)
                    .child(format!("“{}” didn't load", self.found.title())),
            )
            .child(
                div()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .whitespace_normal()
                    .child(error.to_string()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("fix").small().label("Ask pi to Fix").on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.ask_pi_to_fix(&err, window, cx)
                        }),
                    ))
                    .child(
                        Button::new("code").small().label("Open main.js").on_click(
                            cx.listener(|this, _, window, cx| this.show_code(window, cx)),
                        ),
                    )
                    .child(
                        Button::new("retry")
                            .small()
                            .ghost()
                            .label("Retry")
                            .on_click(cx.listener(|this, _, window, cx| {
                                // Re-read panel.json too: the fix may have been there.
                                let fresh = manifest::scan(
                                    this.found.dir.parent().unwrap_or(&this.found.dir),
                                    this.found.project.as_deref(),
                                )
                                .into_iter()
                                .find(|f| f.name == this.found.name);
                                if let Some(f) = fresh {
                                    this.found = f;
                                }
                                if let Some(e) = this.found.error.clone() {
                                    this.state = State::Failed(e);
                                } else if granted(&this.found) {
                                    this.start(window, cx);
                                } else {
                                    this.state = State::Consent;
                                }
                                cx.notify();
                            })),
                    ),
            )
    }
}

impl ScriptPanel {
    /// What the panel shows, as text: the script's element tree, or why it
    /// isn't running. For tests and devctl.
    pub fn describe(&self, cx: &App) -> String {
        match &self.state {
            State::Consent => "consent".into(),
            State::Failed(e) => format!("failed: {e}"),
            State::Running { root, .. } => {
                let content = root.read(cx).content().clone();
                match content.downcast::<gpui_shell::ScriptView>() {
                    Ok(view) => {
                        let view = view.read(cx);
                        match (view.build_error(), view.snapshot()) {
                            (Some(e), _) => format!("error: {e}"),
                            (None, Some(s)) => s.debug_tree(),
                            (None, None) => "not rendered yet".into(),
                        }
                    }
                    Err(_) => "running".into(),
                }
            }
        }
    }
}

fn rel_dir(dir: &Path, root: &Path) -> String {
    dir.strip_prefix(root).unwrap_or(dir).display().to_string()
}

impl Focusable for ScriptPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for ScriptPanel {
    fn panel_name(&self) -> &'static str {
        self.name
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(self.panel_name())
    }
}

impl gpui_kit::component::dock::Panel for ScriptPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.found.title()
    }
}

impl Panel for ScriptPanel {
    fn title_text(&self, _: &App) -> SharedString {
        self.found.title().into()
    }
    fn default_dock(&self) -> DockPosition {
        dock_of(&self.found)
    }
}

impl Render for ScriptPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            State::Running { root, .. } => div().size_full().child(root.clone()).into_any_element(),
            State::Consent => self.consent(cx).into_any_element(),
            State::Failed(e) => {
                let e = e.clone();
                self.failure(&e, cx).into_any_element()
            }
        };
        div()
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .child(body)
    }
}
