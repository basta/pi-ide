//! Starting a server process on the tokio runtime, and the `initialize` request.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{mpsc, oneshot};

use crate::registry::{ServerSpec, find_command, search_path};
use crate::rpc::{self, Incoming, RawLog, Rpc};

const STDERR_TAIL: usize = 20;

/// How a server process ended.
#[derive(Debug, Clone)]
pub struct Exit {
    pub code: Option<i32>,
    pub stderr_tail: Vec<String>,
    /// It never started.
    pub spawn_error: Option<String>,
}

pub struct Started {
    pub rpc: Rpc,
    pub incoming: mpsc::UnboundedReceiver<Incoming>,
    pub exit: oneshot::Receiver<Exit>,
    /// Send (or drop) to kill the process.
    pub kill: oneshot::Sender<()>,
}

/// Start `spec` in `root`. Returns at once; the process runs on the runtime.
pub fn spawn(spec: &ServerSpec, root: &Path) -> Started {
    let (rpc, out_rx, pending) = rpc::channel();
    let (in_tx, incoming) = mpsc::unbounded_channel();
    let (exit_tx, exit) = oneshot::channel();
    let (kill, kill_rx) = oneshot::channel();
    let command = find_command(&spec.command).unwrap_or_else(|| spec.command.clone().into());
    let args = spec.args.clone();
    let root = root.to_path_buf();
    let id = spec.id;
    pide_runtime::spawn(async move {
        let mut cmd = tokio::process::Command::from(pide_runtime::process::command_for(&command));
        let mut child = match cmd
            .args(&args)
            .current_dir(&root)
            .env(
                "PATH",
                std::env::join_paths(search_path()).unwrap_or_default(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = exit_tx.send(Exit {
                    code: None,
                    stderr_tail: vec![],
                    spawn_error: Some(format!("{}: {e}", command.display())),
                });
                return;
            }
        };
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        // stderr → a log file and a short tail for the crash message. Never the UI directly.
        let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
        let stderr_task = {
            let tail = tail.clone();
            let path = pide_core::paths::state_dir().join(format!("lsp-{id}-stderr.log"));
            tokio::spawn(async move {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
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

        let log = Arc::new(RawLog::open(
            pide_core::paths::state_dir().join("lsp-rpc.jsonl"),
            id,
        ));
        tokio::select! {
            _ = rpc::run(stdout, stdin, out_rx, pending, in_tx, log) => {}
            _ = kill_rx => { let _ = child.start_kill(); }
        }
        let status = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        if status.is_err() {
            let _ = child.start_kill();
        }
        let _ = tokio::time::timeout(Duration::from_millis(200), stderr_task).await;
        let code = status.ok().and_then(|s| s.ok()).and_then(|s| s.code());
        let stderr_tail = tail.lock().unwrap().iter().cloned().collect();
        let _ = exit_tx.send(Exit {
            code,
            stderr_tail,
            spawn_error: None,
        });
    });
    Started {
        rpc,
        incoming,
        exit,
        kill,
    }
}

pub fn file_uri(path: &Path) -> String {
    // Percent-encode what a URI can't hold; keep `/`.
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn uri_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(std::path::PathBuf::from(String::from_utf8(out).ok()?))
}

/// What we tell servers we can do. Only what pide actually handles.
pub fn initialize_params(root: &Path) -> Value {
    let uri = file_uri(root);
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "pide", "version": env!("CARGO_PKG_VERSION") },
        "rootUri": uri,
        "workspaceFolders": [{ "uri": uri, "name": name }],
        "capabilities": {
            "general": { "positionEncodings": ["utf-32", "utf-16"] },
            "window": { "workDoneProgress": true },
            "workspace": {
                "configuration": true,
                "workspaceFolders": true,
                "applyEdit": true,
                "executeCommand": { "dynamicRegistration": false },
                "workspaceEdit": { "documentChanges": false }
            },
            "textDocument": {
                "synchronization": { "didSave": true, "dynamicRegistration": false },
                "publishDiagnostics": { "relatedInformation": false },
                "completion": {
                    "contextSupport": true,
                    "completionItem": {
                        "snippetSupport": false,
                        "documentationFormat": ["markdown", "plaintext"],
                        "insertReplaceSupport": false,
                        "labelDetailsSupport": true,
                        // Auto-import edits come on resolve (rust-analyzer only offers
                        // import-on-the-fly completions to clients that resolve them).
                        "resolveSupport": { "properties": ["additionalTextEdits"] }
                    }
                },
                "hover": { "contentFormat": ["markdown", "plaintext"] },
                "definition": { "linkSupport": true },
                "codeAction": {
                    "codeActionLiteralSupport": {
                        "codeActionKind": { "valueSet": [
                            "", "quickfix", "refactor", "refactor.extract", "refactor.inline",
                            "refactor.rewrite", "source", "source.organizeImports"
                        ] }
                    },
                    "resolveSupport": { "properties": ["edit"] }
                },
                "rename": { "prepareSupport": true },
                "signatureHelp": {
                    "signatureInformation": {
                        "documentationFormat": ["markdown", "plaintext"],
                        "parameterInformation": { "labelOffsetSupport": true }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip_with_spaces_and_non_ascii() {
        let p = Path::new("/Users/ja/My Projects/čaj/src/main.rs");
        let uri = file_uri(p);
        assert_eq!(uri, "file:///Users/ja/My%20Projects/%C4%8Daj/src/main.rs");
        assert_eq!(uri_path(&uri).as_deref(), Some(p));
        assert_eq!(
            uri_path("file:///a/b.rs").as_deref(),
            Some(Path::new("/a/b.rs"))
        );
        assert_eq!(uri_path("untitled:1"), None);
    }
}
