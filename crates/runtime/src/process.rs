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
    // TeX Live / MacTeX / BasicTeX put their programs here.
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/Library/TeX/texbin"] {
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
    /// Send `()` to stop the command (and whatever it started). Dropping it does nothing.
    pub stop: oneshot::Sender<()>,
}

/// Start `command` (program + args, no shell) on the runtime, in `cwd` if given.
pub fn run_streaming(command: Vec<String>, cwd: Option<std::path::PathBuf>) -> Streaming {
    let (line_tx, lines) = mpsc::unbounded_channel();
    let (done_tx, done) = oneshot::channel();
    let (stop, mut stop_rx) = oneshot::channel::<()>();
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
            // Its own process group, so Stop reaches what a shell started too.
            .process_group(0)
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
        let status = tokio::select! {
            s = child.wait() => s,
            r = &mut stop_rx => {
                if r.is_ok()
                    && let Some(pid) = child.id()
                {
                    let _ = std::process::Command::new("/bin/kill")
                        .args(["-TERM", "--", &format!("-{pid}")])
                        .status();
                }
                child.wait().await
            }
        };
        let _ = out.await;
        let _ = err.await;
        let _ = done_tx.send(status.map(|s| s.code()).map_err(|e| e.to_string()));
    });
    Streaming { lines, done, stop }
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
        let code = crate::handle().block_on(async { (&mut install.done).await.unwrap() });
        assert_eq!(code, Ok(Some(3)));
        let mut lines = Vec::new();
        while let Ok(l) = install.lines.try_recv() {
            lines.push(l);
        }
        lines.sort();
        assert_eq!(lines, ["one", "two"]);
    }

    #[test]
    fn stop_ends_the_command_and_its_children() {
        crate::init();
        let run = run_streaming(
            vec![
                "sh".into(),
                "-c".into(),
                "sleep 30 & sleep 30; echo late".into(),
            ],
            None,
        );
        let Streaming {
            mut lines,
            done,
            stop,
        } = run;
        let t = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = stop.send(());
        let code = crate::handle().block_on(async { done.await.unwrap() });
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "stopped quickly"
        );
        assert_ne!(code, Ok(Some(0)));
        assert!(lines.try_recv().is_err(), "no output after stop");
    }
}

/// Show a desktop notification (fire and forget). macOS: `osascript`; Linux:
/// `notify-send`. Title and body go in as arguments, never into the script.
pub fn desktop_notification(title: String, body: String) {
    crate::spawn(async move {
        let mut cmd = if cfg!(target_os = "macos") {
            let mut c = tokio::process::Command::new("/usr/bin/osascript");
            c.args([
                "-e",
                "on run argv",
                "-e",
                "display notification (item 2 of argv) with title (item 1 of argv)",
                "-e",
                "end run",
            ]);
            c.arg(&title).arg(&body);
            c
        } else {
            let Some(p) = find_command("notify-send") else {
                return;
            };
            let mut c = tokio::process::Command::new(p);
            c.arg(&title).arg(&body);
            c
        };
        let _ = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    });
}
