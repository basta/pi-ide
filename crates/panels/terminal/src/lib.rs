//! Terminal tool window (bottom dock): shells in a PTY, one per tab.
//! alacritty_terminal does the PTY (ConPTY on Windows), the escape-sequence
//! parsing, the grid and scrollback; pide draws it (`element.rs`) and maps
//! keys (`keys.rs`).

mod element;
mod keys;
mod palette;
mod session;
mod view;

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use pide_core::commands::TERMINAL;
use pide_core::{CommandRegistry, CommandSpec, DockPosition, Panel, Services};

pub use view::{TerminalEvent, TerminalView};

pub const PANEL_NAME: &str = "terminal";

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| TerminalPanel::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "Terminal",
            icon: IconName::SquareTerminal,
            dock: DockPosition::Bottom,
            command: "view.terminal",
            key: Some("alt-f12"),
            create: |window, cx| panel_handle(cx.new(|cx| TerminalPanel::new(window, cx))),
        },
    );
    let on_panel = |f: fn(&mut TerminalPanel, &mut Window, &mut Context<TerminalPanel>)| {
        move |window: &mut Window, cx: &mut App| {
            if let Some(p) = existing(window, cx) {
                p.update(cx, |p, cx| f(p, window, cx));
            }
        }
    };
    let on_view = |f: fn(&mut TerminalView, &mut Context<TerminalView>)| {
        move |window: &mut Window, cx: &mut App| {
            if let Some(v) = existing(window, cx).and_then(|p| p.read(cx).active_view()) {
                v.update(cx, f);
            }
        }
    };
    CommandRegistry::register(
        cx,
        CommandSpec::new("terminal.new", "Terminal: New Tab"),
        move |window, cx| {
            pide_core::toolwindow::show(PANEL_NAME, true, window, cx);
            if let Some(p) = existing(window, cx) {
                p.update(cx, |p, cx| p.add_tab(window, cx));
            }
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("terminal.close", "Terminal: Close Tab"),
        on_panel(|p, window, cx| p.close_tab(p.active, window, cx)),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("terminal.copy", "Terminal: Copy")
            .key("cmd-c")
            .other_key("ctrl-shift-c")
            .in_context(TERMINAL),
        on_view(|v, cx| {
            v.copy(cx);
        }),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("terminal.paste", "Terminal: Paste")
            .key("cmd-v")
            .other_key("ctrl-shift-v")
            .in_context(TERMINAL),
        on_view(|v, cx| v.paste(cx)),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("terminal.clear", "Terminal: Clear"),
        on_view(|v, cx| v.clear(cx)),
    );
}

#[derive(Default)]
struct Instances(HashMap<WindowId, WeakEntity<TerminalPanel>>);
impl Global for Instances {}

/// This window's Terminal panel, if one exists.
pub fn existing(window: &Window, cx: &App) -> Option<Entity<TerminalPanel>> {
    cx.try_global::<Instances>()?
        .0
        .get(&window.window_handle().window_id())?
        .upgrade()
}

pub struct TerminalPanel {
    root: PathBuf,
    tabs: Vec<Entity<TerminalView>>,
    active: usize,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for TerminalPanel {}

impl TerminalPanel {
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
        let mut this = Self {
            root,
            tabs: Vec::new(),
            active: 0,
            focus: cx.focus_handle(),
            _subscriptions: Vec::new(),
        };
        this.add_tab(window, cx);
        this
    }

    pub fn active_view(&self) -> Option<Entity<TerminalView>> {
        self.tabs.get(self.active).cloned()
    }

    pub fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let view = cx.new(|cx| TerminalView::new(session::default_shell(), &root, cx));
        let sub = cx.subscribe_in(&view, window, |this, view, event, window, cx| match event {
            TerminalEvent::TitleChanged => cx.notify(),
            // A shell that ended normally (`exit`) closes its tab; a failure stays to be read.
            TerminalEvent::Exited(Some(0)) => {
                if let Some(i) = this.tabs.iter().position(|v| v == view) {
                    this.close_tab(i, window, cx);
                }
            }
            TerminalEvent::Exited(_) => cx.notify(),
        });
        self._subscriptions.push(sub);
        self.tabs.push(view);
        self.select(self.tabs.len() - 1, window, cx);
    }

    fn select(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = i.min(self.tabs.len().saturating_sub(1));
        if let Some(v) = self.active_view() {
            let focus = v.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    pub fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.active = 0;
            // Like JetBrains: closing the last shell hides the tool window.
            let host = cx
                .try_global::<pide_core::toolwindow::ToolWindowHost>()
                .cloned();
            window.defer(cx, move |window, cx| {
                if let Some(host) = host {
                    (host.toggle)(PANEL_NAME, window, cx);
                }
            });
            cx.notify();
            return;
        }
        let next = if self.active > i || self.active == self.tabs.len() {
            self.active.saturating_sub(1)
        } else {
            self.active
        };
        self.select(next, window, cx);
    }
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.active_view() {
            Some(v) => v.read(cx).focus_handle().clone(),
            None => self.focus.clone(),
        }
    }
}

impl gpui_kit::component::dock::BasePanel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for TerminalPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.title_text(cx)
    }
}

impl Panel for TerminalPanel {
    fn title_text(&self, _: &App) -> SharedString {
        "Terminal".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Bottom
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.tabs.is_empty() && self.focus.is_focused(window) {
            // Shown again after its last tab closed: start a fresh shell.
            cx.defer_in(window, |this, window, cx| {
                if this.tabs.is_empty() {
                    this.add_tab(window, cx);
                }
            });
        }
        let theme = cx.theme().clone();
        let tabs = self.tabs.iter().enumerate().map(|(i, v)| {
            let view = v.read(cx);
            let mut title = view.title().to_string();
            if view.has_exited() {
                title.push_str(" (exited)");
            }
            let on = i == self.active;
            h_flex()
                .gap_0p5()
                .child(
                    pide_core::colors::toggle(
                        Button::new(("terminal-tab", i)).xsmall().label(title),
                        on,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.select(i, window, cx))),
                )
                .child(
                    Button::new(("terminal-tab-close", i))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Close)
                        .tooltip("Close this shell")
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.close_tab(i, window, cx)),
                        ),
                )
        });
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(theme.background)
            .child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .border_b_1()
                    .border_color(theme.border)
                    .children(tabs)
                    .child(
                        Button::new("terminal-new")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Plus)
                            .tooltip("New shell")
                            .on_click(cx.listener(|this, _, window, cx| this.add_tab(window, cx))),
                    ),
            )
            .child(div().flex_1().min_h_0().children(self.active_view()))
    }
}
