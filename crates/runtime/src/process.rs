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
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
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
        .flat_map(|d| candidates(&d, command))
        .find(|c| c.is_file())
}

/// On Windows a bare name is `name.exe`, `name.cmd` or `name.bat`; npm's extensionless
/// `name` next to them is a shell script Windows can't start.
fn candidates(dir: &Path, command: &str) -> Vec<PathBuf> {
    if cfg!(windows) && Path::new(command).extension().is_none() {
        ["exe", "cmd", "bat"]
            .map(|ext| dir.join(command).with_extension(ext))
            .to_vec()
    } else {
        vec![dir.join(command)]
    }
}

/// A command for `program`. On Windows an npm `.cmd` shim runs as `node <script>`:
/// batch files reject arguments with newlines (prompts given to `pi -p` have them), and
/// killing the shim's `cmd.exe` would leave its node process running.
pub fn command_for(program: &Path) -> std::process::Command {
    match npm_shim_target(program) {
        Some((node, script)) => {
            let mut c = std::process::Command::new(node);
            c.arg(script);
            c
        }
        None => std::process::Command::new(program),
    }
}

/// `(node, script)` that an npm cmd-shim runs: its last `"%dp0%\….js"`.
fn npm_shim_target(program: &Path) -> Option<(PathBuf, PathBuf)> {
    if !cfg!(windows)
        || !program
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cmd"))
    {
        return None;
    }
    let shim = std::fs::read_to_string(program).ok()?;
    let script = shim
        .split("\"%dp0%\\")
        .skip(1)
        .filter_map(|s| s.split('"').next())
        .filter(|s| s.ends_with(".js") || s.ends_with(".cjs") || s.ends_with(".mjs"))
        .last()?;
    let dir = program.parent()?;
    let node = Some(dir.join("node.exe"))
        .filter(|p| p.is_file())
        .or_else(|| find_command("node"))?;
    Some((node, dir.join(script)))
}

/// Stop `pid` and everything it started.
fn kill_tree(pid: u32) {
    let _ = if cfg!(windows) {
        std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output()
    } else {
        // Its own process group (see `run_streaming`), so this reaches what a shell started.
        std::process::Command::new("/bin/kill")
            .args(["-TERM", "--", &format!("-{pid}")])
            .output()
    };
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
        let mut cmd = tokio::process::Command::from(command_for(&program));
        if let Some(cwd) = &cwd {
            cmd.current_dir(cwd);
        }
        // Its own process group, so Stop reaches what a shell started too.
        #[cfg(unix)]
        cmd.process_group(0);
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
        let status = tokio::select! {
            s = child.wait() => s,
            r = &mut stop_rx => {
                if r.is_ok()
                    && let Some(pid) = child.id()
                {
                    kill_tree(pid);
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
