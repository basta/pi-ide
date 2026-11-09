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
use pide_core::{Agent, AgentBackend, AgentEvent, AgentStatus, DialogAnswer, paths};
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
    /// Prompts we already echoed into the chat; pi's own echo of them is dropped.
    /// Steered and follow-up messages are shown when pi delivers them.
    echoed: std::collections::VecDeque<String>,
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
        let shared = Shared {
            session_file: resume,
            ..Default::default()
        };
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

    /// Write a record as-is (no command id), e.g. an `extension_ui_response`.
    fn send_raw(&mut self, rec: Value) {
        if let Some(p) = &self.process {
            let _ = p.commands.send(rec.to_string());
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
            agent.set_status(
                AgentStatus::Crashed {
                    message,
                    stderr_tail: vec![],
                },
                cx,
            );
            return;
        };

        let mut args = vec!["--mode".to_string(), "rpc".to_string()];
        // Extra flags for every pi process, e.g. PIDE_PI_ARGS="-e dev/pi-ask-test.ts" or "--model …".
        if let Ok(extra) = std::env::var("PIDE_PI_ARGS") {
            args.extend(extra.split_whitespace().map(str::to_string));
        }
        if let Some(session) = self.shared.borrow().session_file.clone() {
            args.extend([
                "--session".to_string(),
                session.to_string_lossy().into_owned(),
            ]);
        }
        {
            let mut sh = self.shared.borrow_mut();
            sh.expected_exit = false;
            sh.alive = true;
        }
        log::info!(
            "starting {} {} in {}",
            pi.display(),
            args.join(" "),
            self.root.display()
        );

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<String>();
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let (exit_tx, exit_rx) = oneshot::channel::<(Option<i32>, Vec<String>, Option<String>)>();

        self.shared.borrow_mut().kill = Some(kill_tx);
        pide_runtime::spawn(supervise(
            pi,
            args,
            self.root.clone(),
            self.tag,
            cmd_rx,
            in_tx,
            kill_rx,
            exit_tx,
        ));

        // UI side: apply everything pi sends, then handle its exit.
        let shared = self.shared.clone();
        let stats_tx = cmd_tx.clone();
        let tag = self.tag;
        let inbound = cx.spawn(async move |agent: WeakEntity<Agent>, cx| {
            while let Some(rec) = in_rx.recv().await {
                let ok = agent.update(cx, |agent, cx| match rec {
                    Inbound::Event(AgentEvent::UserPrompt { text }) => {
                        let mut sh = shared.borrow_mut();
                        if let Some(i) = sh.echoed.iter().position(|t| *t == text) {
                            sh.echoed.remove(i); // already shown when sent
                        } else {
                            drop(sh);
                            agent.handle_event(AgentEvent::UserPrompt { text }, cx); // delivered steer/follow-up
                        }
                    }
                    Inbound::Event(e @ AgentEvent::Settled { .. }) => {
                        agent.handle_event(e, cx);
                        // Cost and context use after each run.
                        let _ = stats_tx.send(
                            json!({"type": "get_session_stats", "id": format!("pide-{tag}-stats")})
                                .to_string(),
                        );
                        schedule_idle_stop(&shared, cx);
                    }
                    Inbound::Event(e) => agent.handle_event(e, cx),
                    Inbound::Response {
                        command,
                        success,
                        error,
                        data,
                        ..
                    } => handle_response(&shared, agent, &command, success, error, data, cx),
                    Inbound::Dialog(req) => agent.handle_event(AgentEvent::Dialog(req), cx),
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
                    None => format!(
                        "pi exited unexpectedly (exit code {}).",
                        code.map_or("none".into(), |c| c.to_string())
                    ),
                };
                agent.handle_event(
                    AgentEvent::Exited {
                        code,
                        stderr_tail: stderr_tail.clone(),
                    },
                    cx,
                );
                agent.set_status(
                    AgentStatus::Crashed {
                        message,
                        stderr_tail,
                    },
                    cx,
                );
            });
        });

        self.process = Some(Process {
            commands: cmd_tx,
            _inbound: inbound,
        });
        agent.set_status(AgentStatus::Starting, cx);
        self.send(json!({"type": "get_state"}));
        self.send(json!({"type": "get_session_stats"}));
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
        let reason = error.unwrap_or_else(|| "no reason given".into());
        let msg = match command {
            "compact" => format!("Not compacted: {reason}."),
            _ => format!("pi rejected `{command}`: {reason}"),
        };
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
            agent.model = data.get("model").and_then(crate::wire::model_info);
            agent.thinking = data
                .get("thinkingLevel")
                .and_then(Value::as_str)
                .map(str::to_string);
            cx.notify();
            if matches!(agent.status(), AgentStatus::Starting) {
                agent.set_status(AgentStatus::Idle, cx);
            }
        }
        "prompt" => match data.get("disposition").and_then(Value::as_str) {
            Some("handled") => agent.set_status(AgentStatus::Idle, cx),
            _ => agent.set_status(AgentStatus::Busy, cx),
        },
        "set_model" => {
            agent.model = crate::wire::model_info(&data);
            cx.notify();
        }
        "compact" => {
            let n = |k: &str| {
                data[k]
                    .as_u64()
                    .map(|t| format!("{:.1}k", t as f64 / 1000.0))
            };
            let message = match (n("tokensBefore"), n("estimatedTokensAfter")) {
                (Some(b), Some(a)) => format!("Compacted: {b} → about {a} tokens of context."),
                _ => "Compacted.".to_string(),
            };
            if let Some(s) = agent.stats.as_mut() {
                s.context_percent = None; // unknown until the next reply
            }
            agent.handle_event(
                AgentEvent::Notify {
                    message,
                    error: false,
                },
                cx,
            );
        }
        "get_session_stats" => {
            agent.stats = Some(crate::wire::session_stats(&data));
            cx.notify();
        }
        "get_commands" => {
            agent.commands = crate::wire::slash_commands(&data);
            agent.commands_loaded = true;
            cx.notify();
        }
        "get_available_models" => {
            agent.models = data
                .get("models")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(crate::wire::model_info).collect())
                .unwrap_or_default();
            cx.notify();
        }
        "new_session" => {
            shared.borrow_mut().session_file = None;
            agent.session_file = None;
        }
        _ => {}
    }
}

impl PiBackend {
    /// Send a command that needs the process, starting it if needed.
    fn send_started(&mut self, msg: Value, agent: &mut Agent, cx: &mut Context<Agent>) {
        self.shared.borrow_mut().idle_stop = None;
        self.ensure_started(agent, cx);
        if self.process.is_some() {
            self.send(msg);
        }
    }
}

impl AgentBackend for PiBackend {
    fn compact(&mut self, agent: &mut Agent, cx: &mut Context<Agent>) {
        agent.handle_event(
            AgentEvent::Notify {
                message: "Compacting the conversation…".into(),
                error: false,
            },
            cx,
        );
        self.send_started(json!({"type": "compact"}), agent, cx);
    }

    fn request_commands(&mut self, agent: &mut Agent, cx: &mut Context<Agent>) {
        self.send_started(json!({"type": "get_commands"}), agent, cx);
    }

    fn request_models(&mut self, agent: &mut Agent, cx: &mut Context<Agent>) {
        self.send_started(json!({"type": "get_available_models"}), agent, cx);
    }

    fn set_model(
        &mut self,
        provider: String,
        id: String,
        agent: &mut Agent,
        cx: &mut Context<Agent>,
    ) {
        self.send_started(
            json!({"type": "set_model", "provider": provider, "modelId": id}),
            agent,
            cx,
        );
        // The thinking level may change with the model; pi answers in order.
        if self.process.is_some() {
            self.send(json!({"type": "get_state"}));
        }
    }

    fn prompt(&mut self, text: String, agent: &mut Agent, cx: &mut Context<Agent>) {
        if agent.is_busy()
            && self.process.is_some()
            && !matches!(agent.status(), AgentStatus::Starting)
        {
            // Busy: a plain prompt would be rejected by pi; steer instead.
            self.queue(text, false, agent, cx);
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
        self.shared.borrow_mut().echoed.push_back(text.clone());
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

    fn queue(&mut self, text: String, follow_up: bool, agent: &mut Agent, cx: &mut Context<Agent>) {
        if !self.shared.borrow().alive || !agent.is_busy() {
            // Nothing running: just send it.
            self.prompt(text, agent, cx);
            return;
        }
        let kind = if follow_up { "follow_up" } else { "steer" };
        self.send(json!({"type": kind, "message": text}));
    }

    fn answer_dialog(
        &mut self,
        id: String,
        answer: DialogAnswer,
        _agent: &mut Agent,
        _cx: &mut Context<Agent>,
    ) {
        let mut rec = json!({"type": "extension_ui_response", "id": id});
        match answer {
            DialogAnswer::Value(v) => rec["value"] = json!(v),
            DialogAnswer::Confirmed(c) => rec["confirmed"] = json!(c),
            DialogAnswer::Cancelled => rec["cancelled"] = json!(true),
        }
        self.send_raw(rec);
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
                log::info!(
                    "session \"{}\" idle for {IDLE_STOP:?}; stopping its pi process",
                    agent.name
                );
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
    let mut path = pi
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .collect::<Vec<_>>();
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
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .ok();
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
    let connection = conn::run(stdout, stdin, commands, inbound, log, Some(cwd.clone()));
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
