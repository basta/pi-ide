//! Structure tool window (Cmd+7): the outline of the file in the active
//! editor tab. Headings for Typst, LaTeX and Markdown; symbols for code.
//! From the file's language server (`textDocument/documentSymbol`); prose
//! without one is scanned for headings (`outline::scan_headings`). Follows
//! the cursor; click or Enter jumps there.

pub mod outline;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{PanelEvent, PanelState, panel_handle, register_panel};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::DocumentSymbolResponse;
use pide_core::{DockPosition, Panel, Services, Workspace, WorkspaceEvent};
use pide_lsp::LspStore;
use pide_lsp::process::file_uri;
use serde_json::json;

use outline::Entry;

pub const PANEL_NAME: &str = "structure";
const ROW_H: f32 = 24.;
/// Wait for a pause in typing before asking again.
const EDIT_DELAY: Duration = Duration::from_millis(400);

pub fn init(cx: &mut App) {
    register_panel(cx, PANEL_NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| Structure::new(window, cx)))
    });
    pide_core::toolwindow::register(
        cx,
        pide_core::ToolWindowSpec {
            id: PANEL_NAME,
            title: "Structure",
            // GPUI Kit's default icon set has no tree/outline icon.
            icon: IconName::Map,
            dock: DockPosition::Left,
            command: "view.structure",
            key: Some("cmd-7"),
            create: std::rc::Rc::new(|window, cx| {
                panel_handle(cx.new(|cx| Structure::new(window, cx)))
            }),
        },
    );
}

#[derive(Clone, Debug, PartialEq)]
enum Status {
    NoFile,
    Loading,
    Ready,
    /// Code without a running language server.
    NoServer(String),
}

pub struct Structure {
    workspace: Entity<Workspace>,
    store: Entity<LspStore>,
    root: PathBuf,
    path: Option<PathBuf>,
    status: Status,
    /// Everything the source gave, before the headings filter.
    all: Vec<Entry>,
    entries: Vec<Entry>,
    ids: Vec<String>,
    rows: Vec<usize>,
    /// Typst/LaTeX: only headings (no labels, figures, `#let`s).
    headings_only: bool,
    /// Collapsed entries, per file.
    collapsed: HashMap<PathBuf, HashSet<String>>,
    /// The highlighted entry (index into `entries`): the cursor's, or moved with the keys.
    selected: Option<usize>,
    cursor_line: Option<u32>,
    /// The last load had the file's language server running.
    had_server: bool,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _load: Option<Task<()>>,
    _poll: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for Structure {}

impl Structure {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = Services::of(window, cx).workspace;
        let store = LspStore::for_workspace(&workspace, cx);
        let subs = vec![
            cx.subscribe(&workspace, |this, ws, ev: &WorkspaceEvent, cx| match ev {
                WorkspaceEvent::ActiveBufferChanged => this.load(Duration::ZERO, cx),
                WorkspaceEvent::BufferChanged(b)
                | WorkspaceEvent::FileChangedOnDisk { buffer: b, .. }
                    if Some(&b.read(cx).path) == this.path.as_ref() =>
                {
                    this.load(EDIT_DELAY, cx)
                }
                WorkspaceEvent::BufferClosed(p) if Some(p) == this.path.as_ref() => {
                    // Another tab may still be active; otherwise there's nothing to show.
                    if ws.read(cx).active().is_some_and(|b| &b.read(cx).path != p) {
                        this.load(Duration::ZERO, cx);
                    } else {
                        this.clear(cx);
                    }
                }
                _ => {}
            }),
            // The server came up after the file opened: ask it now.
            cx.subscribe(&store, |this, store, ev, cx| {
                if let pide_lsp::LspEvent::Status = ev
                    && !this.had_server
                    && this
                        .path
                        .as_ref()
                        .is_some_and(|p| store.read(cx).server_for(p).is_some())
                {
                    this.load(Duration::ZERO, cx);
                }
            }),
        ];
        let mut this = Self {
            root: workspace.read(cx).root().to_path_buf(),
            workspace,
            store,
            path: None,
            status: Status::NoFile,
            all: vec![],
            entries: vec![],
            ids: vec![],
            rows: vec![],
            headings_only: true,
            collapsed: HashMap::new(),
            selected: None,
            cursor_line: None,
            had_server: false,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _load: None,
            _poll: None,
            _subs: subs,
        };
        this.load(Duration::ZERO, cx);
        this.follow_cursor(window, cx);
        this
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.path = None;
        self.status = Status::NoFile;
        self.all.clear();
        self.apply(cx);
    }

    /// Read the active file's outline after `delay` (a newer call replaces it).
    fn load(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(buffer) = self.workspace.read(cx).active().cloned() else {
            self.clear(cx);
            return;
        };
        let (path, rope) = {
            let b = buffer.read(cx);
            (b.path.clone(), b.text().clone())
        };
        if self.path.as_ref() != Some(&path) {
            self.path = Some(path.clone());
            self.status = Status::Loading;
            self.all.clear();
            self.selected = None;
            self.cursor_line = None;
            self.apply(cx);
        }
        let prose = outline::is_prose(&path);
        let store = self.store.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let server = cx.update(|cx| {
                let s = store.read(cx).server_for(&path).map(|s| s.encoding);
                let task = s.map(|_| {
                    store.update(cx, |s, cx| {
                        s.request(
                            &path,
                            Some(&rope),
                            "textDocument/documentSymbol",
                            json!({ "textDocument": { "uri": file_uri(&path) } }),
                            cx,
                        )
                    })
                });
                s.zip(task)
            });
            let had_server = server.is_some();
            let from_server = match server {
                Some((enc, task)) => {
                    let answer = task.await;
                    let r = rope.clone();
                    cx.background_executor()
                        .spawn(async move {
                            let resp = answer.ok().and_then(|v| {
                                serde_json::from_value::<Option<DocumentSymbolResponse>>(v).ok()?
                            })?;
                            let to_chars = |p| pide_lsp::convert::from_server(&r, p, enc);
                            Some(outline::from_lsp(resp, prose, &to_chars))
                        })
                        .await
                }
                None => None,
            };
            // Prose: headings from the text when the server has nothing (yet).
            let entries = match from_server {
                Some(e) if !(prose && e.is_empty()) => Some(e),
                _ if prose => {
                    let p = path.clone();
                    let text = rope.to_string();
                    Some(
                        cx.background_executor()
                            .spawn(async move { outline::scan_headings(&text, &p) })
                            .await,
                    )
                }
                _ => None,
            };
            let _ = this.update(cx, |this, cx| {
                if this.path.as_ref() != Some(&path) {
                    return; // another file meanwhile
                }
                this.had_server = had_server;
                match entries {
                    Some(e) => {
                        this.all = e;
                        this.status = Status::Ready;
                    }
                    None => {
                        this.all.clear();
                        this.status = Status::NoServer(match store.read(cx).server_of(&path) {
                            Some(s) => format!("{}: {}", s.spec.id, s.status_text()),
                            None => "No language server for this file.".into(),
                        });
                    }
                }
                this.apply(cx);
            });
        }));
    }

    /// Recompute what's shown from `all` (filter, collapse, current entry).
    fn apply(&mut self, cx: &mut Context<Self>) {
        let prose = self.path.as_ref().is_some_and(|p| outline::is_prose(p));
        let selected_id = self.selected.and_then(|i| self.ids.get(i).cloned());
        self.entries = if prose && self.headings_only {
            outline::headings_only(&self.all)
        } else {
            self.all.clone()
        };
        self.ids = outline::ids(&self.entries);
        self.refresh_rows();
        // The cursor's entry (it may have just been typed), else the same one as before.
        let target = self
            .cursor_line
            .and_then(|l| outline::current(&self.entries, l))
            .or_else(|| selected_id.and_then(|id| self.ids.iter().position(|i| *i == id)));
        self.selected = None;
        match target {
            Some(i) => self.select(i, cx),
            None => cx.notify(),
        }
    }

    fn refresh_rows(&mut self) {
        let empty = HashSet::new();
        let collapsed = self
            .path
            .as_ref()
            .and_then(|p| self.collapsed.get(p))
            .unwrap_or(&empty);
        self.rows = outline::visible(&self.entries, &self.ids, collapsed);
    }

    /// Poll the cursor (cheap) and highlight its entry, scrolling it into view.
    fn follow_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._poll = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let alive = this.update_in(cx, |this, window, cx| {
                    let Some(ctx) = pide_core::context::current(window, cx) else {
                        return;
                    };
                    if Some(this.root.join(&ctx.path)) != this.path {
                        return;
                    }
                    let line = ctx.start.0.saturating_sub(1);
                    if this.cursor_line == Some(line) {
                        return;
                    }
                    this.cursor_line = Some(line);
                    if let Some(i) = outline::current(&this.entries, line) {
                        this.select(i, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    /// Highlight entry `i` (or its shown ancestor) and scroll it into view.
    fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(i) = outline::shown_ancestor(&self.entries, &self.rows, i) else {
            return;
        };
        if self.selected != Some(i) {
            self.selected = Some(i);
            if let Some(row) = self.rows.iter().position(|&r| r == i) {
                self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
            }
            cx.notify();
        }
    }

    fn jump(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(path), Some(e)) = (self.path.clone(), self.entries.get(i)) else {
            return;
        };
        self.selected = Some(i);
        self.cursor_line = Some(e.line);
        pide_core::opener::open_file_at(path, e.line, e.character, window, cx);
    }

    fn set_collapsed(&mut self, i: usize, collapse: bool, cx: &mut Context<Self>) {
        let (Some(path), Some(id)) = (self.path.clone(), self.ids.get(i).cloned()) else {
            return;
        };
        let set = self.collapsed.entry(path).or_default();
        if collapse {
            set.insert(id);
        } else {
            set.remove(&id);
        }
        self.refresh_rows();
        if let Some(s) = self.selected {
            self.selected = outline::shown_ancestor(&self.entries, &self.rows, s);
        }
        cx.notify();
    }

    fn is_collapsed(&self, i: usize) -> bool {
        self.path
            .as_ref()
            .and_then(|p| self.collapsed.get(p))
            .zip(self.ids.get(i))
            .is_some_and(|(set, id)| set.contains(id))
    }

    /// Collapse everything below the top level, or expand it all.
    fn collapse_all(&mut self, collapse: bool, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let set = self.collapsed.entry(path).or_default();
        set.clear();
        if collapse {
            for (i, id) in self.ids.iter().enumerate() {
                if outline::has_children(&self.entries, i) {
                    set.insert(id.clone());
                }
            }
        }
        self.refresh_rows();
        if let Some(s) = self.selected {
            self.selected = outline::shown_ancestor(&self.entries, &self.rows, s);
        }
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.modifiers.modified() {
            return;
        }
        let row = self
            .selected
            .and_then(|s| self.rows.iter().position(|&r| r == s));
        match ks.key.as_str() {
            "down" | "up" => {
                if self.rows.is_empty() {
                    return;
                }
                let next = match (row, ks.key.as_str()) {
                    (None, _) => 0,
                    (Some(r), "down") => (r + 1).min(self.rows.len() - 1),
                    (Some(r), _) => r.saturating_sub(1),
                };
                self.select(self.rows[next], cx);
            }
            "enter" => {
                if let Some(s) = self.selected {
                    self.jump(s, window, cx);
                }
            }
            "left" => {
                let Some(s) = self.selected else { return };
                if outline::has_children(&self.entries, s) && !self.is_collapsed(s) {
                    self.set_collapsed(s, true, cx);
                } else if let Some(parent) = (0..s)
                    .rev()
                    .find(|&j| self.entries[j].depth < self.entries[s].depth)
                {
                    self.select(parent, cx);
                }
            }
            "right" => {
                if let Some(s) = self.selected
                    && self.is_collapsed(s)
                {
                    self.set_collapsed(s, false, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn render_row(&self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let i = self.rows[row];
        let e = &self.entries[i];
        let parent = outline::has_children(&self.entries, i);
        let collapsed = self.is_collapsed(i);
        let selected = self.selected == Some(i);
        let prose = self.path.as_ref().is_some_and(|p| outline::is_prose(p));
        let chevron = if parent {
            div()
                .id(("structure-fold", i))
                .flex_none()
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .hover(|s| s.bg(theme.list_hover))
                .child(
                    Icon::new(if collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .xsmall()
                    .text_color(theme.muted_foreground),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let c = this.is_collapsed(i);
                    this.set_collapsed(i, !c, cx);
                }))
                .into_any_element()
        } else {
            div().flex_none().size(px(16.)).into_any_element()
        };
        h_flex()
            .id(("structure-row", i))
            .h(px(ROW_H))
            .w_full()
            .pl(px(6. + 14. * e.depth as f32))
            .pr_2()
            .gap_1()
            .rounded(theme.radius)
            .cursor_pointer()
            .when(selected, |d| d.bg(theme.list_active))
            .when(!selected, |d| d.hover(|s| s.bg(theme.list_hover)))
            .child(chevron)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    // Top-level headings carry the document's shape.
                    .when(prose && e.heading && e.depth == 0, |d| {
                        d.font_weight(FontWeight::SEMIBOLD)
                    })
                    .child(e.name.clone()),
            )
            .when(!e.kind.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(e.kind),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| this.jump(i, window, cx)))
            .into_any_element()
    }
}

impl Focusable for Structure {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for Structure {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState::new(PANEL_NAME)
    }
}

impl gpui_kit::component::dock::Panel for Structure {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Structure"
    }
}

impl Panel for Structure {
    fn title_text(&self, _: &App) -> SharedString {
        "Structure".into()
    }
    fn default_dock(&self) -> DockPosition {
        DockPosition::Left
    }
}

impl Render for Structure {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let prose = self.path.as_ref().is_some_and(|p| outline::is_prose(p));
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string());
        let any_parent = (0..self.entries.len()).any(|i| outline::has_children(&self.entries, i));
        let header =
            h_flex()
                .flex_none()
                .h(px(30.))
                .px_2()
                .gap_1()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .children(name),
                )
                // Only when there's something besides headings to hide.
                .when(prose && self.all.iter().any(|e| !e.heading), |h| {
                    h.child(
                        pide_core::colors::toggle(
                            Button::new("structure-headings").label("Headings").xsmall(),
                            self.headings_only,
                            cx,
                        )
                        .tooltip("Only headings (hide labels, figures and definitions)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.headings_only = !this.headings_only;
                            this.apply(cx);
                        })),
                    )
                })
                .when(any_parent, |h| {
                    // One button: collapse everything, or expand it all again.
                    let any_collapsed = self
                        .path
                        .as_ref()
                        .and_then(|p| self.collapsed.get(p))
                        .is_some_and(|s| !s.is_empty());
                    h.child(
                        Button::new("structure-fold-all")
                            .icon(IconName::ChevronsUpDown)
                            .ghost()
                            .xsmall()
                            .tooltip(if any_collapsed {
                                "Expand All"
                            } else {
                                "Collapse All"
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.collapse_all(!any_collapsed, cx)
                            })),
                    )
                });
        let empty = |text: String| {
            div()
                .p_3()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let body = match &self.status {
            Status::NoFile => empty("Open a file to see its structure.".into()),
            Status::Loading => empty(String::new()),
            Status::NoServer(why) => empty(why.clone()),
            Status::Ready if self.entries.is_empty() => empty(if prose {
                "No headings yet.".into()
            } else {
                "Nothing to show.".into()
            }),
            Status::Ready => uniform_list(
                "structure-rows",
                self.rows.len(),
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    range.map(|row| this.render_row(row, cx)).collect()
                }),
            )
            .track_scroll(&self.scroll)
            .px_1()
            .py_1()
            .size_full()
            .into_any_element(),
        };
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Structure")
            .on_key_down(cx.listener(Self::on_key))
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}
