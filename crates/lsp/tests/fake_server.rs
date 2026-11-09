//! A real server process (tests/fixtures/fake_lsp.py) driven through
//! `process::spawn` and the JSON-RPC layer: initialize, notifications in,
//! request/response, and a crash with its exit code and stderr.

use std::path::{Path, PathBuf};
use std::time::Duration;

use pide_lsp::process;
use pide_lsp::registry::{self, ServerSpec};
use pide_lsp::rpc::Incoming;
use serde_json::{Value, json};

fn spec() -> Option<ServerSpec> {
    registry::find_command("python3")?;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_lsp.py");
    let mut s = registry::specs(None).remove(0);
    s.command = "python3".into();
    s.args = vec![script.display().to_string()];
    Some(s)
}

/// Logs go to a temporary state dir, not the user's.
fn isolate() {
    let dir = std::env::temp_dir().join(format!("pide-lsp-test-{}", std::process::id()));
    // SAFETY: set before any thread reads it; both tests set the same value.
    unsafe { std::env::set_var("XDG_STATE_HOME", &dir) };
    pide_runtime::init();
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    pide_runtime::init();
    pide_runtime::handle().block_on(async move {
        tokio::time::timeout(Duration::from_secs(10), f)
            .await
            .expect("timed out")
    })
}

#[test]
fn initialize_notifications_requests_and_crash() {
    isolate();
    let Some(spec) = spec() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    let root = std::env::temp_dir();
    let mut started = process::spawn(&spec, &root);
    let rpc = started.rpc.clone();

    // initialize: the server picks UTF-16.
    let init = block(rpc.request("initialize", process::initialize_params(&root)))
        .unwrap()
        .unwrap();
    assert_eq!(init["capabilities"]["positionEncoding"], "utf-16");
    rpc.notify("initialized", json!({}));

    // didOpen -> publishDiagnostics comes back as a notification.
    let file: PathBuf = root.join("a.rs");
    rpc.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": process::file_uri(&file), "languageId": "rust", "version": 1, "text": "x" } }),
    );
    let note = block(started.incoming.recv()).expect("a notification");
    match note {
        Incoming::Notification { method, params } => {
            assert_eq!(method, "textDocument/publishDiagnostics");
            assert_eq!(
                process::uri_path(params["uri"].as_str().unwrap()).as_deref(),
                Some(file.as_path())
            );
            assert_eq!(params["diagnostics"][0]["message"], "fake error");
        }
        other => panic!("{other:?}"),
    }

    // A request and its response.
    let hover = block(rpc.request("textDocument/hover", json!({})))
        .unwrap()
        .unwrap();
    assert_eq!(hover["contents"], Value::String("fake".into()));

    // Crash: the exit carries the code and the stderr tail; a request the
    // server never answers fails instead of hanging.
    rpc.notify("test/crash", Value::Null);
    let pending = rpc.request("textDocument/hover", json!({}));
    let exit = block(started.exit).expect("an exit");
    assert_eq!(exit.code, Some(3));
    assert!(
        exit.stderr_tail
            .iter()
            .any(|l| l.contains("crashing on purpose")),
        "{:?}",
        exit.stderr_tail
    );
    assert!(
        block(pending).map(|r| r.is_err()).unwrap_or(true),
        "unanswered request must fail"
    );
}

#[test]
fn a_missing_command_reports_why_it_did_not_start() {
    isolate();
    let mut s = registry::specs(None).remove(0);
    s.command = "/nonexistent/server-xyz".into();
    let started = process::spawn(&s, &std::env::temp_dir());
    let exit = block(started.exit).expect("an exit");
    assert!(exit.spawn_error.is_some());
}
