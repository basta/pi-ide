//! Editor tabs. All editing comes from GPUI Kit's editor; this crate is glue:
//! it shows a workspace [`Buffer`], pushes edits back to the workspace, and
//! shows the bars around it (read-only, conflict, deleted). Also the pi diff tab.

pub mod diff_panel;
mod follow;
mod git_gutter;
mod lsp;
mod rename;
mod signature;
mod symbols;
pub use lsp::apply_workspace_edit;

use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::dock::{
    ClosePanel, DockArea, DockPlacement, PanelEvent, PanelInfo, PanelState, TabGroup, panel_handle,
    register_panel,
};
use gpui_kit::component::input::{Editor, EditorState, InputEditsEvent, InputEvent, TabSize};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::{
    Buffer, CommandRegistry, CommandSpec, DockPosition, Panel, Services, Workspace, WorkspaceEvent,
};

pub const PANEL_NAME: &str = "editor";

/// Where new editor tabs go, per project window. Set by the app once a window's dock exists.
#[derive(Default)]
struct EditorHosts(std::collections::HashMap<WindowId, WeakEntity<DockArea>>);
impl Global for EditorHosts {}

pub fn set_host(window: WindowId, dock: WeakEntity<DockArea>, cx: &mut App) {
    cx.default_global::<EditorHosts>().0.insert(window, dock);
}

pub fn remove_host(window: WindowId, cx: &mut App) {
    cx.default_global::<EditorHosts>().0.remove(&window);
}

pub(crate) fn host(window: &Window, cx: &App) -> Option<Entity<DockArea>> {
    cx.try_global::<EditorHosts>()?
        .0
        .get(&window.window_handle().window_id())?
        .upgrade()
}

#[derive(Default)]
struct OpenEditors(Vec<WeakEntity<EditorPanel>>);
impl Global for OpenEditors {}

pub fn init(cx: &mut App) {
    cx.set_global(OpenEditors::default());
    diff_panel::init(cx);
    cx.set_global(pide_core::context::EditorContextSource(std::rc::Rc::new(
        editor_context,
    )));

    // Rebuild editor tabs from a saved layout.
    register_panel(cx, PANEL_NAME, |ctx, window, cx| {
        let path = match ctx.info() {
            PanelInfo::Panel(v) => v.get("path").and_then(|p| p.as_str()).map(PathBuf::from),
            _ => None,
        };
        let panel = cx.new(|cx| EditorPanel::new(path.unwrap_or_default(), window, cx));
        panel_handle(panel)
    });

    CommandRegistry::register(
        cx,
        CommandSpec::new("editor.save", "File: Save").key("cmd-s"),
        save_active,
    );
    // JetBrains keys for GPUI Kit's editor actions (which have no default binding, or a VS Code one).
    CommandRegistry::register(
        cx,
        CommandSpec::new("editor.go_to_definition", "Navigate: Go to Declaration")
            .key("cmd-b")
            .in_context(pide_core::commands::EDITOR),
        |window, cx| {
            window.dispatch_action(Box::new(gpui_kit::component::input::GoToDefinition), cx)
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("editor.parameter_info", "Code: Parameter Info")
            .key("cmd-p")
            .in_context(pide_core::commands::EDITOR),
        |window, cx| {
            if let Some(panel) = active_panel(window, cx) {
                panel.update(cx, |p, cx| {
                    p.request_signature(signature::Trigger::Invoked, cx)
                });
            }
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("lsp.restart", "Code: Restart Language Server"),
        |window, cx| {
            let Some(panel) = active_panel(window, cx) else {
                return;
            };
            let (path, store) = {
                let p = panel.read(cx);
                (p.path.clone(), p.lsp.clone())
            };
            let Some(store) = store else { return };
            let id = pide_lsp::registry::spec_for(store.read(cx).specs(), &path).map(|s| s.id);
            match id {
                Some(id) => store.update(cx, |s, cx| s.restart(id, cx)),
                None => window.push_notification(
                    Notification::warning("This file has no language server."),
                    cx,
                ),
            }
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("editor.find_usages", "Navigate: Find Usages")
            .key("alt-f7")
            .in_context(pide_core::commands::EDITOR),
        lsp::find_usages,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("editor.file_structure", "Navigate: File Structure")
            .key("cmd-f12")
            .in_context(pide_core::commands::EDITOR),
        symbols::file_structure,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("editor.go_to_symbol", "Navigate: Go to Symbol…").key("cmd-alt-o"),
        symbols::go_to_symbol,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("editor.organize_imports", "Code: Optimize Imports")
            .key("ctrl-alt-o")
            .in_context(pide_core::commands::EDITOR),
        lsp::organize_imports,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("editor.rename", "Refactor: Rename…")
            .key("shift-f6")
            .in_context(pide_core::commands::EDITOR),
        rename::rename_symbol,
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("editor.quick_fix", "Code: Show Quick Fixes")
            .key("alt-enter")
            .in_context(pide_core::commands::EDITOR),
        |window, cx| {
            window.dispatch_action(Box::new(gpui_kit::component::input::ToggleCodeActions), cx)
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("editor.close", "File: Close Tab").key("cmd-w"),
        |window, cx| window.dispatch_action(Box::new(ClosePanel), cx),
    );
}

/// Open `path` in a tab (or switch to its existing tab) and focus it.
pub fn open_path(path: PathBuf, window: &mut Window, cx: &mut App) {
    open_panel(path, window, cx);
}

/// Open `path` and put the cursor at a 0-based line and character column
/// (once the file has loaded, if it's new).
pub fn open_path_at(path: PathBuf, line: u32, character: u32, window: &mut Window, cx: &mut App) {
    let Some(panel) = open_panel(path, window, cx) else {
        return;
    };
    panel.update(cx, |p, cx| p.go_to(line, character, window, cx));
}

fn open_panel(path: PathBuf, window: &mut Window, cx: &mut App) -> Option<Entity<EditorPanel>> {
    let win = window.window_handle().window_id();
    let existing = cx
        .global::<OpenEditors>()
        .0
        .iter()
        .filter_map(|w| w.upgrade())
        .find(|p| p.read(cx).window_id == win && p.read(cx).path == path);
    if let Some(panel) = existing {
        let (group, focus) = {
            let p = panel.read(cx);
            (p.group.clone(), p.focus.clone())
        };
        select_tab_of(group, panel.entity_id(), window, cx);
        focus.focus(window, cx);
        return Some(panel);
    }
    let Some(dock) = host(window, cx) else {
        log::error!("open_path before the dock exists");
        return None;
    };
    let panel = cx.new(|cx| EditorPanel::new(path, window, cx));
    dock.update(cx, |d, cx| {
        d.add_panel_view(
            panel_handle(panel.clone()),
            DockPlacement::Center,
            None,
            window,
            cx,
        )
    });
    let focus = panel.read(cx).focus.clone();
    window.defer(cx, move |window, cx| focus.focus(window, cx));
    Some(panel)
}

/// Make `panel` the selected tab of its group. Call it with no panel being
/// updated: `TabGroup::select_tab` reads every panel in the group, and reading
/// one mid-update panics (this crashed on clicking an already-open file).
pub(crate) fn select_tab_of(
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(group) = group.and_then(|g| g.upgrade()) else {
        return;
    };
    group.update(cx, |g, cx| {
        if let Some(ix) = g
            .panels()
            .iter()
            .position(|p| p.view().entity_id() == panel)
        {
            g.select_tab(ix, window, cx);
        }
    });
}

/// The editor tab showing this window's active buffer.
pub(crate) fn active_panel(window: &Window, cx: &App) -> Option<Entity<EditorPanel>> {
    let win = window.window_handle().window_id();
    let active = Services::for_window(win, cx)?
        .workspace
        .read(cx)
        .active()?
        .clone();
    cx.global::<OpenEditors>()
        .0
        .iter()
        .filter_map(|w| w.upgrade())
        .find(|p| p.read(cx).window_id == win && p.read(cx).buffer.as_ref() == Some(&active))
}

/// The active editor's file and selection, for "send editor context".
fn editor_context(window: &Window, cx: &App) -> Option<pide_core::context::EditorContext> {
    use gpui_kit::component::input::RopeExt as _;
    let services = Services::for_window(window.window_handle().window_id(), cx)?;
    let ws = services.workspace.read(cx);
    let active = ws.active()?.clone();
    let panel = cx
        .global::<OpenEditors>()
        .0
        .iter()
        .filter_map(|w| w.upgrade())
        .find(|p| p.read(cx).buffer.as_ref() == Some(&active))?;
    let panel = panel.read(cx);
    let state = panel.editor.read(cx);
    let range = state.selected_range();
    let text = state.text();
    let pos = |offset: usize| {
        let p = text.offset_to_position(offset);
        (p.line + 1, p.character + 1)
    };
    let selected_bytes = range.end - range.start;
    let selected = (selected_bytes > 0
        && selected_bytes <= pide_core::context::MAX_SELECTION_BYTES)
        .then(|| state.selected_text().to_string());
    Some(pide_core::context::EditorContext {
        path: panel
            .path
            .strip_prefix(ws.root())
            .unwrap_or(&panel.path)
            .to_path_buf(),
        start: pos(range.start),
        end: pos(range.end),
        selected,
        selected_bytes,
        language: language_for(&panel.path),
    })
}

fn save_active(window: &mut Window, cx: &mut App) {
    let workspace = Services::of(window, cx).workspace;
    let Some(buffer) = workspace.read(cx).active().cloned() else {
        return;
    };
    let task = workspace.update(cx, |w, cx| w.save(&buffer, cx));
    window
        .spawn(cx, async move |cx| {
            if let Err(e) = task.await {
                log::error!("save failed: {e:#}");
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(format!("Save failed: {e:#}")), cx)
                });
            }
        })
        .detach();
}

/// GPUI Kit language name for a file, by extension.
pub(crate) fn language_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
    {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "md" | "markdown" => "markdown",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        _ => "text",
    }
}

pub struct EditorPanel {
    path: PathBuf,
    /// The project window this tab lives in.
    window_id: WindowId,
    buffer: Option<Entity<Buffer>>,
    editor: Entity<EditorState>,
    workspace: Entity<Workspace>,
    focus: FocusHandle,
    group: Option<WeakEntity<TabGroup>>,
    applied_revision: u64,
    /// The dock's active tab in its group. `set_active` can arrive before the file has loaded.
    is_active: bool,
    load_error: Option<SharedString>,
    deleted: bool,
    diff: Option<Entity<DiffState>>,
    /// The project's language servers (set once the file is attached).
    lsp: Option<Entity<pide_lsp::LspStore>>,
    providers: Option<std::rc::Rc<lsp::Providers>>,
    /// Line changes since the server last published diagnostics for this file.
    diag_edits: Vec<follow::LineEdit>,
    /// Where to put the cursor once the file has loaded (go to definition, problems).
    pending_cursor: Option<(u32, u32)>,
    /// Signature help being shown, and the line it was asked for on.
    signature: Option<lsp_types::SignatureHelp>,
    signature_line: u32,
    _signature_task: Option<Task<()>>,
    /// Git: the file's text at HEAD, and the gutter marks against it.
    git_base: git_gutter::Base,
    gutter: Option<gpui_kit::component::input::GutterDecorationCollection>,
    _git_task: Option<Task<()>>,
    _git_marks: Option<Task<()>>,
    _subs: Vec<Subscription>,
    _load: Option<Task<()>>,
}

impl EventEmitter<PanelEvent> for EditorPanel {}

impl EditorPanel {
    fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let workspace = Services::of(window, cx).workspace;
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language(language_for(&path).to_string())
                .line_number(true)
                .indent_guides(true)
                .tab_size(TabSize {
                    tab_size: 4,
                    hard_tabs: false,
                })
                .soft_wrap(false);
            state.set_readonly(true, cx); // until loaded
            state
        });
        let focus = editor.focus_handle(cx);

        let mut subs = vec![
            cx.subscribe(&editor, |this, editor, ev: &InputEvent, cx| match ev {
                InputEvent::Change => {
                    if let Some(buffer) = this.buffer.clone() {
                        let text = editor.read(cx).text().clone();
                        this.workspace
                            .update(cx, |w, cx| w.edited(&buffer, text, cx));
                    }
                    this.signature_after_edit(cx);
                    this.refresh_git_marks(std::time::Duration::from_millis(250), cx);
                }
                InputEvent::Blur => this.hide_signature(cx),
                _ => {}
            }),
            cx.subscribe_in(&workspace, window, Self::on_workspace_event),
        ];
        let weak = cx.weak_entity();
        cx.global_mut::<OpenEditors>().0.push(weak);

        let mut this = Self {
            path: path.clone(),
            window_id: window.window_handle().window_id(),
            buffer: None,
            editor,
            workspace: workspace.clone(),
            focus,
            group: None,
            applied_revision: 0,
            is_active: false,
            load_error: None,
            deleted: false,
            diff: None,
            lsp: None,
            providers: None,
            diag_edits: Vec::new(),
            pending_cursor: None,
            signature: None,
            signature_line: 0,
            _signature_task: None,
            git_base: Default::default(),
            gutter: None,
            _git_task: None,
            _git_marks: None,
            _subs: Vec::new(),
            _load: None,
        };

        if path.as_os_str().is_empty() {
            this.load_error = Some("This tab's file is unknown (bad saved layout).".into());
        } else {
            let open = workspace.update(cx, |w, cx| w.open(path, cx));
            this._load = Some(cx.spawn_in(window, async move |this, cx| {
                let result = open.await;
                let _ = this.update_in(cx, |this, window, cx| match result {
                    Ok(buffer) => this.attach(buffer, window, cx),
                    Err(e) => {
                        this.load_error = Some(format!("Can't open: {e:#}").into());
                        cx.notify();
                    }
                });
            }));
        }
        this._subs.append(&mut subs);
        this
    }

    fn attach(&mut self, buffer: Entity<Buffer>, window: &mut Window, cx: &mut Context<Self>) {
        let (text, read_only, revision, path) = {
            let b = buffer.read(cx);
            (
                b.text().to_string(),
                b.read_only.is_some(),
                b.revision(),
                b.path.clone(),
            )
        };
        self.path = path;
        self.applied_revision = revision;
        self.editor.update(cx, |e, cx| {
            e.set_value(text, window, cx);
            e.set_readonly(read_only, cx);
        });
        // Re-render (tab title, bars) whenever the buffer changes.
        self._subs.push(cx.observe(&buffer, |_, _, cx| cx.notify()));
        self.buffer = Some(buffer.clone());
        if self.is_active {
            self.workspace
                .update(cx, |w, cx| w.set_active(Some(buffer), cx));
        }
        self.attach_lsp(window, cx);
        self.load_git_base(cx);
        if let Some((line, character)) = self.pending_cursor.take() {
            self.go_to(line, character, window, cx);
        }
        cx.emit(PanelEvent::LayoutChanged); // path is now known for dump()
        cx.notify();
    }

    /// Put the cursor at a 0-based line and character column (or once loaded).
    pub fn go_to(
        &mut self,
        line: u32,
        character: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.buffer.is_none() {
            self.pending_cursor = Some((line, character));
            return;
        }
        self.editor.update(cx, |e, cx| {
            e.set_cursor_position(
                gpui_kit::component::input::Position::new(line, character),
                window,
                cx,
            )
        });
    }

    fn attach_lsp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = pide_lsp::LspStore::for_workspace(&self.workspace, cx);
        self._subs
            .push(cx.subscribe_in(&store, window, |this, store, ev, _, cx| {
                if let pide_lsp::LspEvent::Diagnostics { path } = ev {
                    if *path == this.path {
                        this.diag_edits.clear();
                        this.apply_diagnostics(store, cx);
                    }
                }
            }));
        self.apply_diagnostics(&store, cx);
        if pide_lsp::registry::spec_for(store.read(cx).specs(), &self.path).is_some() {
            let providers = lsp::Providers::new(&store, self.path.clone());
            let show = lsp::show_document_handler(self.path.clone());
            self.providers = Some(providers.clone());
            self._subs
                .push(cx.subscribe_in(&self.editor, window, Self::on_editor_edits));
            self.editor.update(cx, |e, _| {
                let l = e.lsp_mut();
                l.completion_provider = Some(providers.clone());
                l.hover_provider = Some(providers.clone());
                l.definition_provider = Some(providers.clone());
                l.code_action_providers = vec![providers];
                l.show_document = Some(show);
            });
        }
        self.lsp = Some(store);
    }

    /// A completion was just inserted: add its extra edits (auto-import), which
    /// GPUI Kit leaves out.
    fn on_editor_edits(
        &mut self,
        editor: &Entity<EditorState>,
        ev: &InputEditsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lines: Vec<follow::LineEdit> = ev
            .edits()
            .iter()
            .map(|(e, _)| follow::LineEdit {
                start: e.start_position.row as u32,
                old_end: e.old_end_position.row as u32,
                new_end: e.new_end_position.row as u32,
                at_line_start: e.start_position.column == 0 && e.old_end_byte == e.start_byte,
            })
            .filter(|l| l.old_end != l.new_end)
            .collect();
        if !lines.is_empty() {
            self.diag_edits.extend(lines);
            if let Some(store) = self.lsp.clone() {
                self.apply_diagnostics(&store, cx);
            }
        }
        let Some(p) = self.providers.clone() else {
            return;
        };
        let Some(raw) = p.completion_accepted(ev) else {
            return;
        };
        let Some(enc) = p.server_encoding(cx) else {
            return;
        };
        let task = p.extra_edits(raw, cx);
        let editor = editor.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let edits = task.await;
            if edits.is_empty() {
                return;
            }
            let _ = cx.update(|window, cx| {
                let Some(editor) = editor.upgrade() else {
                    return;
                };
                editor.update(cx, |e, cx| {
                    let text = e.text().clone();
                    let mut edits: Vec<lsp_types::TextEdit> = edits
                        .into_iter()
                        .map(|mut t| {
                            t.range = pide_lsp::convert::range_from_server(&text, t.range, enc);
                            t
                        })
                        .collect();
                    // Last first, so earlier positions stay valid.
                    edits.sort_by(|a, b| {
                        (b.range.start.line, b.range.start.character)
                            .cmp(&(a.range.start.line, a.range.start.character))
                    });
                    // Keep the caret after the completion: shift it by what
                    // the edits above it added (GPUI Kit leaves it at the last edit).
                    use gpui_kit::component::input::RopeExt as _;
                    let cursor = e.cursor();
                    let shift: isize = edits
                        .iter()
                        .map(|t| {
                            (
                                text.position_to_offset(&t.range.start),
                                text.position_to_offset(&t.range.end),
                                t.new_text.len(),
                            )
                        })
                        .filter(|(_, end, _)| *end <= cursor)
                        .map(|(start, end, len)| len as isize - (end - start) as isize)
                        .sum();
                    e.apply_lsp_edits(&edits, window, cx);
                    let at = e
                        .text()
                        .offset_to_position((cursor as isize + shift).max(0) as usize);
                    e.set_cursor_position(at, window, cx);
                });
            });
        })
        .detach();
    }

    /// Show the server's diagnostics for this file (converted to the editor's columns).
    fn apply_diagnostics(&mut self, store: &Entity<pide_lsp::LspStore>, cx: &mut Context<Self>) {
        let diags = store.read(cx).diagnostics_for(&self.path).cloned();
        let shifts = self.diag_edits.clone();
        self.editor.update(cx, |e, cx| {
            let text = e.text().clone();
            let Some(set) = e.diagnostics_mut() else {
                return;
            };
            set.reset(&text);
            for mut d in diags
                .into_iter()
                .flat_map(|f| f.items.into_iter().map(move |i| (f.encoding, i)))
                .map(|(enc, mut i)| {
                    i.range.start.line = follow::shift_line(i.range.start.line, &shifts);
                    i.range.end.line = follow::shift_line(i.range.end.line, &shifts);
                    i.range = pide_lsp::convert::range_from_server(&text, i.range, enc);
                    i
                })
            {
                d.message = d.message.trim().to_string();
                set.push(d);
            }
            cx.notify();
        });
    }

    fn is_mine(&self, buffer: &Entity<Buffer>) -> bool {
        self.buffer.as_ref() == Some(buffer)
    }

    fn on_workspace_event(
        &mut self,
        _: &Entity<Workspace>,
        ev: &WorkspaceEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            WorkspaceEvent::FileChangedOnDisk {
                buffer,
                reloaded: true,
            } if self.is_mine(buffer) => {
                let b = buffer.read(cx);
                if b.revision() == self.applied_revision {
                    return;
                }
                self.applied_revision = b.revision();
                let text = b.text().to_string();
                let read_only = b.read_only.is_some();
                self.diff = None;
                self.deleted = false;
                self.editor.update(cx, |e, cx| {
                    let cursor = e.cursor_position();
                    e.set_value(text, window, cx);
                    e.set_readonly(read_only, cx);
                    e.set_cursor_position(cursor, window, cx);
                });
                self.load_git_base(cx);
                cx.notify();
            }
            WorkspaceEvent::BufferSaved(buffer) if self.is_mine(buffer) => {
                // A commit may have happened since the file was opened.
                self.load_git_base(cx);
            }
            WorkspaceEvent::TextReplaced(buffer) if self.is_mine(buffer) => {
                let text = buffer.read(cx).text().to_string();
                self.editor.update(cx, |e, cx| {
                    if e.text().to_string() == text {
                        return;
                    }
                    use gpui_kit::component::input::RopeExt as _;
                    let rope = e.text().clone();
                    let end = rope.offset_to_position(rope.len());
                    let edit = lsp_types::TextEdit {
                        range: lsp_types::Range::new(lsp_types::Position::new(0, 0), end),
                        new_text: text,
                    };
                    e.apply_lsp_edits(&vec![edit], window, cx);
                });
            }
            WorkspaceEvent::FileDeleted(buffer) if self.is_mine(buffer) => {
                self.deleted = true;
                cx.notify();
            }
            _ => {}
        }
    }

    fn reload(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(b) = self.buffer.clone() {
            self.workspace
                .update(cx, |w, cx| w.reload(&b, cx))
                .detach_and_log_err(cx);
        }
    }

    fn keep_mine(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(b) = self.buffer.clone() {
            self.workspace.update(cx, |w, cx| w.keep_mine(&b, cx));
        }
        self.diff = None;
    }

    fn toggle_diff(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.diff.take().is_some() {
            cx.notify();
            return;
        }
        let Some(buffer) = self.buffer.clone() else {
            return;
        };
        let mine = buffer.read(cx).text().to_string();
        let name = buffer.read(cx).file_name();
        let language = language_for(&self.path);
        let disk = self.workspace.read(cx).read_disk(&buffer, cx);
        cx.spawn_in(window, async move |this, cx| {
            let disk = disk.await?;
            let patch = similar::TextDiff::from_lines(&disk, &mine)
                .unified_diff()
                .context_radius(3)
                .header(&format!("a/{name} (on disk)"), &format!("b/{name} (yours)"))
                .to_string();
            let files = DiffFile::parse(&patch).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            this.update(cx, |this, cx| {
                let files = files.into_iter().map(|f| f.with_language(language));
                this.diff = Some(cx.new(|cx| DiffState::new(files, cx)));
                cx.notify();
            })
        })
        .detach_and_log_err(cx);
    }

    fn bar(&self, cx: &App, error: bool, text: impl Into<SharedString>) -> Div {
        let (bg, fg) = if error {
            (cx.theme().danger, cx.theme().danger_foreground)
        } else {
            (cx.theme().warning, cx.theme().warning_foreground)
        };
        h_flex()
            .w_full()
            .gap_2()
            .px_3()
            .py_1()
            .text_sm()
            .bg(bg)
            .text_color(fg)
            .child(div().flex_1().child(text.into()))
    }
}

impl Focusable for EditorPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui_kit::component::dock::BasePanel for EditorPanel {
    fn panel_name(&self) -> &'static str {
        PANEL_NAME
    }

    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.is_active = active;
        if active {
            let buffer = self.buffer.clone();
            self.workspace.update(cx, |w, cx| w.set_active(buffer, cx));
        }
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity_id();
        cx.global_mut::<OpenEditors>()
            .0
            .retain(|w| w.upgrade().is_some_and(|p| p.entity_id() != me));
        // Keep a dirty buffer open in the workspace so reopening the file keeps the edits.
        if let Some(b) = &self.buffer {
            if !b.read(cx).is_dirty() {
                let b = b.clone();
                self.workspace.update(cx, |w, cx| w.close(&b, cx));
            }
        }
    }

    fn dump(&self, _: &App) -> PanelState {
        let mut state = PanelState::new(PANEL_NAME);
        state.info = PanelInfo::panel(serde_json::json!({ "path": self.path }));
        state
    }
}

impl gpui_kit::component::dock::Panel for EditorPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dirty = self.buffer.as_ref().is_some_and(|b| b.read(cx).is_dirty());
        h_flex()
            .gap_1()
            .child(self.title_text(cx))
            .when(dirty, |this| {
                this.child(div().text_color(cx.theme().warning).child("●"))
            })
    }
}

impl Panel for EditorPanel {
    fn title_text(&self, _: &App) -> SharedString {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".into())
            .into()
    }

    fn default_dock(&self) -> DockPosition {
        DockPosition::Center
    }
}

impl Render for EditorPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(err) = &self.load_error {
            return v_flex().size_full().child(self.bar(cx, true, err.clone()));
        }
        let buffer = self.buffer.as_ref().map(|b| b.read(cx));
        let read_only = buffer.and_then(|b| b.read_only.clone());
        let notice = buffer.and_then(|b| b.notice.clone());
        let conflict = buffer.is_some_and(|b| b.has_conflict());
        let deleted = self.deleted;

        v_flex()
            .size_full()
            // Escape closes signature help (seen before the editor handles it; not consumed).
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.hide_signature(cx);
                }
            }))
            .when_some(read_only, |this, r| this.child(self.bar(cx, false, r)))
            .when_some(notice, |this, n| this.child(self.bar(cx, false, n)))
            .when(deleted, |this| {
                this.child(self.bar(
                    cx,
                    true,
                    "This file was deleted on disk. Saving will create it again.",
                ))
            })
            .when(conflict, |this| {
                this.child(
                    self.bar(
                        cx,
                        true,
                        "This file changed on disk, and you have unsaved changes.",
                    )
                    .child(
                        Button::new("reload")
                            .xsmall()
                            .label("Reload")
                            .on_click(cx.listener(Self::reload)),
                    )
                    .child(
                        Button::new("keep")
                            .xsmall()
                            .label("Keep mine")
                            .on_click(cx.listener(Self::keep_mine)),
                    )
                    .child(
                        Button::new("diff")
                            .xsmall()
                            .ghost()
                            .label(if self.diff.is_some() {
                                "Hide diff"
                            } else {
                                "Show diff"
                            })
                            .on_click(cx.listener(Self::toggle_diff)),
                    ),
                )
            })
            .when_some(self.diff.clone(), |this, diff| {
                this.child(
                    div()
                        .h(relative(0.4))
                        .w_full()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(Diff::new(&diff).line_number(true).syntax_highlight(true)),
                )
            })
            .children(self.render_signature(cx))
            .child(
                div().flex_1().w_full().overflow_hidden().child(
                    Editor::new(&self.editor)
                        .bordered(false)
                        .p_0()
                        .h_full()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(cx.theme().mono_font_size),
                ),
            )
    }
}

/// `PIDE_BENCH=<file>`: open the file, scroll through it and type in it via the
/// editor API, log GPUI's frame timings, then quit. For checking M1's frame budgets
/// in the real app (with docks, tree and chat around the editor).
pub fn run_benchmark(path: PathBuf, window: &mut Window, cx: &mut App) {
    open_path(path.clone(), window, cx);
    window
        .spawn(cx, async move |cx| {
            use std::time::{Duration, Instant};
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let Ok(Some(editor)) = cx.update(|window, cx| {
                let win = window.window_handle().window_id();
                cx.global::<OpenEditors>()
                    .0
                    .iter()
                    .filter_map(|w| w.upgrade())
                    .find(|p| p.read(cx).window_id == win && p.read(cx).path.ends_with(path.file_name().unwrap_or_default()))
                    .map(|p| p.read(cx).editor.clone())
            }) else {
                log::error!("bench: editor not found");
                return;
            };
            set_trace_enabled(true);
            let mut collector = FrameTimingCollector::new();
            let report = |name: &str, collector: &mut FrameTimingCollector| {
                let mut draws: Vec<f64> = collector
                    .collect_unseen()
                    .into_iter()
                    .filter_map(|e| match e {
                        FrameEvent::Draw(t) => Some(t.draw_end.duration_since(t.draw_start).as_secs_f64() * 1e3),
                        _ => None,
                    })
                    .collect();
                draws.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let n = draws.len().max(1);
                let p = |q: f64| draws.get(((n - 1) as f64 * q) as usize).copied().unwrap_or(0.0);
                log::info!(
                    "bench {name}: frames {} | draw p50 {:.2} p95 {:.2} max {:.2} ms | >16ms {} | >32ms {}",
                    draws.len(),
                    p(0.5),
                    p(0.95),
                    draws.last().copied().unwrap_or(0.0),
                    draws.iter().filter(|d| **d > 16.0).count(),
                    draws.iter().filter(|d| **d > 32.0).count()
                );
            };
            let line_h = cx.update(|_, cx| editor.read(cx).line_height()).ok().flatten().unwrap_or(px(20.));
            let lines = cx.update(|_, cx| editor.read(cx).text().to_string().lines().count()).unwrap_or(0);

            collector.collect_unseen();
            for i in 0..(lines / 60) {
                let y = line_h * (60.0 * i as f32);
                let _ = cx.update(|_, cx| editor.update(cx, |e, cx| e.set_scroll_offset(point(px(0.), -y), cx)));
                cx.background_executor().timer(Duration::from_millis(16)).await;
            }
            report("fast scroll (60 lines/frame, whole file)", &mut collector);

            let _ = cx.update(|window, cx| {
                editor.update(cx, |e, cx| e.set_cursor_position(gpui_kit::component::input::Position::new((lines / 2) as u32, 0), window, cx))
            });
            cx.background_executor().timer(Duration::from_millis(300)).await;
            collector.collect_unseen();
            let started = Instant::now();
            for ch in "let bench = 42; // typing in the middle\n".chars().cycle().take(200) {
                let _ = cx.update(|window, cx| editor.update(cx, |e, cx| e.insert(ch.to_string(), window, cx)));
                cx.background_executor().timer(Duration::from_millis(16)).await;
            }
            report("typing (200 chars mid-file)", &mut collector);
            log::info!("bench: done in {:.1} s", started.elapsed().as_secs_f64());
            let _ = cx.update(|_, cx| cx.quit());
        })
        .detach();
}
