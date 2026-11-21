//! Problems panel (spec M3): every diagnostic the project's language servers
//! report, grouped by file, errors first. Clicking one opens the file there.

use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::{ActiveTheme, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::DiagnosticSeverity;
use pide_core::{DockPosition, Panel, Services};
use pide_lsp::LspStore;

pub const PANEL_NAME: &str = "problems";

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| Problems::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "Problems",
            icon: gpui_kit::component::IconName::TriangleAlert,
            dock: pide_core::DockPosition::Bottom,
            command: "view.problems",
            key: Some("cmd-6"),
            create: std::rc::Rc::new(|window, cx| {
                panel_handle(cx.new(|cx| Problems::new(window, cx)))
            }),
        },
    );
}

/// Errors and warnings across the project (for the status bar too).
pub fn counts(store: &LspStore) -> (usize, usize) {
    count(store.diagnostics())
}

fn count(diags: &std::collections::BTreeMap<PathBuf, pide_lsp::FileDiagnostics>) -> (usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    for d in diags.values().flat_map(|f| &f.items) {
        match d.severity {
            Some(DiagnosticSeverity::ERROR) => errors += 1,
            Some(DiagnosticSeverity::WARNING) => warnings += 1,
            _ => {}
        }
    }
    (errors, warnings)
}

/// The open instance per window (restored from a layout or opened by command),
/// so the command focuses it instead of adding a second one.
#[derive(Default)]
struct Instances(std::collections::HashMap<WindowId, WeakEntity<Problems>>);
impl Global for Instances {}

/// This window's open Problems panel, if any.
pub fn existing(window: &Window, cx: &App) -> Option<Entity<Problems>> {
    cx.try_global::<Instances>()?
        .0
        .get(&window.window_handle().window_id())?
        .upgrade()
}

pub struct Problems {
    store: Entity<LspStore>,
    /// What the list shows: the store's diagnostics, taken once they've been
    /// still for a moment. Every keystroke on a half-typed line made the list,
    /// the tab's count and its width churn.
    shown: std::collections::BTreeMap<PathBuf, pide_lsp::FileDiagnostics>,
    _settle: Option<Task<()>>,
    root: PathBuf,
    show_hints: bool,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for Problems {}

impl Problems {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = Services::of(window, cx).workspace;
        let root = workspace.read(cx).root().to_path_buf();
        let store = LspStore::for_workspace(&workspace, cx);
        let me = cx.weak_entity();
        cx.default_global::<Instances>()
            .0
            .insert(window.window_handle().window_id(), me);
        let shown = store.read(cx).diagnostics().clone();
        Self {
            _subs: vec![cx.observe(&store, |this, store, cx| {
                if store.read(cx).diagnostics().len() == this.shown.len()
                    && store
                        .read(cx)
                        .diagnostics()
                        .iter()
                        .zip(this.shown.iter())
                        .all(|(a, b)| a.0 == b.0 && a.1.items == b.1.items)
                {
                    cx.notify(); // server status only
                    return;
                }
                this._settle = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(700))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.shown = this.store.read(cx).diagnostics().clone();
                        cx.notify();
                    });
                }));
            })],
            shown,
            _settle: None,
            store,
            root,
            show_hints: false,
            focus: cx.focus_handle(),
        }
    }

    fn rel<'a>(&self, path: &'a Path) -> std::borrow::Cow<'a, str> {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
    }
}

/// Sort key: errors, then warnings, then the rest.
fn rank(s: Option<DiagnosticSeverity>) -> u8 {
    match s {
        Some(DiagnosticSeverity::ERROR) => 0,
        Some(DiagnosticSeverity::WARNING) => 1,
        Some(DiagnosticSeverity::INFORMATION) => 2,
        _ => 3,
    }
}

impl Focusable for Problems {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for Problems {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for Problems {
    fn title(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (e, w) = count(&self.shown);
        if e + w == 0 {
            "Problems".to_string()
        } else {
            format!("Problems · {}", e + w)
        }
    }
}

impl Panel for Problems {
    fn title_text(&self, _: &App) -> SharedString {
        "Problems".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Bottom
    }
}

/// Open `path` at a server position: its column is in the server's encoding
/// (often UTF-16), the editor's in characters; convert with the file's text,
/// read off the UI thread.
fn jump(
    path: PathBuf,
    line: u32,
    character: u32,
    enc: pide_lsp::convert::Encoding,
    window: &mut Window,
    cx: &mut App,
) {
    let p = path.clone();
    let read = cx.background_executor().spawn(async move {
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        pide_lsp::convert::from_server(
            &pide_lsp::convert::TextLines::new(&text),
            lsp_types::Position::new(line, character),
            enc,
        )
    });
    window
        .spawn(cx, async move |cx| {
            let pos = read.await;
            let _ = cx.update(|window, cx| {
                pide_core::opener::open_file_at(path, pos.line, pos.character, window, cx)
            });
        })
        .detach();
}

impl Render for Problems {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        let (errors, warnings) = count(&self.shown);
        let show_hints = self.show_hints;
        let mut encodings = std::collections::HashMap::new();

        // Files with an error first, then by path.
        let mut files: Vec<(&PathBuf, Vec<&lsp_types::Diagnostic>)> = self
            .shown
            .iter()
            .map(|(p, f)| {
                encodings.insert(p.clone(), f.encoding);
                let mut items: Vec<_> = f
                    .items
                    .iter()
                    .filter(|d| show_hints || rank(d.severity) <= 1)
                    .collect();
                items.sort_by_key(|d| {
                    (
                        rank(d.severity),
                        d.range.start.line,
                        d.range.start.character,
                    )
                });
                (p, items)
            })
            .filter(|(_, items)| !items.is_empty())
            .collect();
        files.sort_by(|a, b| {
            let ka = a.1.iter().map(|d| rank(d.severity)).min();
            let kb = b.1.iter().map(|d| rank(d.severity)).min();
            ka.cmp(&kb).then_with(|| a.0.cmp(b.0))
        });

        let mut rows: Vec<AnyElement> = Vec::new();
        for (path, items) in files {
            rows.push(
                h_flex()
                    .gap_2()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .text_sm()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.rel(path).to_string()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(items.len().to_string()),
                    )
                    .into_any_element(),
            );
            for (i, d) in items.into_iter().enumerate() {
                let (label, color) = match d.severity {
                    Some(DiagnosticSeverity::ERROR) => ("error", theme.danger),
                    Some(DiagnosticSeverity::WARNING) => ("warning", theme.warning),
                    Some(DiagnosticSeverity::INFORMATION) => ("info", theme.info),
                    _ => ("hint", theme.muted_foreground),
                };
                let message = d.message.lines().next().unwrap_or("").to_string();
                let source = d.source.clone().unwrap_or_default();
                let (line, character) = (d.range.start.line, d.range.start.character);
                let target = path.clone();
                let enc = encodings.get(path.as_path()).copied().unwrap_or_default();
                let rel = path
                    .strip_prefix(&self.root)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                let draft = format!(
                    "Fix this {label} in {rel}:{}:{}: {}\n",
                    line + 1,
                    character + 1,
                    d.message.trim()
                );
                rows.push(
                    h_flex()
                        .id(SharedString::from(format!(
                            "p-{}-{i}-{line}-{character}",
                            path.display()
                        )))
                        .gap_2()
                        .px_3()
                        .pl_6()
                        .py_0p5()
                        .text_sm()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.list_hover))
                        .on_click(move |_, window, cx| {
                            jump(target.clone(), line, character, enc, window, cx)
                        })
                        .child(
                            div()
                                .w(px(56.))
                                .flex_none()
                                .text_xs()
                                .text_color(color)
                                .child(label),
                        )
                        .child(
                            div()
                                .flex_1()
                                // When narrow the message keeps its room and the
                                // position/source after it is cut instead.
                                .min_w(px(140.))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(message),
                        )
                        .child(
                            div()
                                .flex_shrink(1.)
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{}:{} {source}", line + 1, character + 1)),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "ask-{}-{i}-{line}",
                                path.display()
                            )))
                            .xsmall()
                            .ghost()
                            .label("Ask pi")
                            .tooltip("Put this problem in pi's message box")
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                pide_core::opener::ask_agent(draft.clone(), window, cx);
                            }),
                        )
                        .into_any_element(),
                );
            }
        }

        let summary = match (errors, warnings) {
            (0, 0) => "No errors or warnings".to_string(),
            (e, w) => format!(
                "{e} error{} · {w} warning{}",
                if e == 1 { "" } else { "s" },
                if w == 1 { "" } else { "s" }
            ),
        };
        let any_server = store.servers().next().is_some();

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .child(
                h_flex()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(summary)
                    .child(
                        div()
                            .id("toggle-hints")
                            .cursor_pointer()
                            .child(if show_hints { "Hide hints" } else { "Show hints" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_hints = !this.show_hints;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .id("problems-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(rows.is_empty(), |this| {
                        this.child(div().p_3().text_sm().text_color(theme.muted_foreground).child(if any_server {
                            "Nothing to fix."
                        } else {
                            "Open a file with a language server (Settings → Language Servers…) to see problems here."
                        }))
                    })
                    .children(rows),
            )
    }
}
