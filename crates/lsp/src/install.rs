//! Running a server's install command (only ever on the user's click).

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::registry::{find_command, search_path};

/// A running install: output lines (stdout and stderr interleaved) and the exit.
pub struct Install {
    pub lines: mpsc::UnboundedReceiver<String>,
    /// `Ok(code)`, or why it couldn't start.
    pub done: oneshot::Receiver<Result<Option<i32>, String>>,
}

/// Start `command` (program + args, no shell) on the runtime.
pub fn run(command: Vec<String>) -> Install {
    let (line_tx, lines) = mpsc::unbounded_channel();
    let (done_tx, done) = oneshot::channel();
    pide_runtime::spawn(async move {
        let Some((program, args)) = command.split_first() else {
            let _ = done_tx.send(Err("empty install command".into()));
            return;
        };
        let program = find_command(program).unwrap_or_else(|| program.into());
        let mut child = match tokio::process::Command::new(&program)
            .args(args)
            .env("PATH", std::env::join_paths(search_path()).unwrap_or_default())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = done_tx.send(Err(format!("{}: {e}", program.display())));
                return;
            }
        };
        let pump = |r: Box<dyn tokio::io::AsyncRead + Unpin + Send>, tx: mpsc::UnboundedSender<String>| {
            tokio::spawn(async move {
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let _ = tx.send(l);
                }
            })
        };
        let out = pump(Box::new(child.stdout.take().expect("piped")), line_tx.clone());
        let err = pump(Box::new(child.stderr.take().expect("piped")), line_tx);
        let status = child.wait().await;
        let _ = out.await;
        let _ = err.await;
        let _ = done_tx.send(status.map(|s| s.code()).map_err(|e| e.to_string()));
    });
    Install { lines, done }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_output_and_reports_the_exit_code() {
        pide_runtime::init();
        let mut install = run(vec!["sh".into(), "-c".into(), "echo one; echo two >&2; exit 3".into()]);
        let code = pide_runtime::handle().block_on(async { install.done.await.unwrap() });
        assert_eq!(code, Ok(Some(3)));
        let mut lines = Vec::new();
        while let Ok(l) = install.lines.try_recv() {
            lines.push(l);
        }
        lines.sort();
        assert_eq!(lines, ["one", "two"]);
    }
}
