//! One shell in a PTY: alacritty's `Term` (parser, grid, scrollback) and its
//! event loop on its own thread. Events come back over a channel.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use pide_runtime::tokio::sync::mpsc;

pub type Shared = Arc<FairMutex<Term<Listener>>>;

#[derive(Clone)]
pub struct Listener(mpsc::UnboundedSender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct Session {
    pub term: Shared,
    sender: EventLoopSender,
    pub size: GridSize,
}

pub struct ShellCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// pwsh (else Windows PowerShell) on Windows; the login shell elsewhere.
pub fn default_shell() -> ShellCommand {
    if cfg!(windows) {
        let program = if pide_runtime::process::find_command("pwsh").is_some() {
            "pwsh"
        } else {
            "powershell"
        };
        ShellCommand {
            program: program.into(),
            args: vec!["-NoLogo".into()],
        }
    } else {
        ShellCommand {
            program: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
            args: vec!["-l".into()],
        }
    }
}

impl Session {
    pub fn spawn(
        shell: ShellCommand,
        cwd: &Path,
        size: GridSize,
        cell: (u16, u16),
    ) -> std::io::Result<(Self, mpsc::UnboundedReceiver<Event>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let listener = Listener(tx);
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            listener.clone(),
        )));
        // Set per child, not with alacritty's `setup_env`, which changes pide's own
        // environment (and so every other process pide starts).
        let env = HashMap::from([
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
            ("TERM_PROGRAM".to_string(), "pide".to_string()),
        ]);
        let options = tty::Options {
            shell: Some(tty::Shell::new(shell.program, shell.args)),
            working_directory: Some(cwd.to_path_buf()),
            drain_on_exit: true,
            env,
            #[cfg(windows)]
            escape_args: true,
        };
        let pty = tty::new(&options, window_size(size, cell), 0)?;
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        event_loop.spawn();
        Ok((Self { term, sender, size }, rx))
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            let _ = self.sender.send(Msg::Input(bytes));
        }
    }

    pub fn resize(&mut self, size: GridSize, cell: (u16, u16)) {
        if size == self.size || size.cols == 0 || size.rows == 0 {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(window_size(size, cell)));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

fn window_size(size: GridSize, (cell_width, cell_height): (u16, u16)) -> WindowSize {
    WindowSize {
        num_lines: size.rows as u16,
        num_cols: size.cols as u16,
        cell_width,
        cell_height,
    }
}
