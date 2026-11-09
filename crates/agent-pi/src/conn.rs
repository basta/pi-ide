//! The JSONL connection: one task per pi process on the runtime thread.
//! Generic over the byte streams so tests can drive it without a process.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use pide_core::AgentEvent;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::wire::{Inbound, parse_line};

/// Append-only raw log of every line in both directions (spec: always on).
pub struct RawLog(Option<Mutex<std::fs::File>>, u64);

impl RawLog {
    pub fn open(path: PathBuf) -> Self {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => Self(Some(Mutex::new(f)), 0),
            Err(e) => {
                log::error!("can't open {}: {e}", path.display());
                Self(None, 0)
            }
        }
    }

    pub fn disabled() -> Self {
        Self(None, 0)
    }

    /// Tag every line with a session number (several sessions share one log).
    pub fn tagged(mut self, session: u64) -> Self {
        self.1 = session;
        self
    }

    fn write(&self, dir: &str, line: &str) {
        let Some(f) = &self.0 else { return };
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let rec = json!({ "ts": ts as u64, "session": self.1, "dir": dir, "line": line });
        if let Ok(mut f) = f.lock() {
            let _ = writeln!(f, "{rec}");
        }
    }
}

/// Tools that change files, and how to find the path in their args (pi 1.5.0, recorded).
const FILE_TOOLS: &[&str] = &["edit", "write"];

/// pi's working dir + a relative tool path, with the directory canonicalized
/// (so it matches the workspace's paths, e.g. /tmp vs /private/tmp).
fn resolve(cwd: &Path, rel: &str) -> PathBuf {
    let path = cwd.join(rel);
    match path.parent().and_then(|p| std::fs::canonicalize(p).ok()) {
        Some(dir) => dir.join(path.file_name().unwrap_or_default()),
        None => path,
    }
}

/// Before-snapshots taken while a tool call's arguments are still streaming.
///
/// `tool_execution_start` is too late to read the "before" text: pi runs the tool
/// a few ms after announcing it (measured 3–7 ms with parallel calls) and the read
/// loses the race. But a call's `path` streams in `toolcall_delta`s long before
/// that, and pi only runs a message's tools once the whole message has streamed.
/// So snapshot as soon as the path is complete, and hand it out at the start.
#[derive(Default)]
struct EarlySnapshots {
    /// contentIndex -> (tool call id, tool name, args JSON so far); file tools only.
    streaming: std::collections::HashMap<u64, (String, String, String)>,
    /// tool call id -> (path, before)
    taken: std::collections::HashMap<String, (PathBuf, Option<String>)>,
}

impl EarlySnapshots {
    /// Look at a raw inbound line; snapshot when a file tool's path completes.
    fn observe(&mut self, line: &str, cwd: &Path) {
        if !line.contains("\"toolcall_") {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        let ev = &v["assistantMessageEvent"];
        let Some(index) = ev["contentIndex"].as_u64() else { return };
        match ev["type"].as_str() {
            Some("toolcall_start") => {
                let name = ev["toolName"].as_str().unwrap_or_default();
                let id = ev["id"].as_str().unwrap_or_default();
                if FILE_TOOLS.contains(&name) && !id.is_empty() {
                    self.streaming.insert(index, (id.to_string(), name.to_string(), String::new()));
                } else {
                    self.streaming.remove(&index);
                }
            }
            Some("toolcall_delta") => {
                let Some((id, _, args)) = self.streaming.get_mut(&index) else { return };
                args.push_str(ev["delta"].as_str().unwrap_or_default());
                if let Some(rel) = partial_path(args) {
                    let path = resolve(cwd, &rel);
                    let before = pide_core::changes::snapshot(&path);
                    self.taken.insert(id.clone(), (path, before));
                    self.streaming.remove(&index);
                }
            }
            Some("toolcall_end") => {
                self.streaming.remove(&index);
            }
            _ => {}
        }
    }

    /// For a file-changing tool start, the `FileTouched` event: the early snapshot
    /// if there is one for this call and path, else a read now (may be too late).
    fn touched(&mut self, ev: &AgentEvent, cwd: &Path) -> Option<AgentEvent> {
        let AgentEvent::ToolStart { id, name, args } = ev else { return None };
        if !FILE_TOOLS.contains(&name.as_str()) {
            return None;
        }
        let path = resolve(cwd, args.get("path").and_then(Value::as_str)?);
        let before = match self.taken.remove(id) {
            Some((p, before)) if p == path => before,
            _ => {
                log::warn!("no early snapshot for {name} {}; reading it now (may already be changed)", path.display());
                pide_core::changes::snapshot(&path)
            }
        };
        Some(AgentEvent::FileTouched { tool_call_id: id.clone(), tool: name.clone(), before, path })
    }
}

/// The `path` value from a tool call's partial args JSON, once its closing quote has arrived.
fn partial_path(args: &str) -> Option<String> {
    let after_key = &args[args.find("\"path\"")? + "\"path\"".len()..];
    let rest = after_key.trim_start().strip_prefix(':')?.trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return serde_json::from_str::<String>(&rest[..=i]).ok(),
            _ => i += 1,
        }
    }
    None
}

/// Pump commands to pi and records back until either side closes.
/// Dropping `commands`' senders closes pi's stdin, which asks pi to shut down.
/// `cwd`: pi's working directory, for before-snapshots of files it changes (`None`: off).
pub async fn run<R, W>(
    reader: R,
    mut writer: W,
    mut commands: mpsc::UnboundedReceiver<String>,
    inbound: mpsc::UnboundedSender<Inbound>,
    log: Arc<RawLog>,
    cwd: Option<PathBuf>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::with_capacity(8 * 1024);
    let mut commands_open = true;
    let mut early = EarlySnapshots::default();

    async fn send<W: AsyncWrite + Unpin>(w: &mut W, log: &RawLog, line: &str) -> std::io::Result<()> {
        log.write("out", line);
        w.write_all(line.as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await
    }

    loop {
        tokio::select! {
            cmd = commands.recv(), if commands_open => match cmd {
                Some(line) => {
                    if let Err(e) = send(&mut writer, &log, &line).await {
                        log::warn!("pi stdin closed: {e}");
                        break;
                    }
                }
                None => {
                    commands_open = false;
                    let _ = writer.shutdown().await;
                }
            },
            // read_until is cancel-safe: partial bytes stay in `buf`.
            read = reader.read_until(b'\n', &mut buf) => {
                match read {
                    Ok(0) => break,
                    Ok(_) => {
                        // Split on LF only; strip one trailing CR. Never readline-style.
                        let mut end = buf.len();
                        if buf[..end].ends_with(b"\n") { end -= 1; }
                        if buf[..end].ends_with(b"\r") { end -= 1; }
                        let line = String::from_utf8_lossy(&buf[..end]).into_owned();
                        buf.clear();
                        if line.is_empty() { continue; }
                        log.write("in", &line);
                        if let Some(cwd) = &cwd {
                            early.observe(&line, cwd);
                        }
                        for rec in parse_line(&line) {
                            let touched = match (&rec, &cwd) {
                                (Inbound::Event(ev), Some(cwd)) => early.touched(ev, cwd),
                                _ => None,
                            };
                            if let Some(t) = touched {
                                // Snapshot first, then the start itself.
                                if inbound.send(Inbound::Event(t)).is_err() {
                                    return;
                                }
                            }
                            if inbound.send(rec).is_err() {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("pi stdout read failed: {e}");
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_path_waits_for_closing_quote() {
        assert_eq!(partial_path(""), None);
        assert_eq!(partial_path("{\"path\": \"src/ma"), None);
        assert_eq!(partial_path("{\"path\": \"src/main.rs"), None);
        assert_eq!(partial_path("{\"path\": \"src/main.rs\""), Some("src/main.rs".into()));
        assert_eq!(partial_path("{\"content\":\"x\",\"path\":\"a \\\"b\\\".txt\", \"e"), Some("a \"b\".txt".into()));
    }

    /// Replays the recorded order (pi 1.5.0): args stream, the file changes, then
    /// tool_execution_start. The before-snapshot must be the old content.
    #[test]
    fn snapshot_is_taken_while_args_stream() {
        let dir = std::env::temp_dir().join(format!("pide-early-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "notes\n").unwrap();

        let mut early = EarlySnapshots::default();
        let upd = |e: Value| json!({"type": "message_update", "assistantMessageEvent": e}).to_string();
        early.observe(&upd(json!({"type":"toolcall_start","contentIndex":2,"id":"functions.edit:3","toolName":"edit"})), &dir);
        early.observe(&upd(json!({"type":"toolcall_delta","contentIndex":2,"delta":"{\"path\": \"notes.txt"})), &dir);
        early.observe(&upd(json!({"type":"toolcall_delta","contentIndex":2,"delta":"\", \"edits"})), &dir);
        std::fs::write(&file, "notes\nedited by pi\n").unwrap(); // pi runs the edit
        let start = AgentEvent::ToolStart {
            id: "functions.edit:3".into(),
            name: "edit".into(),
            args: json!({"path": "notes.txt", "edits": []}),
        };
        match early.touched(&start, &dir) {
            Some(AgentEvent::FileTouched { before, .. }) => assert_eq!(before.as_deref(), Some("notes\n")),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
