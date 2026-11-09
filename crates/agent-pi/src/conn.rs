//! The JSONL connection: one task per pi process on the runtime thread.
//! Generic over the byte streams so tests can drive it without a process.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
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

/// Pump commands to pi and records back until either side closes.
/// Dropping `commands`' senders closes pi's stdin, which asks pi to shut down.
pub async fn run<R, W>(
    reader: R,
    mut writer: W,
    mut commands: mpsc::UnboundedReceiver<String>,
    inbound: mpsc::UnboundedSender<Inbound>,
    log: Arc<RawLog>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::with_capacity(8 * 1024);
    let mut commands_open = true;

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
                        for rec in parse_line(&line) {
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
