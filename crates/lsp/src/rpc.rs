//! JSON-RPC 2.0 over a language server's stdio (`Content-Length` framing).
//! Runs on the tokio runtime; callers on the UI side await `oneshot` receivers,
//! which work on any executor.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// An error the server returned, or the connection closing first.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}
impl std::error::Error for RpcError {}

/// LSP `ContentModified` / `RequestCancelled`: the answer is stale, not a failure.
pub fn is_stale(e: &RpcError) -> bool {
    matches!(e.code, -32801 | -32800)
}

/// What the server sends us, besides responses.
#[derive(Debug)]
pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// Sending side. Cheap to clone; usable from any thread.
#[derive(Clone)]
pub struct Rpc {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: Arc<AtomicI64>,
}

impl Rpc {
    pub fn request(
        &self,
        method: &str,
        params: Value,
    ) -> oneshot::Receiver<Result<Value, RpcError>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if self.out.send(msg.to_string()).is_err() {
            // Closed: the receiver resolves to an error once `tx` is dropped.
            self.pending.lock().unwrap().remove(&id);
        }
        rx
    }

    pub fn notify(&self, method: &str, params: Value) {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.out.send(msg.to_string());
    }

    pub fn respond(&self, id: Value, result: Value) {
        let msg = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let _ = self.out.send(msg.to_string());
    }
}

/// Every raw message in both directions, one JSON line each (like pi's log).
pub struct RawLog {
    file: Option<Mutex<std::fs::File>>,
    server: String,
}

impl RawLog {
    pub fn open(path: PathBuf, server: &str) -> Self {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Keep one previous log once it passes 50 MB.
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 50 * 1024 * 1024) {
            let _ = std::fs::rename(&path, path.with_extension("jsonl.old"));
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .map(Mutex::new);
        Self {
            file,
            server: server.to_string(),
        }
    }

    pub fn disabled() -> Self {
        Self {
            file: None,
            server: String::new(),
        }
    }

    fn write(&self, dir: &str, body: &str) {
        let Some(f) = &self.file else { return };
        let body = clip(body);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let rec = json!({ "ts": ts, "server": self.server, "dir": dir, "body": body });
        if let Ok(mut f) = f.lock() {
            let _ = writeln!(f, "{rec}");
        }
    }
}

/// Largest message body logged whole. Full-text syncs of big files are megabytes
/// each; logging them all would fill the disk while typing.
const LOG_BODY_MAX: usize = 64 * 1024;

fn clip(body: &str) -> std::borrow::Cow<'_, str> {
    if body.len() <= LOG_BODY_MAX {
        return std::borrow::Cow::Borrowed(body);
    }
    let mut cut = 2048;
    while !body.is_char_boundary(cut) {
        cut -= 1;
    }
    std::borrow::Cow::Owned(format!(
        "{}… [{} bytes, clipped in this log]",
        &body[..cut],
        body.len()
    ))
}

/// A new connection: the `Rpc` handle and the outgoing queue the pump drains.
pub fn channel() -> (Rpc, mpsc::UnboundedReceiver<String>, Pending) {
    let (out, out_rx) = mpsc::unbounded_channel();
    let pending: Pending = Arc::default();
    (
        Rpc {
            out,
            pending: pending.clone(),
            next_id: Arc::new(AtomicI64::new(1)),
        },
        out_rx,
        pending,
    )
}

/// Read one framed message. `Ok(None)` at a clean end of stream.
pub async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> std::io::Result<Option<String>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            continue; // stray blank line between messages
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().ok();
            }
        }
    }
    let mut body = vec![0; length.unwrap_or(0)];
    reader.read_exact(&mut body).await?;
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

pub fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

/// Pump messages until either side closes. Pending requests fail when it returns.
pub async fn run<R, W>(
    reader: R,
    mut writer: W,
    mut out_rx: mpsc::UnboundedReceiver<String>,
    pending: Pending,
    incoming: mpsc::UnboundedSender<Incoming>,
    log: Arc<RawLog>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    loop {
        tokio::select! {
            out = out_rx.recv() => {
                let Some(body) = out else { break };
                log.write("out", &body);
                if writer.write_all(frame(&body).as_bytes()).await.is_err() || writer.flush().await.is_err() {
                    break;
                }
            }
            msg = read_message(&mut reader) => {
                let body = match msg {
                    Ok(Some(b)) => b,
                    Ok(None) => break,
                    Err(e) => {
                        log::warn!("language server stdout: {e}");
                        break;
                    }
                };
                log.write("in", &body);
                let Ok(v) = serde_json::from_str::<Value>(&body) else {
                    log::warn!("language server sent invalid JSON");
                    continue;
                };
                dispatch(v, &pending, &incoming);
            }
        }
    }
    for (_, tx) in pending.lock().unwrap().drain() {
        let _ = tx.send(Err(RpcError {
            code: -32099,
            message: "language server stopped".into(),
        }));
    }
}

fn dispatch(v: Value, pending: &Pending, incoming: &mpsc::UnboundedSender<Incoming>) {
    let method = v.get("method").and_then(Value::as_str).map(str::to_string);
    let id = v.get("id").cloned().filter(|id| !id.is_null());
    match (method, id) {
        (Some(method), Some(id)) => {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            let _ = incoming.send(Incoming::Request { id, method, params });
        }
        (Some(method), None) => {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            let _ = incoming.send(Incoming::Notification { method, params });
        }
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(tx) = pending.lock().unwrap().remove(&id) else {
                return;
            };
            let result = match v.get("error") {
                Some(e) => Err(RpcError {
                    code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: e
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                        .to_string(),
                }),
                None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(result);
        }
        (None, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huge_bodies_are_clipped_in_the_log_only() {
        let small = "x".repeat(100);
        assert_eq!(clip(&small), small);
        let big = "č".repeat(LOG_BODY_MAX); // 2 bytes each
        let c = clip(&big);
        assert!(
            c.len() < 3000 && c.ends_with(&format!("[{} bytes, clipped in this log]", big.len()))
        );
    }

    #[tokio::test]
    async fn reads_framed_messages_including_multibyte_bodies() {
        let a = r#"{"jsonrpc":"2.0","method":"x","params":{"s":"čšž"}}"#;
        let b = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let stream = format!("{}{}", frame(a), frame(b));
        let mut reader = BufReader::new(stream.as_bytes());
        assert_eq!(read_message(&mut reader).await.unwrap().as_deref(), Some(a));
        assert_eq!(read_message(&mut reader).await.unwrap().as_deref(), Some(b));
        assert_eq!(read_message(&mut reader).await.unwrap(), None);
    }

    /// A request goes out framed, its response resolves the caller; server
    /// notifications and requests come through `incoming`.
    #[tokio::test]
    async fn request_response_and_incoming() {
        let (client_side, mut server_side) = tokio::io::duplex(64 * 1024);
        let (client_r, client_w) = tokio::io::split(client_side);
        let (rpc, out_rx, pending) = channel();
        let (in_tx, mut in_rx) = mpsc::unbounded_channel();
        tokio::spawn(run(
            client_r,
            client_w,
            out_rx,
            pending,
            in_tx,
            Arc::new(RawLog::disabled()),
        ));

        let answer = rpc.request("initialize", json!({}));
        let (server_r, mut server_w) = {
            let (r, w) = tokio::io::split(&mut server_side);
            (BufReader::new(r), w)
        };
        let mut server_r = server_r;
        let sent: Value =
            serde_json::from_str(&read_message(&mut server_r).await.unwrap().unwrap()).unwrap();
        assert_eq!(sent["method"], "initialize");
        let id = sent["id"].clone();
        let reply = json!({"jsonrpc":"2.0","id":id,"result":{"capabilities":{}}}).to_string();
        let note = json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///a.rs","diagnostics":[]}}).to_string();
        let req = json!({"jsonrpc":"2.0","id":"r1","method":"workspace/configuration","params":{"items":[]}}).to_string();
        server_w
            .write_all(format!("{}{}{}", frame(&reply), frame(&note), frame(&req)).as_bytes())
            .await
            .unwrap();

        assert_eq!(answer.await.unwrap().unwrap()["capabilities"], json!({}));
        match in_rx.recv().await.unwrap() {
            Incoming::Notification { method, .. } => {
                assert_eq!(method, "textDocument/publishDiagnostics")
            }
            other => panic!("{other:?}"),
        }
        match in_rx.recv().await.unwrap() {
            Incoming::Request { id, method, .. } => {
                assert_eq!(id, json!("r1"));
                assert_eq!(method, "workspace/configuration");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn pending_requests_fail_when_the_server_goes_away() {
        let (client_side, server_side) = tokio::io::duplex(1024);
        let (client_r, client_w) = tokio::io::split(client_side);
        let (rpc, out_rx, pending) = channel();
        let (in_tx, _in_rx) = mpsc::unbounded_channel();
        let pump = tokio::spawn(run(
            client_r,
            client_w,
            out_rx,
            pending,
            in_tx,
            Arc::new(RawLog::disabled()),
        ));
        let answer = rpc.request("textDocument/hover", json!({}));
        drop(server_side);
        pump.await.unwrap();
        assert!(answer.await.unwrap().is_err());
    }
}
