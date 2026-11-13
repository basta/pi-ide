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

/// Files checked without an editor, and since when.
type Checking = std::rc::Rc<std::cell::RefCell<std::collections::HashMap<PathBuf, std::time::Instant>>>;

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
                if !open && store.server_of(p).is_some_and(|s| !matches!(s.state, pide_lsp::ServerState::Crashed { .. } | pide_lsp::ServerState::NotInstalled { .. })) && p.is_file() {
                    to_check.push(p.clone());
                    let since = *checking.borrow_mut().entry(p.clone()).or_insert(now);
                    if store.diagnostics_for(p).is_none() && now.duration_since(since) < SETTLE {
                        pending.push(rel(&root, p));
                        return None;
                    }
                    if store.diagnostics_for(p).is_none() {
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
        cx.spawn(async move |cx| {
            let read = cx
                .background_executor()
                .spawn({
                    let path = path.clone();
                    async move { std::fs::read_to_string(&path).ok() }
                })
                .await;
            if let Some(text) = read {
                let _ = store.update(cx, |s, cx| s.check_file(&path, text, cx));
            }
        })
        .detach();
    }
    json!({"diagnostics": items, "notes": notes, "pending": pending})
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}
