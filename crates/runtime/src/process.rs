//! Finding programs and running them with streamed output (language server
//! installs, `pi install`). The UI never waits on these: output arrives over channels.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// Directories searched for commands: PATH, plus where rustup and npm put things
/// (an app started from Finder doesn't get the shell's PATH).
pub fn search_path() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for extra in [".cargo/bin", ".local/bin", ".npm-global/bin"] {
            dirs.push(home.join(extra));
        }
    }
    for extra in ["/opt/homebrew/bin", "/usr/local/bin"] {
        dirs.push(PathBuf::from(extra));
    }
    dirs.dedup();
    dirs
}

/// `command` as an absolute path, if it exists (a path, or a name on the search path).
pub fn find_command(command: &str) -> Option<PathBuf> {
    let p = Path::new(command);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    search_path()
        .into_iter()
        .map(|d| d.join(command))
        .find(|c| c.is_file())
}

/// A running command: output lines (stdout and stderr interleaved) and the exit.
pub struct Streaming {
    pub lines: mpsc::UnboundedReceiver<String>,
    /// `Ok(code)`, or why it couldn't start.
    pub done: oneshot::Receiver<Result<Option<i32>, String>>,
}

/// Start `command` (program + args, no shell) on the runtime, in `cwd` if given.
pub fn run_streaming(command: Vec<String>, cwd: Option<std::path::PathBuf>) -> Streaming {
    let (line_tx, lines) = mpsc::unbounded_channel();
    let (done_tx, done) = oneshot::channel();
    crate::spawn(async move {
        let Some((program, args)) = command.split_first() else {
            let _ = done_tx.send(Err("empty install command".into()));
            return;
        };
        let program = find_command(program).unwrap_or_else(|| program.into());
        let mut cmd = tokio::process::Command::new(&program);
        if let Some(cwd) = &cwd {
            cmd.current_dir(cwd);
        }
        let mut child = match cmd
            .args(args)
            .env(
                "PATH",
                std::env::join_paths(search_path()).unwrap_or_default(),
            )
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
        let pump = |r: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                    tx: mpsc::UnboundedSender<String>| {
            tokio::spawn(async move {
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let _ = tx.send(l);
                }
            })
        };
        let out = pump(
            Box::new(child.stdout.take().expect("piped")),
            line_tx.clone(),
        );
        let err = pump(Box::new(child.stderr.take().expect("piped")), line_tx);
        let status = child.wait().await;
        let _ = out.await;
        let _ = err.await;
        let _ = done_tx.send(status.map(|s| s.code()).map_err(|e| e.to_string()));
    });
    Streaming { lines, done }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_output_and_reports_the_exit_code() {
        crate::init();
        let mut install = run_streaming(
            vec![
                "sh".into(),
                "-c".into(),
                "echo one; echo two >&2; exit 3".into(),
            ],
            None,
        );
        let code = crate::handle().block_on(async { install.done.await.unwrap() });
        assert_eq!(code, Ok(Some(3)));
        let mut lines = Vec::new();
        while let Ok(l) = install.lines.try_recv() {
            lines.push(l);
        }
        lines.sort();
        assert_eq!(lines, ["one", "two"]);
    }
}
