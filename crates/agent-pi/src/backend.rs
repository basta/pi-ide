//! [`AgentBackend`] over a `pi --mode rpc` child process.
//!
//! Lifecycle (spec: Lifecycle): started lazily on the first prompt with the
//! project root as working directory; if it exits unexpectedly the agent goes
//! to `Crashed` with the last 20 stderr lines and a restart action; `abort`
//! takes effect within 1 s (if pi hasn't settled by then, the process is
//! killed and the next prompt resumes the same session).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::*;
use pide_core::{Agent, AgentBackend, AgentEvent, AgentStatus, paths};
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{mpsc, oneshot};

use crate::conn::{self, RawLog};
use crate::wire::Inbound;

const STDERR_TAIL: usize = 20;
const ABORT_DEADLINE: Duration = Duration::from_secs(1);
/// An idle session stops its pi process after this long (spec M2); the next
/// prompt resumes it with `--session <file>`.
const IDLE_STOP: Duration = Duration::from_secs(5 * 60);

/// State shared between the backend and its inbound task.
#[derive(Default)]
struct Shared {
    /// pi's session file, from `get_state` (or the one we resumed); used to resume after a stop.
    session_file: Option<PathBuf>,
    /// Set before we kill/stop pi on purpose, so its exit isn't a crash.
    expected_exit: bool,
    /// The process is running.
    alive: bool,
    kill: Option<oneshot::Sender<()>>,
    idle_stop: Option<Task<()>>,
}

impl Shared {
    fn stop(&mut self) {
        if self.alive {
            self.expected_exit = true;
        }
        if let Some(k) = self.kill.take() {
            let _ = k.send(());
        }
        self.idle_stop = None;
    }
}

struct Process {
    commands: mpsc::UnboundedSender<String>,
    _inbound: Task<()>,
}

static NEXT_SESSION_TAG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct PiBackend {
    root: PathBuf,
    process: Option<Process>,
    shared: Rc<RefCell<Shared>>,
    next_id: u64,
    /// Tags this session's lines in the shared raw log.
    tag: u64,
    abort_deadline: Option<Task<()>>,
}

impl PiBackend {
    /// `resume`: an existing pi session file to continue.
    pub fn new(root: PathBuf, resume: Option<PathBuf>) -> Self {
        let shared = Shared { session_file: resume, ..Default::default() };
        Self {
            root,
            process: None,
            shared: Rc::new(RefCell::new(shared)),
            next_id: 0,
            tag: NEXT_SESSION_TAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            abort_deadline: None,
        }
    }

    fn send(&mut self, mut cmd: Value) {
        self.next_id += 1;
        cmd["id"] = json!(format!("pide-{}-{}", self.tag, self.next_id));
        if let Some(p) = &self.process {
            let _ = p.commands.send(cmd.to_string());
        }
    }

    fn kill(&mut self) {
        self.shared.borrow_mut().stop();
        self.process = None;
    }

    fn ensure_started(&mut self, agent: &mut Agent, cx: &mut Context<Agent>) {
        if !self.shared.borrow().alive {
            self.process = None; // stopped (idle, killed, crashed) since we last looked
        }
        if self.process.is_some() {
            return;
        }
        let Some(pi) = find_pi() else {
            let message = "Can't find the `pi` executable (looked on PATH, /opt/homebrew/bin, /usr/local/bin). Install it with `npm i -g @earendil-works/pi-coding-agent`.".to_string();
            cx.emit(AgentEvent::Error(message.clone()));
            agent.set_status(AgentStatus::Crashed { message, stderr_tail: vec![] }, cx);
            return;
        };

        let mut args = vec!["--mode".to_string(), "rpc".to_string()];
        if let Some(session) = self.shared.borrow().session_file.clone() {
            args.extend(["--session".to_string(), session.to_string_lossy().into_owned()]);
        }
        {
            let mut sh = self.shared.borrow_mut();
            sh.expected_exit = false;
            sh.alive = true;
        }
        log::info!("starting {} {} in {}", pi.display(), args.join(" "), self.root.display());

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<String>();
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let (exit_tx, exit_rx) = oneshot::channel::<(Option<i32>, Vec<String>, Option<String>)>();

        self.shared.borrow_mut().kill = Some(kill_tx);
        pide_runtime::spawn(supervise(pi, args, self.root.clone(), self.tag, cmd_rx, in_tx, kill_rx, exit_tx));

        // UI side: apply everything pi sends, then handle its exit.
        let shared = self.shared.clone();
        let inbound = cx.spawn(async move |agent: WeakEntity<Agent>, cx| {
            while let Some(rec) = in_rx.recv().await {
                let ok = agent.update(cx, |agent, cx| match rec {
                    Inbound::Event(AgentEvent::UserPrompt { .. }) => {} // echoed by us on send
                    Inbound::Event(e @ AgentEvent::Settled { .. }) => {
                        agent.handle_event(e, cx);
                        schedule_idle_stop(&shared, cx);
                    }
                    Inbound::Event(e) => agent.handle_event(e, cx),
                    Inbound::Response { command, success, error, data, .. } => {
                        handle_response(&shared, agent, &command, success, error, data, cx)
                    }
                    Inbound::Dialog { .. } => {} // answered in conn
                });
                if ok.is_err() {
                    return;
                }
            }
            let (code, stderr_tail, spawn_error) = exit_rx.await.unwrap_or((None, vec![], None));
            let expected = {
                let mut sh = shared.borrow_mut();
                sh.alive = false;
                sh.kill = None;
                sh.idle_stop = None;
                std::mem::take(&mut sh.expected_exit)
            };
            let _ = agent.update(cx, |agent, cx| {
                if expected {
                    return;
                }
                let message = match spawn_error {
                    Some(e) => format!("Couldn't start pi: {e}"),
                    None => format!("pi exited unexpectedly (exit code {}).", code.map_or("none".into(), |c| c.to_string())),
                };
                agent.handle_event(AgentEvent::Exited { code, stderr_tail: stderr_tail.clone() }, cx);
                agent.set_status(AgentStatus::Crashed { message, stderr_tail }, cx);
            });
        });

        self.process = Some(Process { commands: cmd_tx, _inbound: inbound });
        agent.set_status(AgentStatus::Starting, cx);
        self.send(json!({"type": "get_state"}));
    }
}

fn handle_response(
    shared: &Rc<RefCell<Shared>>,
    agent: &mut Agent,
    command: &str,
    success: bool,
    error: Option<String>,
    data: Value,
    cx: &mut Context<Agent>,
) {
    if !success {
        let msg = format!("pi rejected `{command}`: {}", error.unwrap_or_else(|| "no reason given".into()));
        log::warn!("{msg}");
        agent.handle_event(AgentEvent::Error(msg), cx);
        if command == "prompt" {
            agent.set_status(AgentStatus::Idle, cx);
        }
        return;
    }
    match command {
        "get_state" => {
            if let Some(f) = data.get("sessionFile").and_then(Value::as_str) {
                shared.borrow_mut().session_file = Some(PathBuf::from(f));
                agent.session_file = Some(PathBuf::from(f));
            }
            if let Some(n) = data.get("sessionName").and_then(Value::as_str) {
                agent.name = n.to_string().into();
            }
            if matches!(agent.status(), AgentStatus::Starting) {
                agent.set_status(AgentStatus::Idle, cx);
            }
        }
        "prompt" => match data.get("disposition").and_then(Value::as_str) {
            Some("handled") => agent.set_status(AgentStatus::Idle, cx),
            _ => agent.set_status(AgentStatus::Busy, cx),
        },
        "new_session" => {
            shared.borrow_mut().session_file = None;
            agent.session_file = None;
        }
        _ => {}
    }
}

impl AgentBackend for PiBackend {
    fn prompt(&mut self, text: String, agent: &mut Agent, cx: &mut Context<Agent>) {
        if agent.is_busy() && self.process.is_some() && !matches!(agent.status(), AgentStatus::Starting) {
            cx.emit(AgentEvent::Error("pi is still working. Steering and follow-ups arrive in M2.".into()));
            return;
        }
        if matches!(agent.status(), AgentStatus::Crashed { .. }) {
            agent.set_status(AgentStatus::Idle, cx);
        }
        self.shared.borrow_mut().idle_stop = None;
        self.ensure_started(agent, cx);
        if self.process.is_none() {
            return;
        }
        cx.emit(AgentEvent::UserPrompt { text: text.clone() });
        self.send(json!({"type": "prompt", "message": text}));
        agent.set_status(AgentStatus::Busy, cx);
    }

    fn abort(&mut self, agent: &mut Agent, cx: &mut Context<Agent>) {
        if !self.shared.borrow().alive || !agent.is_busy() {
            return;
        }
        self.send(json!({"type": "abort"}));
        agent.set_status(AgentStatus::Aborting, cx);
        // Deadline: if pi hasn't settled within 1 s, stop it hard.
        self.abort_deadline = Some(cx.spawn(async move |agent: WeakEntity<Agent>, cx| {
            cx.background_executor().timer(ABORT_DEADLINE).await;
            let _ = agent.update(cx, |agent, cx| {
                if matches!(agent.status(), AgentStatus::Aborting) {
                    log::warn!("pi did not settle within {ABORT_DEADLINE:?} of abort; killing it");
                    agent.restart(cx);
                    agent.handle_event(
                        AgentEvent::Notify {
                            message: "pi didn't stop within 1 s, so it was stopped. Your next prompt continues the same session.".into(),
                            error: false,
                        },
                        cx,
                    );
                    agent.handle_event(AgentEvent::Settled { aborted: true }, cx);
                }
            });
        }));
    }

    fn new_session(&mut self, _agent: &mut Agent, _cx: &mut Context<Agent>) {
        if self.shared.borrow().alive {
            self.send(json!({"type": "new_session"}));
        }
        self.shared.borrow_mut().session_file = None;
    }

    fn restart(&mut self, agent: &mut Agent, cx: &mut Context<Agent>) {
        self.kill();
        self.abort_deadline = None;
        agent.set_status(AgentStatus::Idle, cx);
    }
}

/// After a run settles, stop the process if the session stays idle for [`IDLE_STOP`].
fn schedule_idle_stop(shared: &Rc<RefCell<Shared>>, cx: &mut Context<Agent>) {
    let weak = Rc::downgrade(shared);
    let task = cx.spawn(async move |agent: WeakEntity<Agent>, cx| {
        cx.background_executor().timer(IDLE_STOP).await;
        let _ = agent.update(cx, |agent, _| {
            if agent.is_busy() || agent.waiting {
                return;
            }
            if let Some(shared) = weak.upgrade() {
                log::info!("session \"{}\" idle for {IDLE_STOP:?}; stopping its pi process", agent.name);
                // Don't drop this task from inside itself: take only the kill switch.
                let mut sh = shared.borrow_mut();
                if sh.alive {
                    sh.expected_exit = true;
                }
                if let Some(k) = sh.kill.take() {
                    let _ = k.send(());
                }
            }
        });
    });
    shared.borrow_mut().idle_stop = Some(task);
}

impl Drop for PiBackend {
    fn drop(&mut self) {
        self.kill();
    }
}

/// `pi` on PATH, or in the usual npm-global locations (a GUI launch may have a bare PATH).
fn find_pi() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs.into_iter().map(|d| d.join("pi")).find(|p| p.is_file())
}

/// Runs on the runtime thread: owns the child, its pipes and the connection.
async fn supervise(
    pi: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    tag: u64,
    commands: mpsc::UnboundedReceiver<String>,
    inbound: mpsc::UnboundedSender<Inbound>,
    kill: oneshot::Receiver<()>,
    exit: oneshot::Sender<(Option<i32>, Vec<String>, Option<String>)>,
) {
    // pi is a node script (`#!/usr/bin/env node`); make sure node's dir is on PATH.
    let mut path = pi.parent().map(Path::to_path_buf).into_iter().collect::<Vec<_>>();
    if let Some(p) = std::env::var_os("PATH") {
        path.extend(std::env::split_paths(&p));
    }
    let mut child = match tokio::process::Command::new(&pi)
        .args(&args)
        .current_dir(&cwd)
        .env("PATH", std::env::join_paths(path).unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            drop(inbound);
            let _ = exit.send((None, vec![], Some(e.to_string())));
            return;
        }
    };

    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    // stderr → log file + last-N ring buffer. Never to the UI directly.
    let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let stderr_task = {
        let tail = tail.clone();
        let log_path = paths::state_dir().join("pi-stderr.log");
        tokio::spawn(async move {
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path).ok();
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(f) = file.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
                let mut t = tail.lock().unwrap();
                if t.len() == STDERR_TAIL {
                    t.pop_front();
                }
                t.push_back(line);
            }
        })
    };

    let log = Arc::new(RawLog::open(paths::state_dir().join("pi-rpc.jsonl")).tagged(tag));
    let connection = conn::run(stdout, stdin, commands, inbound, log);
    tokio::select! {
        _ = connection => {}
        _ = kill => {
            let _ = child.start_kill();
        }
    }
    let status = child.wait().await.ok();
    let _ = tokio::time::timeout(Duration::from_millis(200), stderr_task).await;
    let tail: Vec<String> = tail.lock().unwrap().iter().cloned().collect();
    let _ = exit.send((status.and_then(|s| s.code()), tail, None));
}
