//! Answers to pide's pi extension (collaborator mode, docs/COLLAB.md): pi's
//! `ide_diagnostics` tool asks what the editor's language servers report.
//! Runs on the UI thread from in-memory state only.

use std::path::{Path, PathBuf};

use gpui_kit::*;
use pide_core::Workspace;
use serde_json::{Value, json};

/// Most problems sent back in one answer.
const MAX_ITEMS: usize = 200;

pub fn handler(workspace: WeakEntity<Workspace>) -> pide_core::collab::QueryHandler {
    std::rc::Rc::new(move |payload: &Value, cx: &mut App| {
        let Some(workspace) = workspace.upgrade() else {
            return json!({"notes": ["The project window is closed."]});
        };
        match payload.get("kind").and_then(Value::as_str) {
            Some("diagnostics") => diagnostics(&workspace, payload, cx),
            other => json!({"notes": [format!("pide doesn't know the question {other:?}.")]}),
        }
    })
}

fn diagnostics(workspace: &Entity<Workspace>, payload: &Value, cx: &mut App) -> Value {
    let root = workspace.read(cx).root().to_path_buf();
    let store = pide_lsp::LspStore::for_workspace(workspace, cx);
    let store = store.read(cx);
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
            .filter_map(|p| match store.diagnostics_for(p) {
                Some(d) => Some((p.clone(), d)),
                // Open with a running server: it reports nothing, so it's clean
                // (servers drop a file's entry once its problems are gone).
                None if ws.buffer_for(p, cx).is_some() && store.server_for(p).is_some() => None,
                None => {
                    notes.push(format!(
                        "{}: no diagnostics known (the editor hasn't opened it, or no language server handles it).",
                        rel(&root, p)
                    ));
                    None
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
    json!({"diagnostics": items, "notes": notes})
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}
