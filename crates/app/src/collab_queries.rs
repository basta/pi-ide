//! Answers to pide's pi extension (collaborator mode, docs/COLLAB.md): pi's
//! `ide_diagnostics` tool asks what the editor's language servers report.
//! Runs on the UI thread from in-memory state only; files the editor hasn't
//! opened are read in the background and handed to their language server, and
//! the answer says they're pending until it has had a moment to check them.

use std::path::{Path, PathBuf};

use gpui_kit::*;
use pide_core::Workspace;
use serde_json::{Value, json};

/// Most problems sent back in one answer.
const MAX_ITEMS: usize = 200;

/// How long a file just handed to its server counts as pending (no
/// diagnostics yet); after that, no entry means no problems.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(6);

/// Files checked without an editor: when their text last changed (or was first
/// handed over), the hash of that text, and when the last read from disk finished.
#[derive(Clone, Copy)]
struct Check {
    since: std::time::Instant,
    hash: u64,
    read_at: Option<std::time::Instant>,
    /// When this tool call's first question came (its reads are the current ones).
    round: std::time::Instant,
    /// The last read failed (deleted, unreadable).
    missing: bool,
}


type Checking = std::rc::Rc<std::cell::RefCell<std::collections::HashMap<PathBuf, Check>>>;

/// After a file's text changes, its old diagnostics may still be the server's
/// last word for this long.
const FRESH: std::time::Duration = std::time::Duration::from_millis(2500);

pub fn handler(workspace: WeakEntity<Workspace>) -> pide_core::collab::QueryHandler {
    let checking: Checking = Default::default();
    std::rc::Rc::new(move |payload: &Value, cx: &mut App| {
        let Some(workspace) = workspace.upgrade() else {
            return json!({"notes": ["The project window is closed."]});
        };
        match payload.get("kind").and_then(Value::as_str) {
            Some("diagnostics") => diagnostics(&workspace, &checking, payload, cx),
            other => json!({"notes": [format!("pide doesn't know the question {other:?}.")]}),
        }
    })
}

fn diagnostics(
    workspace: &Entity<Workspace>,
    checking: &Checking,
    payload: &Value,
    cx: &mut App,
) -> Value {
    let root = workspace.read(cx).root().to_path_buf();
    let store_entity = pide_lsp::LspStore::for_workspace(workspace, cx);
    let mut pending: Vec<String> = Vec::new();
    let mut to_check: Vec<PathBuf> = Vec::new();
    let now = std::time::Instant::now();
    let store = store_entity.read(cx);
    let ws = workspace.read(cx);
    let asked: Vec<PathBuf> = payload
        .get("paths")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|p| root.join(p))
                .collect()
        })
        .unwrap_or_default();
    let mut notes = Vec::new();
    let files: Vec<(PathBuf, &pide_lsp::store::FileDiagnostics)> = if asked.is_empty() {
        store
            .diagnostics()
            .iter()
            .map(|(p, d)| (p.clone(), d))
            .collect()
    } else {
        asked
            .iter()
            .filter_map(|p| {
                let open = ws.buffer_for(p, cx).is_some();
                // Not open in the editor but a server handles it: (re)check it from disk.
                let server = store.server_of(p).filter(|s| {
                    !matches!(
                        s.state,
                        pide_lsp::ServerState::Crashed { .. } | pide_lsp::ServerState::NotInstalled { .. }
                    )
                });
                if !open && let Some(server) = server {
                    // Re-read it for every question (pi may have changed it since).
                    to_check.push(p.clone());
                    // pi's tool asks again while files are pending: attempt 0 starts a
                    // round, and only reads that finished after its start count.
                    let first = payload.get("attempt").and_then(Value::as_u64).unwrap_or(0) == 0;
                    let c = {
                        let mut map = checking.borrow_mut();
                        let c = map.entry(p.clone()).or_insert(Check {
                            since: now,
                            hash: 0,
                            read_at: None,
                            round: now,
                            missing: false,
                        });
                        if first {
                            c.round = now;
                        }
                        *c
                    };
                    if c.missing && !first {
                        notes.push(format!("{}: can't read it.", rel(&root, p)));
                        return None;
                    }
                    let age = now.duration_since(c.since);
                    let diags = store.diagnostics_for(p);
                    let wait = first
                        || c.read_at.is_none_or(|t| t < c.round)
                        || !server.is_running()
                        || age < FRESH
                        || (diags.is_none() && age < SETTLE);
                    if wait {
                        pending.push(rel(&root, p));
                        return None;
                    }
                    if diags.is_none() {
                        return None; // checked a while ago, nothing reported: clean
                    }
                }
                match store.diagnostics_for(p) {
                Some(d) => Some((p.clone(), d)),
                // Open with a running server: it reports nothing, so it's clean
                // (servers drop a file's entry once its problems are gone).
                None if open && store.server_for(p).is_some() => None,
                None => {
                    notes.push(match store.server_of(p) {
                        // The server for it exists but isn't running: say so, it's fixable.
                        Some(server) if !server.is_running() => format!(
                            "{}: no diagnostics: {} isn't running ({}).",
                            rel(&root, p),
                            server.spec.id,
                            server.status_text()
                        ),
                        _ => format!(
                            "{}: no diagnostics known (the editor hasn't opened it, or no language server handles it).",
                            rel(&root, p)
                        ),
                    });
                    None
                }
                }
            })
            .collect()
    };
    let mut items = Vec::new();
    for (path, d) in files {
        for diag in &d.items {
            let severity = match diag.severity {
                Some(lsp_types::DiagnosticSeverity::ERROR) => "error",
                Some(lsp_types::DiagnosticSeverity::WARNING) => "warning",
                Some(lsp_types::DiagnosticSeverity::INFORMATION) => "info",
                Some(lsp_types::DiagnosticSeverity::HINT) => "hint",
                _ => "problem",
            };
            items.push(json!({
                "path": rel(&root, &path),
                "line": diag.range.start.line + 1,
                // In the server's position encoding (UTF-16 by default): close enough to point at.
                "column": diag.range.start.character + 1,
                "severity": severity,
                "message": diag.message,
                "source": diag.source,
            }));
        }
    }
    // Errors first, then by file and line.
    let rank = |v: &Value| match v["severity"].as_str() {
        Some("error") => 0,
        Some("warning") => 1,
        _ => 2,
    };
    items.sort_by(|a, b| {
        (rank(a), a["path"].as_str(), a["line"].as_u64()).cmp(&(rank(b), b["path"].as_str(), b["line"].as_u64()))
    });
    if items.len() > MAX_ITEMS {
        notes.push(format!("{} more not shown.", items.len() - MAX_ITEMS));
        items.truncate(MAX_ITEMS);
    }
    // Hand the files no editor has open to their server, from disk (off the UI thread).
    for path in to_check {
        let store = store_entity.downgrade();
        let checking = checking.clone();
        cx.spawn(async move |cx| {
            let read = cx
                .background_executor()
                .spawn({
                    let path = path.clone();
                    async move { std::fs::read_to_string(&path).ok() }
                })
                .await;
            let Some(text) = read else {
                if let Some(c) = checking.borrow_mut().get_mut(&path) {
                    c.missing = true;
                    c.read_at = Some(std::time::Instant::now());
                }
                return;
            };
            {
                let hash = pide_core::buffer::hash_bytes(text.as_bytes());
                let _ = store.update(cx, |s, cx| s.check_file(&path, text, cx));
                if let Some(c) = checking.borrow_mut().get_mut(&path) {
                    if c.hash != hash {
                        // New text: the server needs a moment before its answer is current.
                        c.hash = hash;
                        c.since = std::time::Instant::now();
                    }
                    c.read_at = Some(std::time::Instant::now());
                    c.missing = false;
                }
            }
        })
        .detach();
    }
    json!({"diagnostics": items, "notes": notes, "pending": pending})
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}
