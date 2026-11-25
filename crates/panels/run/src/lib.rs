//! Run panel (bottom dock): runs a shell command in the project root through
//! the login shell (so PATH is right), streams its output, Stop and Rerun.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{DockPosition, Panel, Services};

pub const PANEL_NAME: &str = "run";
/// Keep at most this many output lines (the oldest go first).
const MAX_LINES: usize = 20_000;

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| RunPanel::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "Run",
            icon: gpui_kit::component::IconName::SquareTerminal,
            dock: pide_core::DockPosition::Bottom,
            command: "view.run",
            key: Some("cmd-4"),
            create: |window, cx| panel_handle(cx.new(|cx| RunPanel::new(window, cx))),
        },
    );
}

#[derive(Default)]
struct Instances(HashMap<WindowId, WeakEntity<RunPanel>>);
impl Global for Instances {}

/// This window's Run panel, if one exists.
pub fn existing(window: &Window, cx: &App) -> Option<Entity<RunPanel>> {
    cx.try_global::<Instances>()?
        .0
        .get(&window.window_handle().window_id())?
        .upgrade()
}

/// Remove terminal escape sequences (colors, cursor moves) from a line.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: parameters, then one final byte in @..~
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: up to BEL or ESC \
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
        } else if c == '\r' {
            // Progress bars redraw with \r: keep what comes after the last one.
            out.clear();
        } else {
            out.push(c);
        }
    }
    out
}

/// A `path:line[:col]` reference in an output line (compiler errors, test
/// failures): byte range in the line, path, 1-based line and column.
pub fn find_location(line: &str) -> Option<(std::ops::Range<usize>, String, u32, u32)> {
    let is_sep = |c: char| c.is_whitespace() || "()[]{}<>'\",;`".contains(c);
    let mut start = 0;
    for token in line.split(is_sep) {
        let begin = start;
        start += token.len() + 1;
        let t = token.trim_end_matches(['.', ':']);
        let mut parts = t.splitn(3, ':');
        let (Some(path), Some(l)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(l) = l.parse::<u32>() else { continue };
        let col = parts
            .next()
            .and_then(|c| c.parse::<u32>().ok())
            .unwrap_or(1);
        // Looks like a file: has an extension, not a URL or a time.
        let name = path.rsplit('/').next().unwrap_or(path);
        if path.contains("://") || !name.contains('.') {
            continue;
        }
        if name
            .rsplit('.')
            .next()
            .is_none_or(|ext| ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()))
        {
            continue;
        }
        return Some((
            begin..begin + t.len(),
            path.to_string(),
            l.max(1),
            col.max(1),
        ));
    }
    None
}

pub struct RunPanel {
    root: PathBuf,
    command: String,
    lines: Vec<SharedString>,
    running: Option<pide_runtime::tokio::sync::oneshot::Sender<()>>,
    status: Option<String>,
    started: Option<Instant>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _task: Option<Task<()>>,
}

impl EventEmitter<PanelEvent> for RunPanel {}

impl RunPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = Services::of(window, cx)
            .workspace
            .read(cx)
            .root()
            .to_path_buf();
        let me = cx.entity().downgrade();
        cx.default_global::<Instances>()
            .0
            .insert(window.window_handle().window_id(), me);
        Self {
            root,
            command: String::new(),
            lines: Vec::new(),
            running: None,
            status: None,
            started: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _task: None,
        }
    }

    pub fn last_command(&self) -> &str {
        &self.command
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Run `command` (stopping the current one first).
    pub fn start(&mut self, command: String, cx: &mut Context<Self>) {
        self.stop(cx);
        self.command = command.clone();
        self.lines.clear();
        self.status = None;
        self.started = Some(Instant::now());
        let argv = if cfg!(windows) {
            let shell = if pide_runtime::process::find_command("pwsh").is_some() {
                "pwsh"
            } else {
                "powershell"
            };
            vec![shell.into(), "-NoLogo".into(), "-Command".into(), command]
        } else {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            // A login shell, so PATH matches the terminal's.
            vec![shell, "-lc".into(), command]
        };
        let run = pide_runtime::process::run_streaming(argv, Some(self.root.clone()));
        let pide_runtime::process::Streaming {
            mut lines,
            done,
            stop,
        } = run;
        self.running = Some(stop);
        self._task = Some(cx.spawn(async move |this, cx| {
            // Lines arrive in bursts: apply them in batches, one redraw each.
            while let Some(first) = lines.recv().await {
                let mut batch = vec![first];
                while let Ok(l) = lines.try_recv() {
                    batch.push(l);
                }
                let alive = this.update(cx, |this, cx| {
                    this.lines
                        .extend(batch.iter().map(|l| SharedString::from(strip_ansi(l))));
                    if this.lines.len() > MAX_LINES {
                        let extra = this.lines.len() - MAX_LINES;
                        this.lines.drain(..extra);
                    }
                    this.scroll
                        .scroll_to_item(this.lines.len().saturating_sub(1), ScrollStrategy::Bottom);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            let code = done.await;
            let _ = this.update(cx, |this, cx| {
                let secs = this
                    .started
                    .map(|t| t.elapsed().as_secs_f32())
                    .unwrap_or(0.0);
                this.status = Some(match code {
                    Ok(Ok(Some(0))) => format!("finished in {secs:.1}s"),
                    Ok(Ok(Some(c))) => format!("✗ failed (exit code {c}) after {secs:.1}s"),
                    Ok(Ok(None)) => format!("stopped after {secs:.1}s"),
                    Ok(Err(e)) => format!("couldn't start: {e}"),
                    Err(_) => "ended".into(),
                });
                this.running = None;
                cx.notify();
            });
        }));
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    /// The command, how it ended and its last lines, for pi.
    fn draft_for_agent(&self) -> String {
        let tail: Vec<&str> = self
            .lines
            .iter()
            .rev()
            .take(80)
            .map(|l| l.as_ref())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!(
            "I ran `{}` in the project folder ({}). Output{}:\n```\n{}\n```\n",
            self.command,
            self.status.clone().unwrap_or_default(),
            if self.lines.len() > 80 {
                " (last 80 lines)"
            } else {
                ""
            },
            tail.join("\n")
        )
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(stop) = self.running.take() {
            let _ = stop.send(());
            cx.notify();
        }
    }
}

/// Closing the panel (or the window) stops what it runs.
impl Drop for RunPanel {
    fn drop(&mut self) {
        if let Some(stop) = self.running.take() {
            let _ = stop.send(());
        }
    }
}

impl Focusable for RunPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for RunPanel {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for RunPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for RunPanel {
    fn title_text(&self, _: &App) -> SharedString {
        "Run".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Bottom
    }
}

impl Render for RunPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let running = self.running.is_some();
        let has_command = !self.command.is_empty();
        let lines = Rc::new(self.lines.clone());
        let mono = theme.mono_font_family.clone();
        let root = self.root.clone();
        let link = pide_core::colors::accent(cx);
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(mono.clone())
                            .child(if has_command {
                                format!("$ {}", self.command)
                            } else {
                                format!("Run: Run Command… ({}) runs a shell command here.", pide_core::keymap::label("ctrl-alt-r"))
                            }),
                    )
                    .when_some(self.status.clone(), |this, s| {
                        // A failure shows at a glance; success stays quiet.
                        let failed = s.starts_with('✗') || s.starts_with("couldn't");
                        this.child(
                            div()
                                .text_color(if failed { theme.danger } else { theme.muted_foreground })
                                .child(s),
                        )
                    })
                    .when(running, |this| {
                        this.child(div().text_color(theme.muted_foreground).child("running…"))
                            .child(
                                Button::new("run-stop")
                                    .xsmall()
                                    .danger()
                                    .label("Stop")
                                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                            )
                    })
                    .when(!running && has_command && !self.lines.is_empty(), |this| {
                        this.child(
                            Button::new("run-ask")
                                .xsmall()
                                .label("Ask pi")
                                .tooltip("Put the command and its output in pi's message box")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let draft = this.draft_for_agent();
                                    pide_core::opener::ask_agent(draft, window, cx);
                                })),
                        )
                    })
                    .when(!running && has_command, |this| {
                        this.child(Button::new("run-again").xsmall().label("Rerun").on_click(
                            cx.listener(|this, _, _, cx| {
                                let c = this.command.clone();
                                this.start(c, cx);
                            }),
                        ))
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("run-output", lines.len(), move |range, _, _| {
                            range
                                .map(|i| {
                                    let text = lines[i].clone();
                                    let row = h_flex()
                                        .px_3()
                                        .whitespace_nowrap()
                                        .font_family(mono.clone())
                                        .text_xs();
                                    // `src/main.rs:10:5` opens the file there.
                                    match find_location(&text) {
                                        Some((r, path, line, col)) => {
                                            let full = if std::path::Path::new(&path).is_absolute()
                                            {
                                                PathBuf::from(&path)
                                            } else {
                                                root.join(&path)
                                            };
                                            row.child(text[..r.start].to_string())
                                                .child(
                                                    div()
                                                        .id(("run-loc", i))
                                                        .text_color(link)
                                                        .underline()
                                                        .cursor_pointer()
                                                        .on_click(move |_, window, cx| {
                                                            if full.is_file() {
                                                                pide_core::opener::open_file_at(
                                                                    full.clone(),
                                                                    line - 1,
                                                                    col - 1,
                                                                    window,
                                                                    cx,
                                                                );
                                                            }
                                                        })
                                                        .child(text[r.clone()].to_string()),
                                                )
                                                .child(text[r.end..].to_string())
                                                .into_any_element()
                                        }
                                        None => row.child(text).into_any_element(),
                                    }
                                })
                                .collect()
                        })
                        .size_full()
                        .track_scroll(&self.scroll),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
    }
}

/// Ask for a command (`default` prefilled); `on_ok` runs it.
pub fn prompt(
    default: String,
    on_ok: Rc<dyn Fn(String, &mut Window, &mut App)>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("cargo test")
            .default_value(default)
    });
    let focus_once = Rc::new(Cell::new(true));
    let run = Rc::new({
        let input = input.clone();
        move |window: &mut Window, cx: &mut App| {
            let c = input.read(cx).value().trim().to_string();
            if !c.is_empty() {
                on_ok(c, window, cx);
            }
        }
    });
    let enter = run.clone();
    let sub = window.subscribe(&input, cx, move |_, ev: &InputEvent, window, cx| {
        if let InputEvent::PressEnter { .. } = ev {
            window.close_dialog(cx);
            enter(window, cx);
        }
    });
    let sub = Rc::new(RefCell::new(Some(sub)));
    window.open_dialog(cx, move |dialog, window, cx| {
        let (input, run, sub) = (input.clone(), run.clone(), sub.clone());
        if focus_once.replace(false) {
            let input = input.clone();
            window.defer(cx, move |window, cx| {
                input.update(cx, |i, cx| {
                    i.focus(window, cx);
                    i.select_all(window, cx);
                })
            });
        }
        dialog
            .text_sm()
            .title("Run a command in the project folder")
            .w(px(520.))
            .child(Input::new(&input))
            .footer(
                DialogFooter::new()
                    .child(
                        DialogClose::new()
                            .child(Button::new("run-cancel").label("Cancel").outline()),
                    )
                    .child(DialogAction::new().child(Button::new("run-ok").label("Run").primary())),
            )
            .on_ok(move |_, window, cx| {
                run(window, cx);
                true
            })
            .on_close(move |_, _, _| {
                sub.borrow_mut().take();
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn finds_file_locations() {
        let l = "  --> src/main.rs:10:5";
        let (r, p, line, col) = find_location(l).unwrap();
        assert_eq!(
            (&l[r], p.as_str(), line, col),
            ("src/main.rs:10:5", "src/main.rs", 10, 5)
        );
        assert_eq!(
            find_location("error at (app.py:3)").map(|x| (x.1, x.2)),
            Some(("app.py".into(), 3))
        );
        assert_eq!(
            find_location("thread 'main' panicked at src/lib.rs:7:9:").map(|x| x.2),
            Some(7)
        );
        assert!(find_location("see https://x.dev:443/a").is_none());
        assert!(find_location("at 12:30:01 done").is_none());
        assert!(find_location("plain text").is_none());
    }

    #[::core::prelude::v1::test]
    fn strips_colors_and_progress_redraws() {
        assert_eq!(
            strip_ansi("\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m pide"),
            "   Compiling pide"
        );
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("10%\r50%\r100% done"), "100% done");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}after"), "after");
    }
}
