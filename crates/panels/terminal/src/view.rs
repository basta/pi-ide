//! One terminal tab: a [`Session`], its keyboard/mouse/text input, and the
//! events alacritty sends back (title, clipboard, replies to queries).

use std::ops::Range;
use std::path::Path;

use alacritty_terminal::event::{Event, WindowSize};
use alacritty_terminal::grid::{Dimensions as _, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::*;
use pide_core::CommandRegistry;
use pide_runtime::tokio::sync::mpsc;

use crate::element::{self, TerminalElement};
use crate::keys;
use crate::palette::Palette;
use crate::session::{GridSize, Session, ShellCommand};

/// Where the grid is on screen, from the last prepaint.
#[derive(Clone, Copy, PartialEq)]
pub struct Geometry {
    pub origin: Point<Pixels>,
    pub cell: Size<Pixels>,
    pub size: GridSize,
}

pub enum TerminalEvent {
    TitleChanged,
    Exited(Option<i32>),
}

pub struct TerminalView {
    session: Option<Session>,
    error: Option<String>,
    default_title: SharedString,
    title: SharedString,
    exited: Option<Option<i32>>,
    focus: FocusHandle,
    geometry: Option<Geometry>,
    preedit: Option<String>,
    selecting: bool,
    last_motion_cell: Option<(usize, usize)>,
    scroll_remainder: f32,
    _events: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

const START_SIZE: GridSize = GridSize { cols: 80, rows: 24 };

impl TerminalView {
    pub fn new(
        shell: ShellCommand,
        cwd: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus_in(&focus, window, |this, _, cx| this.focus_changed(true, cx)),
            cx.on_focus_out(&focus, window, |this, _, _, cx| {
                this.focus_changed(false, cx)
            }),
        ];
        let default_title: SharedString = Path::new(&shell.program)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| shell.program.clone())
            .into();
        let mut this = Self {
            session: None,
            error: None,
            title: default_title.clone(),
            default_title,
            exited: None,
            focus,
            geometry: None,
            preedit: None,
            selecting: false,
            last_motion_cell: None,
            scroll_remainder: 0.,
            _events: None,
            _subscriptions: subscriptions,
        };
        match Session::spawn(shell, cwd, START_SIZE, (8, 16)) {
            Ok((session, events)) => {
                this.session = Some(session);
                this._events = Some(Self::pump(events, cx));
            }
            Err(e) => {
                log::error!("terminal: can't start the shell: {e}");
                this.error = Some(format!("Couldn't start the shell: {e}"));
            }
        }
        this
    }

    fn pump(mut events: mpsc::UnboundedReceiver<Event>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            // Output arrives in bursts: handle everything queued, then redraw once.
            while let Some(first) = events.recv().await {
                let mut batch = vec![first];
                while let Ok(e) = events.try_recv() {
                    batch.push(e);
                }
                let alive = this.update(cx, |this, cx| {
                    for e in batch {
                        this.on_event(e, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        })
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Title(t) => {
                self.title = t.into();
                cx.emit(TerminalEvent::TitleChanged);
            }
            Event::ResetTitle => {
                self.title = self.default_title.clone();
                cx.emit(TerminalEvent::TitleChanged);
            }
            Event::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            Event::ClipboardLoad(_, format) => {
                let text = cx
                    .read_from_clipboard()
                    .and_then(|c| c.text())
                    .unwrap_or_default();
                self.write(format(&text).into_bytes());
            }
            Event::ColorRequest(index, format) => {
                let palette = Palette::from_theme(cx);
                let rgb = match &self.session {
                    Some(s) => palette.rgb_of(index, s.term.lock().colors()),
                    None => return,
                };
                self.write(format(rgb).into_bytes());
            }
            Event::PtyWrite(text) => self.write(text.into_bytes()),
            Event::TextAreaSizeRequest(format) => {
                if let Some(g) = self.geometry {
                    self.write(format(window_size(&g)).into_bytes());
                }
            }
            Event::ChildExit(status) => {
                self.exited = Some(status.code());
                cx.emit(TerminalEvent::Exited(status.code()));
            }
            Event::Exit => {
                if self.exited.is_none() {
                    self.exited = Some(None);
                    cx.emit(TerminalEvent::Exited(None));
                }
            }
            Event::Wakeup | Event::Bell | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
            }
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    pub fn has_exited(&self) -> bool {
        self.exited.is_some()
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub(crate) fn set_geometry(&mut self, g: Geometry) {
        if let Some(s) = &mut self.session {
            s.resize(g.size, cell_px(&g));
        }
        self.geometry = Some(g);
    }

    pub(crate) fn snapshot(&self, palette: &Palette) -> Option<element::Snapshot> {
        let s = self.session.as_ref()?;
        let term = s.term.lock();
        Some(element::snapshot(&term, palette))
    }

    pub(crate) fn preedit(&self) -> Option<&str> {
        self.preedit.as_deref()
    }

    fn write(&self, bytes: impl Into<std::borrow::Cow<'static, [u8]>>) {
        if let Some(s) = &self.session {
            s.write(bytes);
        }
    }

    fn mode(&self) -> TermMode {
        self.session
            .as_ref()
            .map(|s| *s.term.lock().mode())
            .unwrap_or(TermMode::empty())
    }

    /// Typing brings the view back to the prompt and drops the selection.
    fn type_bytes(
        &mut self,
        bytes: impl Into<std::borrow::Cow<'static, [u8]>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(s) = &self.session {
            let mut term = s.term.lock();
            term.selection = None;
            term.scroll_display(Scroll::Bottom);
        }
        self.write(bytes);
        cx.notify();
    }

    pub fn selected_text(&self) -> Option<String> {
        let s = self.session.as_ref()?;
        s.term
            .lock()
            .selection_to_string()
            .filter(|t| !t.is_empty())
    }

    /// Copy the selection. False if nothing is selected.
    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(text) = self.selected_text() else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if let Some(s) = &self.session {
            s.term.lock().selection = None;
        }
        cx.notify();
        true
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
            return;
        };
        let bytes = keys::paste_bytes(&text, self.mode());
        self.type_bytes(bytes, cx);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = &self.session {
            let mut term = s.term.lock();
            term.grid_mut().clear_history();
            term.selection = None;
        }
        // Ctrl+L: the shell redraws its prompt on a clean screen.
        self.type_bytes(&b"\x0c"[..], cx);
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        // Windows Terminal's rule: Ctrl+C copies when something is selected.
        if !cfg!(target_os = "macos")
            && ks.key == "c"
            && m.control
            && !m.shift
            && !m.alt
            && self.copy(cx)
        {
            cx.stop_propagation();
            return;
        }
        if let Some(bytes) = keys::to_bytes(ks, self.mode()) {
            self.type_bytes(bytes, cx);
            cx.stop_propagation();
        }
    }

    /// The mode, if the running program takes mouse events (vim, htop).
    /// Shift selects text anyway, as in other terminals.
    fn mouse_reporting(&self, modifiers: &Modifiers) -> Option<TermMode> {
        let mode = self.mode();
        (mode.intersects(TermMode::MOUSE_MODE) && !modifiers.shift).then_some(mode)
    }

    fn report_mouse(
        &mut self,
        button: u8,
        motion: bool,
        pressed: bool,
        position: Point<Pixels>,
        modifiers: &Modifiers,
        mode: TermMode,
    ) {
        let Some(g) = self.geometry else {
            return;
        };
        let cell = element::screen_cell(&g, position);
        if motion {
            if self.last_motion_cell == Some(cell) {
                return;
            }
            self.last_motion_cell = Some(cell);
        }
        if let Some(bytes) = keys::mouse_report(button, motion, pressed, cell, modifiers, mode) {
            self.write(bytes);
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if let Some(mode) = self.mouse_reporting(&event.modifiers) {
            let button = button_code(event.button);
            self.report_mouse(button, false, true, event.position, &event.modifiers, mode);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        let (Some(g), Some(s)) = (self.geometry, &self.session) else {
            return;
        };
        let mut term = s.term.lock();
        let (point, side) = element::grid_point(&g, event.position, term.grid().display_offset());
        let ty = match event.click_count {
            2 => SelectionType::Semantic,
            3.. => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        let mut selection = Selection::new(ty, point, side);
        if ty != SelectionType::Simple {
            selection.include_all();
        }
        term.selection = Some(selection);
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(mode) = self.mouse_reporting(&event.modifiers) {
            let button = event.pressed_button.map(button_code);
            if mode.contains(TermMode::MOUSE_MOTION)
                || (mode.contains(TermMode::MOUSE_DRAG) && button.is_some())
            {
                let button = button.unwrap_or(3);
                self.report_mouse(button, true, true, event.position, &event.modifiers, mode);
            }
            return;
        }
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (Some(g), Some(s)) = (self.geometry, &self.session) else {
            return;
        };
        let mut term = s.term.lock();
        let (point, side) = element::grid_point(&g, event.position, term.grid().display_offset());
        if let Some(sel) = &mut term.selection {
            sel.update(point, side);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting
            && let Some(mode) = self.mouse_reporting(&event.modifiers)
        {
            let button = button_code(event.button);
            self.report_mouse(button, false, false, event.position, &event.modifiers, mode);
            return;
        }
        self.selecting = false;
        if let Some(s) = &self.session {
            let mut term = s.term.lock();
            if term.selection.as_ref().is_some_and(|sel| sel.is_empty()) {
                term.selection = None;
            }
        }
        cx.notify();
    }

    fn scroll_wheel(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = self.geometry else {
            return;
        };
        let line_height = f32::from(g.cell.height);
        self.scroll_remainder += f32::from(event.delta.pixel_delta(g.cell.height).y);
        let lines = (self.scroll_remainder / line_height) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= lines as f32 * line_height;
        if let Some(mode) = self.mouse_reporting(&event.modifiers) {
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.unsigned_abs() {
                self.report_mouse(button, false, true, event.position, &event.modifiers, mode);
            }
            return;
        }
        let mode = self.mode();
        // Full-screen programs without mouse support (less) get arrow keys.
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            let key: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            self.write(key.repeat(lines.unsigned_abs() as usize));
        } else if let Some(s) = &self.session {
            s.term.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        if self.mode().contains(TermMode::FOCUS_IN_OUT) {
            self.write(if focused {
                &b"\x1b[I"[..]
            } else {
                &b"\x1b[O"[..]
            });
        }
        cx.notify();
    }

    /// For pi: the selection, else what's on screen.
    pub fn ask_pi_draft(&self) -> Option<String> {
        let text = self.selected_text().or_else(|| self.screen_text())?;
        Some(format!(
            "From the terminal ({}):\n```\n{}\n```\n",
            self.title,
            text.trim_end()
        ))
    }

    fn screen_text(&self) -> Option<String> {
        let s = self.session.as_ref()?;
        let term = s.term.lock();
        let offset = term.grid().display_offset() as i32;
        let start = GridPoint::new(Line(-offset), Column(0));
        let end = GridPoint::new(
            Line(term.screen_lines() as i32 - 1 - offset),
            Column(term.columns() - 1),
        );
        let text = term.bounds_to_string(start, end);
        let text = text.trim_end();
        (!text.is_empty()).then(|| text.to_string())
    }
}

fn button_code(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        _ => 3,
    }
}

fn cell_px(g: &Geometry) -> (u16, u16) {
    (
        f32::from(g.cell.width).round() as u16,
        f32::from(g.cell.height).round() as u16,
    )
}

fn window_size(g: &Geometry) -> WindowSize {
    let (cell_width, cell_height) = cell_px(g);
    WindowSize {
        num_lines: g.size.rows as u16,
        num_cols: g.size.cols as u16,
        cell_width,
        cell_height,
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity();
        div()
            .id("terminal")
            .key_context(pide_core::commands::TERMINAL)
            .track_focus(&self.focus)
            .size_full()
            .px_1()
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll_wheel))
            .context_menu(|menu, _, _| {
                let run = |id: &'static str| {
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        window.defer(cx, move |window, cx| {
                            CommandRegistry::run(id, window, cx);
                        });
                    }
                };
                menu.item(PopupMenuItem::new("Copy").on_click(run("terminal.copy")))
                    .item(PopupMenuItem::new("Paste").on_click(run("terminal.paste")))
                    .separator()
                    .item(PopupMenuItem::new("Ask pi About This").on_click(run("terminal.ask_pi")))
                    .separator()
                    .item(PopupMenuItem::new("Clear").on_click(run("terminal.clear")))
                    .item(PopupMenuItem::new("New Tab").on_click(run("terminal.new")))
                    .item(PopupMenuItem::new("Close Tab").on_click(run("terminal.close")))
            })
            .child(match &self.error {
                Some(e) => div().p_2().child(e.clone()).into_any_element(),
                None => TerminalElement::new(me, self.focus.clone()).into_any_element(),
            })
    }
}

/// Text from the keyboard (IME, dead keys, AltGr) goes straight to the shell;
/// only the IME's unfinished text is kept, to draw at the cursor.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.preedit.as_ref().map(|t| 0..t.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.preedit = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit = None;
        self.type_bytes(text.as_bytes().to_vec(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preedit = (!text.is_empty()).then(|| text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let g = self.geometry?;
        let s = self.session.as_ref()?;
        let term = s.term.lock();
        let cursor = term.grid().cursor.point;
        let row = cursor.line.0 + term.grid().display_offset() as i32;
        Some(Bounds::new(
            point(
                g.origin.x + g.cell.width * cursor.column.0 as f32,
                g.origin.y + g.cell.height * row.max(0) as f32,
            ),
            g.cell,
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
