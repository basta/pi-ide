//! `LspStore`: one per project (workspace). Starts servers when a file of their
//! language opens, keeps their copy of open files in sync, collects diagnostics
//! and progress, and restarts a server that crashes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use gpui_kit::component::input::Rope;
use gpui_kit::*;
use lsp_types::ServerCapabilities;
use pide_core::{Buffer, Workspace, WorkspaceEvent};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::convert::Encoding;
use crate::process::{self, file_uri, uri_path};
use crate::registry::{self, Installed, ServerSpec};
use crate::rpc::{Incoming, Rpc};

/// Automatic restarts allowed within `CRASH_WINDOW` before it waits for Restart.
const MAX_AUTO_RESTARTS: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, PartialEq)]
pub enum ServerState {
    /// Checking it's installed, starting, or waiting for `initialize`.
    Starting,
    Running,
    NotInstalled {
        reason: String,
    },
    Crashed {
        message: String,
        stderr_tail: Vec<String>,
    },
    Stopped,
}

pub struct Server {
    pub spec: ServerSpec,
    pub state: ServerState,
    /// The installed binary and its `--version`, once detected.
    pub installed: Option<Installed>,
    pub encoding: Encoding,
    pub capabilities: Option<ServerCapabilities>,
    /// Work-done progress by token: the latest title/message ("Indexing 40%").
    pub progress: BTreeMap<String, String>,
    rpc: Option<Rpc>,
    kill: Option<oneshot::Sender<()>>,
    crashes: Vec<Instant>,
    /// Bumped on every (re)start; stale tasks from an older process check it.
    generation: u64,
    _tasks: Vec<Task<()>>,
}

impl Server {
    pub fn is_running(&self) -> bool {
        self.state == ServerState::Running
    }

    /// One line for the status bar / the Language Servers tab.
    pub fn status_text(&self) -> String {
        match &self.state {
            ServerState::Starting => "starting".into(),
            ServerState::Running => match self.progress.values().next() {
                Some(p) => p.clone(),
                None => "ready".into(),
            },
            ServerState::NotInstalled { .. } => "not installed".into(),
            ServerState::Crashed { .. } => "crashed".into(),
            ServerState::Stopped => "stopped".into(),
        }
    }
}

/// Diagnostics for one file, in the server's own position encoding.
#[derive(Clone, Debug)]
pub struct FileDiagnostics {
    pub encoding: Encoding,
    pub items: Vec<lsp_types::Diagnostic>,
}

struct Doc {
    /// The servers that handle it: the main one first (if any), then add-ons
    /// (spell checking).
    servers: Vec<&'static str>,
    /// The ones it's open in (didOpen sent).
    opened: Vec<&'static str>,
    version: i32,
    text: Rope,
    /// Tracked for pi without an editor (`check_file`); an editor buffer takes it over.
    headless: bool,
}

/// A server asked us to apply an edit (`workspace/applyEdit`). Whoever applies
/// it answers with `LspStore::respond`.
#[derive(Debug)]
pub struct ApplyEditRequest {
    pub server: &'static str,
    pub id: Value,
    pub edit: lsp_types::WorkspaceEdit,
    pub encoding: Encoding,
}

#[derive(Clone, Debug)]
pub enum LspEvent {
    ApplyEdit(std::rc::Rc<ApplyEditRequest>),
    Diagnostics {
        path: PathBuf,
    },
    /// A server's state or progress changed.
    Status,
    /// A file opened whose server isn't installed (once per server and project).
    MissingServer {
        server: &'static str,
    },
}

pub struct LspStore {
    root: PathBuf,
    specs: Vec<ServerSpec>,
    servers: HashMap<&'static str, Server>,
    docs: HashMap<PathBuf, Doc>,
    diagnostics: BTreeMap<PathBuf, FileDiagnostics>,
    /// From add-on servers (spelling), per file and server. Kept apart: the
    /// editor shows them, Problems and the error counts don't.
    addon_diagnostics: BTreeMap<PathBuf, BTreeMap<&'static str, FileDiagnostics>>,
    /// `spellcheck.enabled`: run the add-on servers.
    spellcheck: bool,
    missing_reported: Vec<&'static str>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<LspEvent> for LspStore {}

#[derive(Default)]
struct Stores(HashMap<EntityId, Entity<LspStore>>);
impl Global for Stores {}

impl LspStore {
    /// The store for a project, created on first use. It goes away with the workspace.
    pub fn for_workspace(workspace: &Entity<Workspace>, cx: &mut App) -> Entity<LspStore> {
        if let Some(s) = cx
            .try_global::<Stores>()
            .and_then(|s| s.0.get(&workspace.entity_id()))
        {
            return s.clone();
        }
        let store = cx.new(|cx| LspStore::new(workspace, cx));
        let id = workspace.entity_id();
        cx.default_global::<Stores>().0.insert(id, store.clone());
        cx.observe_release(workspace, move |_, cx| {
            // Dropping the store kills its servers.
            cx.defer(move |cx| {
                cx.default_global::<Stores>().0.remove(&id);
            });
        })
        .detach();
        store
    }

    fn new(workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let settings = cx.try_global::<pide_core::settings::Settings>();
        let lsp_settings = settings.and_then(|s| s.lsp.clone());
        let spellcheck = settings.is_none_or(|s| s.spellcheck);
        let mut this = Self {
            root: workspace.read(cx).root().to_path_buf(),
            specs: registry::specs(lsp_settings.as_ref()),
            servers: HashMap::new(),
            docs: HashMap::new(),
            diagnostics: BTreeMap::new(),
            addon_diagnostics: BTreeMap::new(),
            spellcheck,
            missing_reported: Vec::new(),
            _subs: vec![
                cx.subscribe(workspace, Self::on_workspace_event),
                cx.observe_global::<pide_core::settings::Settings>(|this, cx| {
                    let on = cx.global::<pide_core::settings::Settings>().spellcheck;
                    this.set_spellcheck(on, cx);
                }),
            ],
        };
        // Buffers opened before the store existed.
        for buffer in workspace.read(cx).buffers().to_vec() {
            this.buffer_opened(&buffer, cx);
        }
        this
    }

    pub fn specs(&self) -> &[ServerSpec] {
        &self.specs
    }

    pub fn server(&self, id: &str) -> Option<&Server> {
        self.servers.get(id)
    }

    pub fn servers(&self) -> impl Iterator<Item = &Server> {
        self.servers.values()
    }

    /// The server for a file in whatever state it's in (status bar).
    pub fn server_of(&self, path: &Path) -> Option<&Server> {
        let spec = registry::spec_for(&self.specs, path)?;
        self.servers.get(spec.id)
    }

    /// The running server for a file, if any.
    pub fn server_for(&self, path: &Path) -> Option<&Server> {
        let spec = registry::spec_for(&self.specs, path)?;
        self.servers.get(spec.id).filter(|s| s.is_running())
    }

    pub fn diagnostics(&self) -> &BTreeMap<PathBuf, FileDiagnostics> {
        &self.diagnostics
    }

    pub fn diagnostics_for(&self, path: &Path) -> Option<&FileDiagnostics> {
        self.diagnostics.get(path)
    }

    /// Diagnostics from the add-on servers (spelling), by server.
    pub fn addon_diagnostics_for(
        &self,
        path: &Path,
    ) -> impl Iterator<Item = (&'static str, &FileDiagnostics)> {
        self.addon_diagnostics
            .get(path)
            .into_iter()
            .flat_map(|m| m.iter().map(|(id, d)| (*id, d)))
    }

    /// The add-on servers that handle a file (none while spell checking is off).
    pub fn addons_for(&self, path: &Path) -> Vec<&'static str> {
        if !self.spellcheck {
            return vec![];
        }
        registry::addons_for(&self.specs, path)
            .into_iter()
            .map(|s| s.id)
            .collect()
    }

    /// A server by id, if it's running.
    pub fn running(&self, id: &str) -> Option<&Server> {
        self.servers.get(id).filter(|s| s.is_running())
    }

    /// Every server for a file: the main one first, then the add-ons.
    fn servers_for(&self, path: &Path) -> Vec<&'static str> {
        let mut ids: Vec<&'static str> = registry::spec_for(&self.specs, path)
            .map(|s| s.id)
            .into_iter()
            .collect();
        ids.extend(self.addons_for(path));
        ids
    }

    /// Turn the add-on servers (spell checking) on or off for open files.
    fn set_spellcheck(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.spellcheck == on {
            return;
        }
        self.spellcheck = on;
        let addons: Vec<&'static str> = self
            .specs
            .iter()
            .filter(|s| s.addon)
            .map(|s| s.id)
            .collect();
        if on {
            let paths: Vec<PathBuf> = self
                .docs
                .iter()
                .filter(|(_, d)| !d.headless)
                .map(|(p, _)| p.clone())
                .collect();
            let mut wanted = Vec::new();
            for p in paths {
                let ids = self.addons_for(&p);
                if let Some(d) = self.docs.get_mut(&p) {
                    for id in ids {
                        if !d.servers.contains(&id) {
                            d.servers.push(id);
                        }
                        if !wanted.contains(&id) {
                            wanted.push(id);
                        }
                    }
                }
            }
            for id in wanted {
                self.ensure_server(id, cx);
                self.open_docs(id);
            }
        } else {
            for id in &addons {
                if self.servers.contains_key(id) {
                    self.stop(id, cx);
                }
            }
            for d in self.docs.values_mut() {
                d.servers.retain(|s| !addons.contains(s));
            }
            for path in std::mem::take(&mut self.addon_diagnostics).into_keys() {
                cx.emit(LspEvent::Diagnostics { path });
            }
            cx.notify();
        }
    }

    fn on_workspace_event(
        &mut self,
        _: Entity<Workspace>,
        ev: &WorkspaceEvent,
        cx: &mut Context<Self>,
    ) {
        match ev {
            WorkspaceEvent::BufferOpened(b) => self.buffer_opened(b, cx),
            WorkspaceEvent::BufferChanged(b)
            | WorkspaceEvent::FileChangedOnDisk { buffer: b, .. } => {
                let (path, text) = {
                    let b = b.read(cx);
                    (b.path.clone(), b.text().clone())
                };
                self.sync_text(&path, &text);
            }
            WorkspaceEvent::BufferSaved(b) => {
                let path = b.read(cx).path.clone();
                for rpc in self.rpcs_of(&path) {
                    rpc.notify(
                        "textDocument/didSave",
                        json!({ "textDocument": { "uri": file_uri(&path) } }),
                    );
                }
            }
            WorkspaceEvent::BufferClosed(path) => {
                for rpc in self.rpcs_of(path) {
                    rpc.notify(
                        "textDocument/didClose",
                        json!({ "textDocument": { "uri": file_uri(path) } }),
                    );
                }
                self.docs.remove(path);
                if self.addon_diagnostics.remove(path).is_some() {
                    cx.emit(LspEvent::Diagnostics { path: path.clone() });
                }
            }
            _ => {}
        }
    }

    /// The connections of the servers a file is open in.
    fn rpcs_of(&self, path: &Path) -> Vec<Rpc> {
        self.docs
            .get(path)
            .map(|d| {
                d.opened
                    .iter()
                    .filter_map(|id| self.servers.get(id)?.rpc.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn buffer_opened(&mut self, buffer: &Entity<Buffer>, cx: &mut Context<Self>) {
        let (path, text) = {
            let b = buffer.read(cx);
            (b.path.clone(), b.text().clone())
        };
        let servers = self.servers_for(&path);
        if servers.is_empty() {
            return;
        }
        if let Some(d) = self.docs.get_mut(&path) {
            // Tracked for pi without an editor: the editor takes it over.
            d.headless = false;
            for id in &servers {
                if !d.servers.contains(id) {
                    d.servers.push(id);
                }
            }
            self.sync_text(&path, &text);
        } else {
            self.docs.insert(
                path.clone(),
                Doc {
                    servers: servers.clone(),
                    opened: vec![],
                    version: 1,
                    text,
                    headless: false,
                },
            );
        }
        for id in servers {
            self.ensure_server(id, cx);
            self.open_docs(id);
        }
    }

    /// Have the language server check a file no editor has open (pi's
    /// `ide_diagnostics` asks about files it changed): track it with `text`, as
    /// read from disk, starting the server if needed; or update a file tracked
    /// this way. False if no server handles it. Diagnostics arrive later.
    pub fn check_file(&mut self, path: &Path, text: String, cx: &mut Context<Self>) -> bool {
        let rope = Rope::from(text.as_str());
        if let Some(doc) = self.docs.get(path) {
            // An editor's buffer is the source of truth for its file: never overwrite it from disk.
            if doc.headless {
                self.sync_text(path, &rope);
            }
            return true;
        }
        let Some(spec) = registry::spec_for(&self.specs, path).cloned() else {
            return false;
        };
        // Only the main server: pi asks about errors, not spelling.
        self.docs.insert(
            path.to_path_buf(),
            Doc {
                servers: vec![spec.id],
                opened: vec![],
                version: 1,
                text: rope,
                headless: true,
            },
        );
        self.ensure_server(spec.id, cx);
        self.open_docs(spec.id);
        true
    }

    /// Bring the server's copy of `path` up to date with `text` (no-op if equal).
    pub fn sync_text(&mut self, path: &Path, text: &Rope) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        if &doc.text == text {
            return;
        }
        doc.text = text.clone();
        doc.version += 1;
        // Servers it isn't open in yet get it with didOpen.
        let doc = &self.docs[path];
        for rpc in doc
            .opened
            .iter()
            .filter_map(|id| self.servers.get(id)?.rpc.clone())
        {
            rpc.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": file_uri(path), "version": doc.version },
                    "contentChanges": [{ "text": doc.text.to_string() }],
                }),
            );
        }
    }

    /// didOpen every tracked file of a running server that isn't open there yet.
    fn open_docs(&mut self, server: &'static str) {
        let Some(s) = self.servers.get(server).filter(|s| s.is_running()) else {
            return;
        };
        let Some(rpc) = s.rpc.clone() else { return };
        let spec = s.spec.clone();
        for (path, doc) in self
            .docs
            .iter_mut()
            .filter(|(_, d)| d.servers.contains(&server) && !d.opened.contains(&server))
        {
            rpc.notify(
                "textDocument/didOpen",
                json!({ "textDocument": {
                    "uri": file_uri(path),
                    "languageId": registry::language_id_for(&spec, path),
                    "version": doc.version, "text": doc.text.to_string(),
                } }),
            );
            doc.opened.push(server);
        }
    }

    /// Start the server unless it is starting, running, or waiting for a manual restart.
    fn ensure_server(&mut self, id: &'static str, cx: &mut Context<Self>) {
        if let Some(s) = self.servers.get(id) {
            match s.state {
                ServerState::Starting | ServerState::Running | ServerState::Crashed { .. } => {
                    return;
                }
                ServerState::NotInstalled { .. } | ServerState::Stopped => {}
            }
        }
        let Some(spec) = self.specs.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        self.start(spec, cx);
    }

    /// Stop (if running) and start again. Also the manual "Restart" after a crash.
    pub fn restart(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(spec) = self.specs.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        if let Some(s) = self.servers.get_mut(spec.id) {
            s.crashes.clear();
        }
        self.stop(spec.id, cx);
        self.start(spec, cx);
    }

    /// Re-check every server's install state (after an install finishes).
    pub fn redetect(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<&'static str> = self
            .servers
            .iter()
            .filter(|(_, s)| matches!(s.state, ServerState::NotInstalled { .. }))
            .map(|(id, _)| *id)
            .collect();
        for id in wanted {
            self.missing_reported.retain(|m| *m != id);
            self.ensure_server(id, cx);
        }
    }

    fn stop(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let Some(s) = self.servers.get_mut(id) else {
            return;
        };
        s.generation += 1; // its exit is expected now
        if let Some(rpc) = s.rpc.take() {
            // Polite first; the kill follows if it lingers.
            let shutdown = rpc.request("shutdown", Value::Null);
            let kill = s.kill.take();
            pide_runtime::spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(1), shutdown).await;
                rpc.notify("exit", Value::Null);
                tokio::time::sleep(Duration::from_millis(300)).await;
                drop(kill); // kills it if it's still there
            });
        }
        s.state = ServerState::Stopped;
        s.progress.clear();
        for doc in self.docs.values_mut() {
            doc.opened.retain(|s| *s != id);
        }
        cx.emit(LspEvent::Status);
        cx.notify();
    }

    fn start(&mut self, spec: ServerSpec, cx: &mut Context<Self>) {
        let id = spec.id;
        let server = self.servers.entry(id).or_insert_with(|| Server {
            spec: spec.clone(),
            state: ServerState::Starting,
            installed: None,
            encoding: Encoding::Utf16,
            capabilities: None,
            progress: BTreeMap::new(),
            rpc: None,
            kill: None,
            crashes: Vec::new(),
            generation: 0,
            _tasks: Vec::new(),
        });
        server.spec = spec.clone();
        server.state = ServerState::Starting;
        server.generation += 1;
        let generation = server.generation;
        cx.emit(LspEvent::Status);

        // Detect off the UI thread, then start.
        let detect_spec = spec.clone();
        let detect = cx
            .background_executor()
            .spawn(async move { registry::detect(&detect_spec) });
        let task = cx.spawn(async move |this, cx| {
            let installed = detect.await;
            let _ = this.update(cx, |this, cx| this.detected(id, generation, installed, cx));
        });
        server._tasks = vec![task];
    }

    fn detected(
        &mut self,
        id: &'static str,
        generation: u64,
        installed: Installed,
        cx: &mut Context<Self>,
    ) {
        let root = self.root.clone();
        let Some(s) = self
            .servers
            .get_mut(id)
            .filter(|s| s.generation == generation)
        else {
            return;
        };
        s.installed = Some(installed.clone());
        if let Installed::No { reason } = installed {
            log::info!("{id}: not installed ({reason})");
            s.state = ServerState::NotInstalled { reason };
            cx.emit(LspEvent::Status);
            // A missing spell checker isn't worth a notice on every project:
            // the Language Servers tab and the setting say how to get it.
            if !s.spec.addon && !self.missing_reported.contains(&id) {
                self.missing_reported.push(id);
                cx.emit(LspEvent::MissingServer { server: id });
            }
            cx.notify();
            return;
        }
        log::info!("{id}: starting in {}", root.display());
        let started = process::spawn(&s.spec, &root);
        let rpc = started.rpc.clone();
        s.rpc = Some(started.rpc);
        s.kill = Some(started.kill);
        let init = rpc.request("initialize", process::initialize_params(&root));

        let mut incoming = started.incoming;
        let incoming_task = cx.spawn(async move |this, cx| {
            while let Some(msg) = incoming.recv().await {
                if this
                    .update(cx, |this, cx| this.on_incoming(id, generation, msg, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let exit = started.exit;
        let exit_task = cx.spawn(async move |this, cx| {
            let exit = exit.await.ok();
            let _ = this.update(cx, |this, cx| this.on_exit(id, generation, exit, cx));
        });
        let init_task = cx.spawn(async move |this, cx| {
            let result = init.await;
            let _ = this.update(cx, |this, cx| {
                this.on_initialized(id, generation, result, cx)
            });
        });
        s._tasks = vec![incoming_task, exit_task, init_task];
        cx.notify();
    }

    fn on_initialized(
        &mut self,
        id: &'static str,
        generation: u64,
        result: Result<Result<Value, crate::rpc::RpcError>, oneshot::error::RecvError>,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = self
            .servers
            .get_mut(id)
            .filter(|s| s.generation == generation)
        else {
            return;
        };
        let value = match result {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                log::error!("{id}: initialize failed: {e}");
                return; // the exit (or a hang) decides what happens next
            }
            Err(_) => return,
        };
        let caps: ServerCapabilities =
            serde_json::from_value(value["capabilities"].clone()).unwrap_or_default();
        s.encoding = Encoding::from_capability(caps.position_encoding.as_ref());
        s.capabilities = Some(caps);
        s.state = ServerState::Running;
        if let Some(rpc) = &s.rpc {
            rpc.notify("initialized", json!({}));
        }
        log::info!("{id}: running ({:?} positions)", s.encoding);
        self.open_docs(id);
        cx.emit(LspEvent::Status);
        cx.notify();
    }

    fn on_exit(
        &mut self,
        id: &'static str,
        generation: u64,
        exit: Option<process::Exit>,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = self
            .servers
            .get_mut(id)
            .filter(|s| s.generation == generation)
        else {
            return;
        };
        let exit = exit.unwrap_or(process::Exit {
            code: None,
            stderr_tail: vec![],
            spawn_error: None,
        });
        let message = match (&exit.spawn_error, exit.code) {
            (Some(e), _) => format!("couldn't start: {e}"),
            (None, Some(c)) => format!("exited with code {c}"),
            (None, None) => "was killed".to_string(),
        };
        log::warn!("{id}: {message}");
        s.rpc = None;
        s.kill = None;
        s.progress.clear();
        s.state = ServerState::Crashed {
            message,
            stderr_tail: exit.stderr_tail,
        };
        for doc in self.docs.values_mut() {
            doc.opened.retain(|s| *s != id);
        }
        // Its diagnostics are no longer maintained.
        let mut gone_addon = Vec::new();
        for (p, by) in &mut self.addon_diagnostics {
            if by.remove(id).is_some() {
                gone_addon.push(p.clone());
            }
        }
        self.addon_diagnostics.retain(|_, by| !by.is_empty());
        for p in gone_addon {
            cx.emit(LspEvent::Diagnostics { path: p });
        }
        let stale: Vec<PathBuf> = self
            .diagnostics
            .keys()
            .filter(|p| registry::spec_for(&self.specs, p).is_some_and(|sp| sp.id == id))
            .cloned()
            .collect();
        for p in stale {
            self.diagnostics.remove(&p);
            cx.emit(LspEvent::Diagnostics { path: p });
        }
        let s = self.servers.get_mut(id).unwrap();
        let now = Instant::now();
        s.crashes.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
        s.crashes.push(now);
        let auto = s.crashes.len() <= MAX_AUTO_RESTARTS && exit.spawn_error.is_none();
        cx.emit(LspEvent::Status);
        cx.notify();
        if auto {
            let spec = s.spec.clone();
            s._tasks = vec![cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let _ = this.update(cx, |this, cx| {
                    if this
                        .servers
                        .get(id)
                        .is_some_and(|s| s.generation == generation)
                    {
                        log::info!("{id}: restarting after a crash");
                        this.start(spec, cx);
                    }
                });
            })];
        }
    }

    fn on_incoming(
        &mut self,
        id: &'static str,
        generation: u64,
        msg: Incoming,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = self
            .servers
            .get_mut(id)
            .filter(|s| s.generation == generation)
        else {
            return;
        };
        match msg {
            Incoming::Notification { method, params } => match method.as_str() {
                "textDocument/publishDiagnostics" => {
                    let Ok(p) =
                        serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params)
                    else {
                        return;
                    };
                    let Some(path) = uri_path(p.uri.as_str()) else {
                        return;
                    };
                    if s.spec.addon {
                        let by = self.addon_diagnostics.entry(path.clone()).or_default();
                        if p.diagnostics.is_empty() {
                            by.remove(id);
                        } else {
                            by.insert(
                                id,
                                FileDiagnostics {
                                    encoding: s.encoding,
                                    items: p.diagnostics,
                                },
                            );
                        }
                        if by.is_empty() {
                            self.addon_diagnostics.remove(&path);
                        }
                    } else if p.diagnostics.is_empty() {
                        self.diagnostics.remove(&path);
                    } else {
                        self.diagnostics.insert(
                            path.clone(),
                            FileDiagnostics {
                                encoding: s.encoding,
                                items: p.diagnostics,
                            },
                        );
                    }
                    cx.emit(LspEvent::Diagnostics { path });
                    cx.notify();
                }
                "$/progress" => {
                    let token = match &params["token"] {
                        Value::String(t) => t.clone(),
                        other => other.to_string(),
                    };
                    let v = &params["value"];
                    match v["kind"].as_str() {
                        Some("begin") | Some("report") => {
                            let title = v["title"]
                                .as_str()
                                .map(str::to_string)
                                .or_else(|| s.progress.get(&token).cloned())
                                .unwrap_or_default();
                            let title = title.split(" · ").next().unwrap_or("").to_string();
                            let mut text = title;
                            if let Some(m) = v["message"].as_str() {
                                text = format!("{text} · {m}");
                            }
                            if let Some(pct) = v["percentage"].as_u64() {
                                text = format!("{text} {pct}%");
                            }
                            s.progress.insert(token, text);
                        }
                        Some("end") => {
                            s.progress.remove(&token);
                        }
                        _ => {}
                    }
                    cx.emit(LspEvent::Status);
                    cx.notify();
                }
                "window/logMessage" | "window/showMessage" => {
                    log::info!("{id}: {}", params["message"].as_str().unwrap_or(""));
                }
                _ => {}
            },
            Incoming::Request {
                id: req_id,
                method,
                params,
            } => {
                let Some(rpc) = s.rpc.clone() else { return };
                let result = match method.as_str() {
                    // One `null` per requested item: "use your defaults".
                    "workspace/configuration" => {
                        let n = params["items"].as_array().map(Vec::len).unwrap_or(0);
                        Value::Array(vec![Value::Null; n])
                    }
                    "workspace/applyEdit" => {
                        match serde_json::from_value::<lsp_types::ApplyWorkspaceEditParams>(params)
                        {
                            Ok(p) => {
                                let req = ApplyEditRequest {
                                    server: id,
                                    id: req_id,
                                    edit: p.edit,
                                    encoding: s.encoding,
                                };
                                cx.emit(LspEvent::ApplyEdit(std::rc::Rc::new(req)));
                                return; // answered by `respond` once applied
                            }
                            Err(e) => json!({ "applied": false, "failureReason": e.to_string() }),
                        }
                    }
                    _ => Value::Null,
                };
                rpc.respond(req_id, result);
            }
        }
    }

    /// Send a request about `path` to its running server, after syncing `text`
    /// (the editor's current rope). Errors if there is no running server.
    pub fn request(
        &mut self,
        path: &Path,
        text: Option<&Rope>,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Value>> {
        if let Some(text) = text {
            self.sync_text(path, text);
        }
        let rpc = self.server_for(path).and_then(|s| s.rpc.clone());
        self.send(rpc, path, method, params, cx)
    }

    /// Like `request`, to one server by id (an add-on's code actions).
    pub fn request_to(
        &mut self,
        server: &str,
        path: &Path,
        text: Option<&Rope>,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Value>> {
        if let Some(text) = text {
            self.sync_text(path, text);
        }
        let rpc = self.running(server).and_then(|s| s.rpc.clone());
        self.send(rpc, path, method, params, cx)
    }

    fn send(
        &self,
        rpc: Option<Rpc>,
        path: &Path,
        method: &str,
        params: Value,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Value>> {
        let Some(rpc) = rpc else {
            return Task::ready(Err(anyhow!(
                "no language server running for {}",
                path.display()
            )));
        };
        let rx = rpc.request(method, params);
        let method = method.to_string();
        cx.background_executor().spawn(async move {
            match rx.await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(anyhow::Error::new(e)).with_context(|| method.clone()),
                Err(_) => Err(anyhow!("{method}: language server stopped")),
            }
        })
    }

    /// Answer a request the server sent us.
    pub fn respond(&self, server: &str, id: Value, result: Value) {
        if let Some(rpc) = self.servers.get(server).and_then(|s| s.rpc.clone()) {
            rpc.respond(id, result);
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
