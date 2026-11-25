//! CMake tool window (bottom dock), after CLion:
//!
//! - **Profiles** are the project's configure presets (`CMakePresets.json`,
//!   `CMakeUserPresets.json`), or CLion's `Debug`/`Release` in
//!   `cmake-build-*` without presets (`presets.rs`).
//! - **Reload CMake Project** configures the selected profile, asking for
//!   `compile_commands.json` and, through the CMake File API, for the code
//!   model: that's where the **targets** come from (`fileapi.rs`).
//! - **Build** (Ctrl+F9) builds the selected target (or all), configuring
//!   first if the profile never was; **Run** (Shift+F10) builds, then starts
//!   the executable. Editing CMake files shows a "reload" notice instead of
//!   reloading by itself.
//! - clangd follows the selected profile's `compile_commands.json`.
//! - On Windows everything runs in Visual Studio's developer environment
//!   (`pide_lsp::msvc`), as CLion's Visual Studio toolchain does.
//! - Errors and warnings from the output are listed beside it (`diag.rs`).

pub mod diag;
pub mod fileapi;
pub mod presets;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{CommandRegistry, CommandSpec, DockPosition, Panel, Services, Workspace, WorkspaceEvent};
use pide_lsp::LspStore;
use serde::{Deserialize, Serialize};

use diag::{Diag, Severity};
use fileapi::{Kind, Target};
use presets::Profile;

pub const PANEL_NAME: &str = "cmake";
/// Keep at most this many output lines (the oldest go first).
const MAX_LINES: usize = 20_000;
const MAX_DIAGS: usize = 2_000;

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| CMakePanel::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "CMake",
            icon: IconName::Building2,
            dock: DockPosition::Bottom,
            command: "view.cmake",
            key: None,
            create: |window, cx| panel_handle(cx.new(|cx| CMakePanel::new(window, cx))),
        },
    );
    let commands: [(CommandSpec, fn(&mut CMakePanel, &mut Context<CMakePanel>)); 7] = [
        (
            CommandSpec::new("cmake.reload", "CMake: Reload CMake Project"),
            |p, cx| p.reload(false, cx),
        ),
        (
            CommandSpec::new("cmake.reset", "CMake: Reset Cache and Reload Project"),
            |p, cx| p.reload(true, cx),
        ),
        (
            CommandSpec::new("cmake.build", "Build: Build").key("cmd-f9"),
            |p, cx| p.build(false, cx),
        ),
        (
            CommandSpec::new("cmake.rebuild", "Build: Rebuild"),
            |p, cx| p.build(true, cx),
        ),
        (CommandSpec::new("cmake.clean", "Build: Clean"), |p, cx| p.clean(cx)),
        (
            CommandSpec::new("cmake.run", "Run: Run CMake Target").key("shift-f10"),
            |p, cx| p.run(cx),
        ),
        (CommandSpec::new("cmake.stop", "Run: Stop CMake Process"), |p, cx| p.stop(cx)),
    ];
    for (spec, f) in commands {
        CommandRegistry::register(cx, spec, move |window, cx| {
            pide_core::toolwindow::show(PANEL_NAME, false, window, cx);
            if let Some(p) = existing(window, cx) {
                p.update(cx, |p, cx| p.when_ready(f, cx));
            }
        });
    }
    // clangd should follow the saved profile from the start, not only once the
    // tool window has been opened.
    cx.observe_new(|_: &mut Workspace, _, cx: &mut Context<Workspace>| {
        let ws = cx.entity();
        cx.defer(move |cx| point_clangd_at_saved_profile(ws, cx));
    })
    .detach();
}

#[derive(Default)]
struct Instances(HashMap<WindowId, WeakEntity<CMakePanel>>);
impl Global for Instances {}

/// This window's CMake panel, if one exists.
pub fn existing(window: &Window, cx: &App) -> Option<Entity<CMakePanel>> {
    cx.try_global::<Instances>()?
        .0
        .get(&window.window_handle().window_id())?
        .upgrade()
}

/// What pide remembers per project: the profile, the target, run arguments.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Saved {
    profile: Option<String>,
    target: Option<String>,
    #[serde(default)]
    args: HashMap<String, String>,
}

fn state_file(root: &Path) -> PathBuf {
    // FNV-1a: stable across builds (the std hasher isn't).
    let mut h: u64 = 0xcbf29ce484222325;
    for b in root.to_string_lossy().bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    pide_core::paths::state_dir()
        .join("cmake")
        .join(format!("{h:016x}.json"))
}

/// What a background load finds (blocking I/O).
struct Loaded {
    has_cmake: bool,
    profiles: Result<Vec<Profile>, String>,
    saved: Saved,
}

fn load_project(root: &Path) -> Loaded {
    let has_cmake = root.join("CMakeLists.txt").is_file();
    let saved: Saved = std::fs::read_to_string(state_file(root))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let files: Vec<(PathBuf, String)> = presets::PRESET_FILES
        .iter()
        .map(|f| root.join(f))
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|t| (p, t)))
        .collect();
    let profiles = if !has_cmake {
        Ok(vec![])
    } else if files.is_empty() {
        let ninja = pide_runtime::process::find_command("ninja").is_some()
            || dev_env().iter().any(|(k, v)| {
                k.eq_ignore_ascii_case("PATH")
                    && std::env::split_paths(v).any(|d| d.join("ninja.exe").is_file())
            });
        Ok(presets::default_profiles(root, ninja))
    } else {
        presets::profiles(root, &files, &|p| std::fs::read_to_string(p).ok())
            .map_err(|e| format!("{e:#}"))
    };
    Loaded {
        has_cmake,
        profiles,
        saved,
    }
}

/// The environment builds and programs run in: Visual Studio's developer
/// environment on Windows, nothing elsewhere. Blocking the first time.
fn dev_env() -> Vec<(String, String)> {
    pide_lsp::msvc::dev_env().to_vec()
}

/// What we know about a profile's build folder (blocking I/O).
struct BuildState {
    configured: bool,
    targets: Vec<Target>,
    compile_db: bool,
}

fn read_build_state(p: &Profile) -> BuildState {
    let targets = fileapi::read_targets(&p.binary_dir, p.build_type.as_deref())
        .map_err(|e| log::warn!("cmake: reading targets: {e:#}"))
        .ok()
        .flatten()
        .unwrap_or_default();
    BuildState {
        configured: p.is_configured(),
        targets,
        compile_db: p.binary_dir.join("compile_commands.json").is_file(),
    }
}

fn clangd_args(p: &Profile) -> Vec<String> {
    vec![format!("--compile-commands-dir={}", p.binary_dir.display())]
}

/// At project open: point clangd at the saved (or first) profile's database.
fn point_clangd_at_saved_profile(workspace: Entity<Workspace>, cx: &mut App) {
    let root = workspace.read(cx).root().to_path_buf();
    let task = cx.background_executor().spawn(async move {
        let loaded = load_project(&root);
        let profiles = loaded.profiles.ok()?;
        let p = profiles
            .iter()
            .find(|p| Some(&p.name) == loaded.saved.profile.as_ref())
            .or(profiles.first())?;
        p.binary_dir
            .join("compile_commands.json")
            .is_file()
            .then(|| clangd_args(p))
    });
    cx.spawn(async move |cx| {
        if let Some(args) = task.await {
            let _ = cx.update(|cx| {
                let store = LspStore::for_workspace(&workspace, cx);
                store.update(cx, |s, cx| s.set_project_args(pide_lsp::registry::CLANGD, args, cx));
            });
        }
    })
    .detach();
}

/// One step of what a button starts. Steps run in order; a failure ends the chain.
#[derive(Clone, Debug)]
enum Step {
    Configure { reset: bool },
    Build { clean_first: bool },
    Clean,
    Run,
}

struct Job {
    stop: Option<pide_runtime::tokio::sync::oneshot::Sender<()>>,
    /// Stop pressed before the process was up.
    cancelled: bool,
}

pub struct CMakePanel {
    root: PathBuf,
    lsp: Entity<LspStore>,
    loading: bool,
    /// The profile and its build folder are known: actions can run.
    ready: bool,
    /// An action that came before that (a key pressed as the panel opened).
    pending: Option<fn(&mut CMakePanel, &mut Context<CMakePanel>)>,
    has_cmake: bool,
    profiles: Vec<Profile>,
    presets_error: Option<String>,
    profile: Option<String>,
    configured: bool,
    targets: Vec<Target>,
    /// `None`: all targets.
    target: Option<String>,
    run_args: HashMap<String, String>,
    /// CMake files changed since the last reload.
    stale: bool,
    job: Option<Job>,
    /// What the current output is of ("Build vrgAr (Debug)").
    title: String,
    lines: Vec<SharedString>,
    diags: Vec<Diag>,
    /// How the last process ended; `true` = failed.
    status: Option<(String, bool)>,
    started: Option<Instant>,
    output_scroll: UniformListScrollHandle,
    diag_scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _load: Option<Task<()>>,
    _job_task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for CMakePanel {}

impl CMakePanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = Services::of(window, cx).workspace.clone();
        let root = workspace.read(cx).root().to_path_buf();
        let lsp = LspStore::for_workspace(&workspace, cx);
        let me = cx.entity().downgrade();
        cx.default_global::<Instances>()
            .0
            .insert(window.window_handle().window_id(), me);
        let subs = vec![cx.subscribe(&workspace, |this, _, ev: &WorkspaceEvent, cx| {
            if let WorkspaceEvent::BufferSaved(b) = ev {
                let path = b.read(cx).path.clone();
                this.saved_file(&path, cx);
            }
        })];
        let mut this = Self {
            root,
            lsp,
            loading: true,
            ready: false,
            pending: None,
            has_cmake: false,
            profiles: vec![],
            presets_error: None,
            profile: None,
            configured: false,
            targets: vec![],
            target: None,
            run_args: HashMap::new(),
            stale: false,
            job: None,
            title: String::new(),
            lines: vec![],
            diags: vec![],
            status: None,
            started: None,
            output_scroll: UniformListScrollHandle::new(),
            diag_scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _load: None,
            _job_task: None,
            _subs: subs,
        };
        this.load(cx);
        this
    }

    /// (Re)read presets and the saved choices, then the build folder.
    fn load(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let task = cx.background_executor().spawn(async move { load_project(&root) });
        self._load = Some(cx.spawn(async move |this, cx| {
            let loaded = task.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.has_cmake = loaded.has_cmake;
                match loaded.profiles {
                    Ok(p) => {
                        this.profiles = p;
                        this.presets_error = None;
                    }
                    Err(e) => this.presets_error = Some(e),
                }
                // First load: what was saved. Later: keep the current choice if it's still there.
                let wanted = this.profile.clone().or(loaded.saved.profile);
                this.profile = wanted
                    .filter(|w| this.profiles.iter().any(|p| &p.name == w))
                    .or_else(|| this.profiles.first().map(|p| p.name.clone()));
                if this.run_args.is_empty() {
                    this.run_args = loaded.saved.args;
                }
                if this.target.is_none() {
                    this.target = loaded.saved.target;
                }
                this.refresh_build_state(cx);
                cx.notify();
            });
        }));
    }

    fn current(&self) -> Option<&Profile> {
        let name = self.profile.as_ref()?;
        self.profiles.iter().find(|p| &p.name == name)
    }

    /// Targets and configured-ness of the selected profile; points clangd there.
    /// Run `f` now, or once the project has loaded.
    fn when_ready(&mut self, f: fn(&mut CMakePanel, &mut Context<CMakePanel>), cx: &mut Context<Self>) {
        if self.ready {
            f(self, cx);
        } else {
            self.pending = Some(f);
        }
    }

    fn set_ready(&mut self, cx: &mut Context<Self>) {
        self.ready = true;
        if let Some(f) = self.pending.take() {
            f(self, cx);
        }
    }

    fn refresh_build_state(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.current().cloned() else {
            self.targets.clear();
            self.configured = false;
            self.set_ready(cx);
            return;
        };
        let task = cx.background_executor().spawn(async move { read_build_state(&p) });
        cx.spawn(async move |this, cx| {
            let state = task.await;
            let _ = this.update(cx, |this, cx| {
                this.configured = state.configured;
                this.targets = state.targets;
                // A target that's gone (renamed, other profile) falls back to all.
                if !this.targets.is_empty()
                    && let Some(t) = &this.target
                    && !this.targets.iter().any(|x| &x.name == t)
                {
                    this.target = None;
                }
                if state.compile_db
                    && let Some(p) = this.current()
                {
                    let args = clangd_args(p);
                    this.lsp.update(cx, |s, cx| {
                        s.set_project_args(pide_lsp::registry::CLANGD, args, cx)
                    });
                }
                this.set_ready(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn save_state(&self, cx: &mut Context<Self>) {
        let saved = Saved {
            profile: self.profile.clone(),
            target: self.target.clone(),
            args: self.run_args.clone(),
        };
        let path = state_file(&self.root);
        cx.background_executor()
            .spawn(async move {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Ok(text) = serde_json::to_string_pretty(&saved) {
                    let _ = std::fs::write(&path, text);
                }
            })
            .detach();
    }

    fn saved_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_cmake = name.eq_ignore_ascii_case("CMakeLists.txt")
            || name.to_ascii_lowercase().ends_with(".cmake")
            || presets::PRESET_FILES.contains(&name);
        if !is_cmake || !path.starts_with(&self.root) {
            return;
        }
        if presets::PRESET_FILES.contains(&name) {
            self.load(cx);
        }
        self.stale = true;
        cx.notify();
    }

    fn select_profile(&mut self, name: String, cx: &mut Context<Self>) {
        if self.profile.as_ref() == Some(&name) {
            return;
        }
        self.profile = Some(name);
        self.targets.clear();
        self.configured = false;
        self.refresh_build_state(cx);
        self.save_state(cx);
        cx.notify();
    }

    fn select_target(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        self.target = target;
        self.save_state(cx);
        cx.notify();
    }

    fn selected_target(&self) -> Option<&Target> {
        let t = self.target.as_ref()?;
        self.targets.iter().find(|x| &x.name == t)
    }

    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    // ---- actions ----

    pub fn reload(&mut self, reset: bool, cx: &mut Context<Self>) {
        self.start(VecDeque::from([Step::Configure { reset }]), cx);
    }

    pub fn build(&mut self, clean_first: bool, cx: &mut Context<Self>) {
        let mut steps = VecDeque::new();
        // CLion configures a profile that never was before building it.
        if !self.configured {
            steps.push_back(Step::Configure { reset: false });
        }
        steps.push_back(Step::Build { clean_first });
        self.start(steps, cx);
    }

    pub fn clean(&mut self, cx: &mut Context<Self>) {
        if self.configured {
            self.start(VecDeque::from([Step::Clean]), cx);
        }
    }

    pub fn run(&mut self, cx: &mut Context<Self>) {
        if self.selected_target().is_none_or(|t| t.kind != Kind::Executable) {
            self.lines.clear();
            self.diags.clear();
            self.title = "Run".into();
            self.status = Some((
                "Choose an executable target to run (the target picker).".into(),
                true,
            ));
            cx.notify();
            return;
        }
        let mut steps = VecDeque::new();
        if !self.configured {
            steps.push_back(Step::Configure { reset: false });
        }
        steps.push_back(Step::Build { clean_first: false });
        steps.push_back(Step::Run);
        self.start(steps, cx);
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = &mut self.job {
            job.cancelled = true;
            if let Some(stop) = job.stop.take() {
                let _ = stop.send(());
            }
            cx.notify();
        }
    }

    /// Start a chain of steps, replacing what runs (a running program holds
    /// its .exe open on Windows; the build couldn't link over it).
    fn start(&mut self, steps: VecDeque<Step>, cx: &mut Context<Self>) {
        if self.current().is_none() {
            return;
        }
        self.stop(cx);
        self.job = None;
        self.lines.clear();
        self.diags.clear();
        self.status = None;
        self.started = Some(Instant::now());
        self.next_step(steps, cx);
    }

    /// The program and arguments for a step, its title, and what to do first.
    fn command_for(&self, step: &Step) -> Result<(String, Vec<String>, PathBuf), String> {
        let p = self.current().ok_or("No CMake profile")?;
        let target = self.target.clone();
        let named = |verb: &str| match &target {
            Some(t) => format!("{verb} {t} ({})", p.display),
            None => format!("{verb} ({})", p.display),
        };
        let cmake = |args: Vec<String>| {
            let mut v = vec!["cmake".to_string()];
            v.extend(args);
            v
        };
        Ok(match step {
            Step::Configure { reset } => {
                let mut args = p.configure.clone();
                args.push("-DCMAKE_EXPORT_COMPILE_COMMANDS=ON".into());
                (
                    format!(
                        "{} ({})",
                        if *reset { "Reset cache and reload CMake" } else { "Reload CMake" },
                        p.display
                    ),
                    cmake(args),
                    self.root.clone(),
                )
            }
            Step::Build { clean_first } => (
                named(if *clean_first { "Rebuild" } else { "Build" }),
                cmake(p.build_args(target.as_deref(), *clean_first)),
                self.root.clone(),
            ),
            Step::Clean => (
                format!("Clean ({})", p.display),
                cmake(p.build_args(Some("clean"), false)),
                self.root.clone(),
            ),
            Step::Run => {
                let t = self.selected_target().ok_or("No target")?;
                let program = t
                    .program
                    .clone()
                    .ok_or_else(|| format!("{} has no program to run", t.name))?;
                let mut argv = vec![program.to_string_lossy().into_owned()];
                argv.extend(split_args(self.run_args.get(&t.name).map(String::as_str).unwrap_or("")));
                (format!("Run {}", t.name), argv, self.root.clone())
            }
        })
    }

    fn next_step(&mut self, mut steps: VecDeque<Step>, cx: &mut Context<Self>) {
        let Some(step) = steps.pop_front() else {
            self.job = None;
            cx.notify();
            return;
        };
        let (title, argv, cwd) = match self.command_for(&step) {
            Ok(c) => c,
            Err(e) => {
                self.job = None;
                self.status = Some((e, true));
                cx.notify();
                return;
            }
        };
        if !self.lines.is_empty() {
            self.lines.push("".into());
        }
        self.lines.push(format!("▶ {title}").into());
        self.lines.push(format!("  {}", show_command(&argv)).into());
        self.title = title.clone();
        self.job = Some(Job {
            stop: None,
            cancelled: false,
        });
        let step_started = Instant::now();
        // Work before the process: the developer environment (slow the first
        // time), the File API query, deleting the cache for a reset.
        let build_dir = self.current().map(|p| p.binary_dir.clone());
        let prep_step = step.clone();
        let prep = cx.background_executor().spawn(async move {
            if let (Step::Configure { reset }, Some(dir)) = (&prep_step, &build_dir) {
                if *reset {
                    let _ = std::fs::remove_file(dir.join("CMakeCache.txt"));
                    let _ = std::fs::remove_dir_all(dir.join("CMakeFiles"));
                }
                if let Err(e) = fileapi::write_query(dir) {
                    log::warn!("cmake: {e:#}");
                }
            }
            dev_env()
        });
        cx.notify();
        self._job_task = Some(cx.spawn(async move |this, cx| {
            let env = prep.await;
            let run = pide_runtime::process::run_streaming_with(argv, Some(cwd), env);
            let pide_runtime::process::Streaming { mut lines, done, stop } = run;
            let cancelled = this
                .update(cx, |this, _| match &mut this.job {
                    Some(job) if !job.cancelled => {
                        job.stop = Some(stop);
                        false
                    }
                    _ => {
                        let _ = stop.send(());
                        true
                    }
                })
                .unwrap_or(true);
            if !cancelled {
                while let Some(first) = lines.recv().await {
                    let mut batch = vec![first];
                    while let Ok(l) = lines.try_recv() {
                        batch.push(l);
                    }
                    if this.update(cx, |this, cx| this.append(batch, cx)).is_err() {
                        return;
                    }
                }
            }
            let code = done.await;
            let _ = this.update(cx, |this, cx| {
                let secs = step_started.elapsed().as_secs_f32();
                let stopped = this.job.as_ref().is_none_or(|j| j.cancelled);
                let ok = matches!(code, Ok(Ok(Some(0))));
                let what = match step {
                    Step::Configure { .. } => "CMake reload",
                    Step::Build { .. } => "Build",
                    Step::Clean => "Clean",
                    Step::Run => "Process",
                };
                let counts = this.counts();
                let text = match &code {
                    _ if stopped => format!("{what} stopped after {secs:.1} s"),
                    Ok(Ok(Some(0))) => format!("{what} finished in {secs:.1} s{counts}"),
                    Ok(Ok(Some(c))) => format!("✗ {what} failed (exit code {c}) after {secs:.1} s{counts}"),
                    Ok(Ok(None)) => format!("{what} ended after {secs:.1} s{counts}"),
                    Ok(Err(e)) => format!("✗ Couldn't start: {e}"),
                    Err(_) => format!("{what} ended"),
                };
                this.lines.push(format!("  {text}").into());
                this.status = Some((text, !ok && !stopped));
                if let Step::Configure { .. } = step
                    && ok
                {
                    this.stale = false;
                    this.configured = true;
                    this.refresh_build_state(cx);
                }
                if ok && !stopped {
                    this.next_step(steps, cx);
                } else {
                    this.job = None;
                }
                this.output_scroll
                    .scroll_to_item(this.lines.len().saturating_sub(1), ScrollStrategy::Bottom);
                cx.notify();
            });
        }));
    }

    fn append(&mut self, batch: Vec<String>, cx: &mut Context<Self>) {
        for l in batch {
            let l = strip_ansi(&l);
            if self.diags.len() < MAX_DIAGS
                && let Some(d) = diag::parse(&l)
                && d.severity != Severity::Note
                // MSVC repeats a header's error for every file that includes it.
                && !self.diags.contains(&d)
            {
                self.diags.push(d);
            }
            self.lines.push(l.into());
        }
        if self.lines.len() > MAX_LINES {
            let extra = self.lines.len() - MAX_LINES;
            self.lines.drain(..extra);
        }
        self.output_scroll
            .scroll_to_item(self.lines.len().saturating_sub(1), ScrollStrategy::Bottom);
        cx.notify();
    }

    /// ": 3 errors, 1 warning" for the status, or nothing.
    fn counts(&self) -> String {
        let e = self.diags.iter().filter(|d| d.severity == Severity::Error).count();
        let w = self.diags.iter().filter(|d| d.severity == Severity::Warning).count();
        let n = |k: usize, one: &str| format!("{k} {one}{}", if k == 1 { "" } else { "s" });
        match (e, w) {
            (0, 0) => String::new(),
            (e, 0) => format!(" · {}", n(e, "error")),
            (0, w) => format!(" · {}", n(w, "warning")),
            (e, w) => format!(" · {}, {}", n(e, "error"), n(w, "warning")),
        }
    }

    /// A file named in the output: absolute, or relative to the build folder
    /// (where Ninja runs the compiler) or to the project.
    fn resolve(&self, path: &str) -> Option<PathBuf> {
        let p = Path::new(path);
        if p.is_absolute() {
            return Some(p.to_path_buf());
        }
        let build = self.current().map(|p| p.binary_dir.clone());
        build
            .into_iter()
            .chain([self.root.clone()])
            .map(|d| d.join(p))
            .find(|c| c.is_file())
    }

    fn diag_file(&self, d: &Diag) -> Option<PathBuf> {
        let path = d.path.as_deref().and_then(|p| self.resolve(p))?;
        Some(pide_core::paths::canonicalize(&path).unwrap_or(path))
    }
}

/// Open the file a diagnostic names, at its line.
fn open_diag(panel: &WeakEntity<CMakePanel>, d: &Diag, window: &mut Window, cx: &mut App) {
    let Some(path) = panel.upgrade().and_then(|p| p.read(cx).diag_file(d)) else {
        return;
    };
    pide_core::opener::open_file_at(path, d.line.saturating_sub(1), d.col.saturating_sub(1), window, cx);
}

impl CMakePanel {

    fn edit_run_args(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.selected_target().filter(|t| t.kind == Kind::Executable) else {
            return;
        };
        let name = t.name.clone();
        let current = self.run_args.get(&name).cloned().unwrap_or_default();
        let this = cx.entity().downgrade();
        let title = name.clone();
        prompt_args(
            &title,
            current,
            Rc::new(move |args, _, cx| {
                let name = name.clone();
                let _ = this.update(cx, |p, cx| {
                    p.run_args.insert(name, args);
                    p.save_state(cx);
                    cx.notify();
                });
            }),
            window,
            cx,
        );
    }
}

/// Closing the panel (or the window) stops what it runs.
impl Drop for CMakePanel {
    fn drop(&mut self) {
        if let Some(stop) = self.job.as_mut().and_then(|j| j.stop.take()) {
            let _ = stop.send(());
        }
    }
}

/// `a "b c" d` → `[a, b c, d]`: double quotes group, nothing else is special.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// A command line as one would type it.
fn show_command(argv: &[String]) -> String {
    argv.iter()
        .map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Remove terminal escape sequences (colors) and keep what follows the last
/// `\r` (progress redraws). The Run panel has the same; panels don't share code.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            '\r' => out.clear(),
            c => out.push(c),
        }
    }
    out
}

impl Focusable for CMakePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for CMakePanel {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for CMakePanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for CMakePanel {
    fn title_text(&self, _: &App) -> SharedString {
        "CMake".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Bottom
    }
}

fn kind_label(k: Kind) -> &'static str {
    match k {
        Kind::Executable => "executable",
        Kind::SharedLibrary => "shared library",
        Kind::StaticLibrary => "static library",
        Kind::ModuleLibrary => "module",
        Kind::ObjectLibrary => "object library",
        Kind::InterfaceLibrary => "interface library",
        Kind::Utility => "utility",
    }
}

impl CMakePanel {
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let running = self.job.is_some();
        let this = cx.entity().downgrade();
        let profile_label = self
            .current()
            .map(|p| p.display.clone())
            .unwrap_or_else(|| "No profile".into());
        let profiles: Vec<(String, String)> = self
            .profiles
            .iter()
            .map(|p| (p.name.clone(), p.display.clone()))
            .collect();
        let selected_profile = self.profile.clone();
        let profile_menu = {
            let this = this.clone();
            Button::new("cmake-profile")
                .xsmall()
                .ghost()
                .icon(IconName::Settings2)
                .label(format!("{profile_label} ▾"))
                .tooltip("CMake profile: which configure preset (build folder) to use")
                .disabled(profiles.is_empty())
                .dropdown_menu(move |mut menu, _, _| {
                    menu = menu.label("CMake profiles");
                    for (name, display) in &profiles {
                        let this = this.clone();
                        let n = name.clone();
                        menu = menu.item(
                            PopupMenuItem::new(display.clone())
                                .checked(Some(name) == selected_profile.as_ref())
                                .on_click(move |_, _, cx| {
                                    let _ = this.update(cx, |p, cx| p.select_profile(n.clone(), cx));
                                }),
                        );
                    }
                    menu
                })
        };

        let target_label = match &self.target {
            Some(t) => t.clone(),
            None => "All targets".into(),
        };
        let targets: Vec<(String, Kind)> = self.targets.iter().map(|t| (t.name.clone(), t.kind)).collect();
        let selected_target = self.target.clone();
        let target_menu = {
            let this = this.clone();
            Button::new("cmake-target")
                .xsmall()
                .ghost()
                .label(format!("{target_label} ▾"))
                .tooltip(if self.targets.is_empty() {
                    "Targets appear after Reload CMake Project"
                } else {
                    "What Build builds and Run runs"
                })
                .dropdown_menu(move |mut menu, _, _| {
                    let pick = |menu: PopupMenu, label: String, value: Option<String>, on: bool| {
                        let this = this.clone();
                        menu.item(PopupMenuItem::new(label).checked(on).on_click(move |_, _, cx| {
                            let v = value.clone();
                            let _ = this.update(cx, |p, cx| p.select_target(v, cx));
                        }))
                    };
                    menu = pick(menu, "All targets".into(), None, selected_target.is_none());
                    let groups: [(&str, fn(Kind) -> bool); 3] = [
                        ("Executables", |k| k == Kind::Executable),
                        ("Libraries", Kind::is_library),
                        ("Other", |k| k == Kind::Utility),
                    ];
                    for (title, keep) in groups {
                        let names: Vec<&(String, Kind)> = targets.iter().filter(|(_, k)| keep(*k)).collect();
                        if names.is_empty() {
                            continue;
                        }
                        menu = menu.separator().label(title);
                        for (name, _) in names {
                            let on = Some(name) == selected_target.as_ref();
                            menu = pick(menu, name.clone(), Some(name.clone()), on);
                        }
                    }
                    menu.scrollable(true).max_h(px(420.))
                })
        };

        let can_run = self.selected_target().is_some_and(|t| t.kind == Kind::Executable);
        let has_profile = self.current().is_some();
        let has_args = self
            .target
            .as_ref()
            .is_some_and(|t| self.run_args.get(t).is_some_and(|a| !a.is_empty()));
        let more_menu = {
            let this = this.clone();
            let configured = self.configured;
            let cache = self.current().map(|p| p.binary_dir.join("CMakeCache.txt"));
            let presets_file = self.root.join(presets::PRESET_FILES[0]);
            let user_presets = self.root.join(presets::PRESET_FILES[1]);
            let lists = self.root.join("CMakeLists.txt");
            Button::new("cmake-more")
                .xsmall()
                .ghost()
                .icon(IconName::Ellipsis)
                .tooltip("More CMake actions")
                .dropdown_menu(move |menu, _, _| {
                    let act = |label: &str, enabled: bool, f: fn(&mut CMakePanel, &mut Context<CMakePanel>)| {
                        let this = this.clone();
                        PopupMenuItem::new(label.to_string()).disabled(!enabled).on_click(move |_, _, cx| {
                            let _ = this.update(cx, |p, cx| f(p, cx));
                        })
                    };
                    let open = |label: &str, path: PathBuf| {
                        PopupMenuItem::new(label.to_string()).on_click(move |_, window, cx| {
                            pide_core::opener::open_file(path.clone(), window, cx);
                        })
                    };
                    let mut menu = menu
                        .item(act("Rebuild", has_profile && !running, |p, cx| p.build(true, cx)))
                        .item(act("Clean", configured && !running, |p, cx| p.clean(cx)))
                        .separator()
                        .item(act("Reset Cache and Reload Project", has_profile && !running, |p, cx| {
                            p.reload(true, cx)
                        }));
                    {
                        let this = this.clone();
                        menu = menu.item(
                            PopupMenuItem::new("Run Arguments…")
                                .disabled(!can_run)
                                .on_click(move |_, window, cx| {
                                    let _ = this.update(cx, |p, cx| p.edit_run_args(window, cx));
                                }),
                        );
                    }
                    menu = menu.separator().item(open("Open CMakeLists.txt", lists.clone()));
                    if presets_file.is_file() {
                        menu = menu.item(open("Open CMakePresets.json", presets_file.clone()));
                    }
                    if user_presets.is_file() {
                        menu = menu.item(open("Open CMakeUserPresets.json", user_presets.clone()));
                    }
                    if let Some(cache) = cache.clone().filter(|c| c.is_file()) {
                        menu = menu.item(open("Open CMakeCache.txt", cache));
                    }
                    menu
                })
        };

        h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .child(profile_menu)
            .child(target_menu)
            .child(div().w(px(1.)).h(px(16.)).mx_1().bg(theme.border))
            .child(
                Button::new("cmake-reload")
                    .xsmall()
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .tooltip("Reload CMake Project")
                    .disabled(!has_profile || running)
                    .on_click(cx.listener(|this, _, _, cx| this.reload(false, cx))),
            )
            .child(
                Button::new("cmake-build")
                    .xsmall()
                    .ghost()
                    .label("Build")
                    .tooltip(format!("Build ({})", pide_core::keymap::label("secondary-f9")))
                    .disabled(!has_profile || running)
                    .on_click(cx.listener(|this, _, _, cx| this.build(false, cx))),
            )
            .child(
                Button::new("cmake-run")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Play)
                    .label(if has_args { "Run…" } else { "Run" })
                    .tooltip(if can_run {
                        format!("Build, then run ({})", pide_core::keymap::label("shift-f10"))
                    } else {
                        "Choose an executable target to run".into()
                    })
                    .disabled(!can_run || running)
                    .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
            )
            .when(running, |this| {
                this.child(
                    Button::new("cmake-stop")
                        .xsmall()
                        .danger()
                        .icon(IconName::Square)
                        .label("Stop")
                        .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                )
            })
            .child(more_menu)
            .child(div().flex_1())
            .when(running, |this| {
                this.child(
                    div()
                        .text_color(pide_core::colors::accent(cx))
                        .whitespace_nowrap()
                        .child(format!("{}…", self.title)),
                )
            })
            .when_some(self.status.clone().filter(|_| !running), |this, (s, failed)| {
                this.child(
                    div()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(if failed { theme.danger } else { theme.muted_foreground })
                        .child(s),
                )
            })
    }

    fn render_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (text, error, action): (String, bool, Option<&str>) = if let Some(e) = &self.presets_error {
            (format!("CMake presets: {e}"), true, None)
        } else if self.job.is_some() || self.loading || !self.has_cmake {
            return None;
        } else if let Some(p) = self.current().filter(|_| !self.configured) {
            (
                format!("{} isn't configured yet ({}).", p.display, p.binary_dir.display()),
                false,
                Some("Reload CMake Project"),
            )
        } else if self.stale {
            ("CMake files changed since the last reload.".into(), false, Some("Reload CMake Project"))
        } else {
            return None;
        };
        Some(
            h_flex()
                .gap_2()
                .px_3()
                .py_1()
                .bg(pide_core::colors::notice_bg(error, cx))
                .child(div().flex_1().min_w_0().child(text))
                .when_some(action, |this, label| {
                    this.child(
                        Button::new("cmake-notice-reload")
                            .xsmall()
                            .label(label)
                            .on_click(cx.listener(|this, _, _, cx| this.reload(false, cx))),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_diags(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let diags = Rc::new(self.diags.clone());
        let this = cx.entity().downgrade();
        v_flex()
            .w(relative(0.42))
            .min_w(px(240.))
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("cmake-diags", diags.len(), move |range, _, cx| {
                            let theme = cx.theme().clone();
                            range
                                .map(|i| {
                                    let d = diags[i].clone();
                                    let (icon, color) = match d.severity {
                                        Severity::Error => (IconName::CircleX, theme.danger),
                                        _ => (IconName::TriangleAlert, theme.warning),
                                    };
                                    let place = match &d.path {
                                        Some(p) => {
                                            let name = Path::new(p)
                                                .file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_else(|| p.clone());
                                            format!("{name}:{}", d.line)
                                        }
                                        None => "linker".into(),
                                    };
                                    let this = this.clone();
                                    h_flex()
                                        .id(("cmake-diag", i))
                                        .gap_2()
                                        .px_2()
                                        .h(px(22.))
                                        .text_xs()
                                        .whitespace_nowrap()
                                        .hover(|s| s.bg(theme.list_hover))
                                        .when(d.path.is_some(), |r| r.cursor_pointer())
                                        .on_click(move |_, window, cx| open_diag(&this, &d, window, cx))
                                        .child(Icon::new(icon).xsmall().text_color(color))
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_color(theme.muted_foreground)
                                                .child(place),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(diags[i].message.clone()),
                                        )
                                })
                                .collect()
                        })
                        .size_full()
                        .track_scroll(&self.diag_scroll),
                    )
                    .vertical_scrollbar(&self.diag_scroll),
            )
    }

    fn render_output(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let lines = Rc::new(self.lines.clone());
        let mono = theme.mono_font_family.clone();
        let link = pide_core::colors::accent(cx);
        let this = cx.entity().downgrade();
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                uniform_list("cmake-output", lines.len(), move |range, _, cx| {
                    let theme = cx.theme().clone();
                    range
                        .map(|i| {
                            let text = lines[i].clone();
                            let row = h_flex()
                                .px_3()
                                .whitespace_nowrap()
                                .font_family(mono.clone())
                                .text_xs();
                            if text.starts_with('▶') {
                                return row
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(text)
                                    .into_any_element();
                            }
                            let Some(d) = diag::parse(&text) else {
                                return row.child(text).into_any_element();
                            };
                            let color = match d.severity {
                                Severity::Error => theme.danger,
                                Severity::Warning => theme.warning,
                                Severity::Note => theme.muted_foreground,
                            };
                            let Some(r) = d.range.clone() else {
                                return row.text_color(color).child(text).into_any_element();
                            };
                            let this = this.clone();
                            row.child(text[..r.start].to_string())
                                .child(
                                    div()
                                        .id(("cmake-loc", i))
                                        .text_color(link)
                                        .underline()
                                        .cursor_pointer()
                                        .on_click(move |_, window, cx| open_diag(&this, &d, window, cx))
                                        .child(text[r.clone()].to_string()),
                                )
                                .child(div().text_color(color).child(text[r.end..].to_string()))
                                .into_any_element()
                        })
                        .collect()
                })
                .size_full()
                .track_scroll(&self.output_scroll),
            )
            .vertical_scrollbar(&self.output_scroll)
    }

    fn render_hint(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let text = if self.loading {
            "Loading the CMake project…".to_string()
        } else if !self.has_cmake {
            "No CMakeLists.txt at the top of this project.".to_string()
        } else {
            let target = self
                .selected_target()
                .map(|t| format!("{} ({})", t.name, kind_label(t.kind)))
                .unwrap_or_else(|| "all targets".into());
            format!(
                "{} builds {target}; {} builds and runs an executable target. Reload CMake Project after changing CMakeLists.txt.",
                pide_core::keymap::label("secondary-f9"),
                pide_core::keymap::label("shift-f10"),
            )
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .px_4()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(text)
    }
}

impl Render for CMakePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.lines.is_empty();
        let has_diags = !self.diags.is_empty();
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .when(self.has_cmake || self.loading, |this| this.child(self.render_toolbar(cx)))
            .children(self.render_notice(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .when(empty, |this| this.child(self.render_hint(cx)))
                    .when(!empty && has_diags, |this| this.child(self.render_diags(cx)))
                    .when(!empty, |this| this.child(self.render_output(cx))),
            )
    }
}

/// Ask for a target's program arguments.
fn prompt_args(
    target: &str,
    current: String,
    on_ok: Rc<dyn Fn(String, &mut Window, &mut App)>,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
    use gpui_kit::component::input::{Input, InputEvent, InputState};
    use std::cell::{Cell, RefCell};

    if window.has_active_dialog(cx) {
        return;
    }
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("-i input.png -o out")
            .default_value(current)
    });
    let focus_once = Rc::new(Cell::new(true));
    let save = Rc::new({
        let input = input.clone();
        move |window: &mut Window, cx: &mut App| {
            let v = input.read(cx).value().trim().to_string();
            on_ok(v, window, cx);
        }
    });
    let enter = save.clone();
    let sub = window.subscribe(&input, cx, move |_, ev: &InputEvent, window, cx| {
        if let InputEvent::PressEnter { .. } = ev {
            window.close_dialog(cx);
            enter(window, cx);
        }
    });
    let sub = Rc::new(RefCell::new(Some(sub)));
    let title = format!("Arguments for {target}");
    window.open_dialog(cx, move |dialog, window, cx| {
        let (input, save, sub) = (input.clone(), save.clone(), sub.clone());
        if focus_once.replace(false) {
            let input = input.clone();
            window.defer(cx, move |window, cx| {
                input.update(cx, |i, cx| {
                    i.focus(window, cx);
                    i.select_all(window, cx);
                })
            });
        }
        dialog
            .text_sm()
            .title(title.clone())
            .w(px(520.))
            .child(Input::new(&input))
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("cmake-args-cancel").label("Cancel").outline()))
                    .child(DialogAction::new().child(Button::new("cmake-args-ok").label("Save").primary())),
            )
            .on_ok(move |_, window, cx| {
                save(window, cx);
                true
            })
            .on_close(move |_, _, _| {
                sub.borrow_mut().take();
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn splits_arguments_like_a_shell() {
        assert_eq!(split_args(r#"-i "a b.png"  -w 3072"#), ["-i", "a b.png", "-w", "3072"]);
        assert_eq!(split_args(r#"-o """#), ["-o", ""]);
        assert!(split_args("  ").is_empty());
    }

    /// The panel's steps, without the panel, on a real project:
    /// `PIDE_CMAKE_DEMO=<project> cargo test -p pide-cmake -- --ignored --nocapture`.
    #[::core::prelude::v1::test]
    #[ignore]
    fn configure_build_and_run_a_real_project() {
        let Some(root) = std::env::var_os("PIDE_CMAKE_DEMO").map(PathBuf::from) else {
            return;
        };
        pide_runtime::init();
        let run = |argv: Vec<String>| -> (Option<i32>, Vec<String>) {
            println!("$ {}", show_command(&argv));
            let s = pide_runtime::process::run_streaming_with(argv, Some(root.clone()), dev_env());
            let code = pide_runtime::handle().block_on(async { s.done.await.unwrap() });
            let mut lines = s.lines;
            let mut out = vec![];
            while let Ok(l) = lines.try_recv() {
                println!("  {l}");
                out.push(strip_ansi(&l));
            }
            (code.unwrap(), out)
        };
        let loaded = load_project(&root);
        assert!(loaded.has_cmake);
        let p = loaded.profiles.unwrap().remove(0);
        println!("profile {} in {}", p.name, p.binary_dir.display());
        let _ = std::fs::remove_dir_all(&p.binary_dir);
        fileapi::write_query(&p.binary_dir).unwrap();
        let mut argv = vec!["cmake".to_string()];
        argv.extend(p.configure.clone());
        argv.push("-DCMAKE_EXPORT_COMPILE_COMMANDS=ON".into());
        assert_eq!(run(argv).0, Some(0), "configure");
        let state = read_build_state(&p);
        assert!(state.configured && state.compile_db);
        let names: Vec<_> = state.targets.iter().map(|t| (t.name.as_str(), t.kind)).collect();
        println!("targets {names:?}");
        let runner = state.targets.iter().find(|t| t.name == "runner").unwrap();
        assert_eq!(runner.source_dir, "tools/runner");
        let program = runner.program.clone().unwrap();

        let mut argv = vec!["cmake".to_string()];
        argv.extend(p.build_args(Some("runner"), false));
        let (code, out) = run(argv);
        let diags: Vec<Diag> = out.iter().filter_map(|l| diag::parse(l)).collect();
        println!("diagnostics {diags:#?}");
        if code != Some(0) {
            assert!(diags.iter().any(|d| d.severity == Severity::Error), "a failed build names its error");
            return;
        }
        let (code, out) = run(vec![program.to_string_lossy().into_owned(), "pide".into()]);
        assert_eq!(code, Some(0));
        assert_eq!(out, ["hello pide (1 args)"]);
    }

    #[::core::prelude::v1::test]
    fn strips_colors() {
        assert_eq!(strip_ansi("\u{1b}[1;31merror\u{1b}[0m: x"), "error: x");
        assert_eq!(strip_ansi("[1/9] a\r[2/9] b"), "[2/9] b");
    }
}
