//! GPUI Kit editor providers backed by the project's language server (spec M3):
//! completion, hover, go to definition (into other files too) and code actions.
//! Every position crosses `pide_lsp::convert`.

use std::cell::RefCell;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Result;
use gpui_kit::component::input::{
    CodeActionProvider, CompletionProvider, DefinitionProvider, EditorState, HoverProvider,
    InputEditsEvent, Rope, RopeExt as _,
};
use gpui_kit::*;
use lsp_types::{
    CodeAction, CodeActionOrCommand, CompletionContext, CompletionItem, CompletionResponse,
    CompletionTextEdit, GotoDefinitionResponse, Hover, LocationLink, TextEdit, WorkspaceEdit,
};
use pide_lsp::LspStore;
use pide_lsp::convert::{Encoding, TextLines, offset_to_server, range_from_server};
use pide_lsp::process::{file_uri, uri_path};
use serde_json::{Value, json};

pub(crate) struct Providers {
    store: WeakEntity<LspStore>,
    path: PathBuf,
    /// An add-on server (spell checking) by id, for its code actions only;
    /// `None`: the file's main server.
    server: Option<&'static str>,
    /// The last completion list: (as given to GPUI Kit, as the server sent it).
    /// GPUI Kit applies only an item's main edit; we spot the insertion and
    /// add its `additionalTextEdits` (auto-import) ourselves.
    last_completions: Rc<RefCell<Vec<(CompletionItem, CompletionItem)>>>,
}

impl Providers {
    pub(crate) fn new(store: &Entity<LspStore>, path: PathBuf) -> Rc<Self> {
        Rc::new(Self {
            store: store.downgrade(),
            path,
            server: None,
            last_completions: Rc::default(),
        })
    }

    /// Code actions from an add-on server (the spell checker's fixes).
    pub(crate) fn addon(store: &Entity<LspStore>, path: PathBuf, server: &'static str) -> Rc<Self> {
        Rc::new(Self {
            store: store.downgrade(),
            path,
            server: Some(server),
            last_completions: Rc::default(),
        })
    }

    /// This provider's server, if it's running.
    fn server<'a>(&self, store: &'a LspStore) -> Option<&'a pide_lsp::store::Server> {
        match self.server {
            Some(id) => store.running(id),
            None => store.server_for(&self.path),
        }
    }

    fn encoding(&self, cx: &App) -> Option<Encoding> {
        let store = self.store.upgrade()?;
        self.server(store.read(cx)).map(|s| s.encoding)
    }

    fn request(
        &self,
        text: &Rope,
        method: &str,
        params: Value,
        cx: &mut App,
    ) -> Task<Result<Value>> {
        self.request_with(Some(text), method, params, cx)
    }

    /// `text`: the editor's current text, synced to the server first. `None`
    /// when the caller can't read the editor (GPUI Kit calls some providers
    /// from inside the editor's own update; reading it there panics).
    fn request_with(
        &self,
        text: Option<&Rope>,
        method: &str,
        params: Value,
        cx: &mut App,
    ) -> Task<Result<Value>> {
        let Some(store) = self.store.upgrade() else {
            return Task::ready(Err(anyhow::anyhow!("project closed")));
        };
        store.update(cx, |s, cx| match self.server {
            Some(id) => s.request_to(id, &self.path, text, method, params, cx),
            None => s.request(&self.path, text, method, params, cx),
        })
    }

    fn trigger_characters(&self, cx: &App) -> Vec<String> {
        self.store
            .upgrade()
            .and_then(|s| {
                s.read(cx)
                    .server_for(&self.path)
                    .and_then(|s| s.capabilities.as_ref())
                    .and_then(|c| c.completion_provider.as_ref())
                    .and_then(|c| c.trigger_characters.clone())
            })
            .unwrap_or_default()
    }

    /// Commands the server executes itself (`executeCommandProvider`).
    fn server_commands(&self, cx: &App) -> Vec<String> {
        self.store
            .upgrade()
            .and_then(|s| {
                self.server(s.read(cx))
                    .and_then(|s| s.capabilities.as_ref())
                    .and_then(|c| c.execute_command_provider.as_ref())
                    .map(|e| e.commands.clone())
            })
            .unwrap_or_default()
    }

    fn doc(&self) -> Value {
        json!({ "uri": file_uri(&self.path) })
    }

    /// The server's form of the completion item whose main edit `ev` just
    /// applied, if any (GPUI Kit inserts it; we only watch).
    pub(crate) fn completion_accepted(&self, ev: &InputEditsEvent) -> Option<CompletionItem> {
        let last = self.last_completions.borrow();
        let (edit, after) = ev.edits().last()?;
        let inserted = after.slice(edit.start_byte..edit.new_end_byte).to_string();
        let found = last.iter().find(|(shown, _)| match &shown.text_edit {
            Some(CompletionTextEdit::Edit(e)) => {
                e.new_text == inserted
                    && after.position_to_offset(&e.range.start) == edit.start_byte
            }
            Some(CompletionTextEdit::InsertAndReplace(e)) => {
                e.new_text == inserted
                    && after.position_to_offset(&e.replace.start) == edit.start_byte
            }
            // No range: the inserted text is all we can match on; skip one-letter items.
            None => {
                let t = shown.insert_text.as_deref().unwrap_or(&shown.label);
                t.chars().count() > 1 && t == inserted
            }
        })?;
        let raw = found.1.clone();
        drop(last);
        // One acceptance per list: a later undo/redo or reload that inserts the
        // same text must not add the import again.
        self.last_completions.borrow_mut().clear();
        Some(raw)
    }

    /// The accepted item's `additionalTextEdits` (asking the server to resolve
    /// it when the list didn't carry them), in server positions.
    pub(crate) fn extra_edits(&self, raw: CompletionItem, cx: &mut App) -> Task<Vec<TextEdit>> {
        if let Some(edits) = raw.additional_text_edits.clone().filter(|e| !e.is_empty()) {
            return Task::ready(edits);
        }
        let resolves = self
            .store
            .upgrade()
            .and_then(|s| {
                s.read(cx)
                    .server_for(&self.path)
                    .and_then(|s| s.capabilities.as_ref())
                    .and_then(|c| c.completion_provider.as_ref())
                    .and_then(|c| c.resolve_provider)
            })
            .unwrap_or(false);
        // `data` exists only to be sent back on resolve (rust-analyzer's
        // import-on-the-fly items carry it even when resolveProvider isn't reported).
        if !resolves && raw.data.is_none() {
            return Task::ready(vec![]);
        }
        let Ok(params) = serde_json::to_value(&raw) else {
            return Task::ready(vec![]);
        };
        // The editor already has the main edit; the server sees it on the next sync.
        let task = self.request_with(None, "completionItem/resolve", params, cx);
        cx.background_executor().spawn(async move {
            task.await
                .ok()
                .and_then(|v| serde_json::from_value::<CompletionItem>(v).ok())
                .and_then(|i| i.additional_text_edits)
                .unwrap_or_default()
        })
    }

    pub(crate) fn server_encoding(&self, cx: &App) -> Option<Encoding> {
        self.encoding(cx)
    }
}

/// A server error that just means "stale, ask again" is an empty answer, not a failure.
fn quiet<T: Default>(r: Result<T>) -> Result<T> {
    match r {
        Err(e)
            if e.downcast_ref::<pide_lsp::rpc::RpcError>()
                .is_some_and(pide_lsp::rpc::is_stale) =>
        {
            Ok(T::default())
        }
        other => other,
    }
}

impl CompletionProvider for Providers {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let Some(enc) = self.encoding(cx) else {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        };
        // `offset` is the cursor. GPUI Kit puts the word typed since the menu
        // opened in `trigger_character`; the server wants a trigger character
        // only when one was really typed. Servers leave filtering to the
        // client, and GPUI Kit shows the list as given, so we rank it.
        let query = trigger.trigger_character.clone().unwrap_or_default();
        let cursor = offset;
        let triggers = self.trigger_characters(cx);
        let context = if !query.is_empty() && triggers.contains(&query) {
            json!({ "triggerKind": 2, "triggerCharacter": query })
        } else {
            json!({ "triggerKind": 1 })
        };
        let word = if triggers.contains(&query) {
            String::new()
        } else {
            query
        };
        let params = json!({
            "textDocument": self.doc(),
            "position": offset_to_server(text, cursor, enc),
            "context": context,
        });
        let task = self.request(text, "textDocument/completion", params, cx);
        let rope = text.clone();
        let converted = cx.background_executor().spawn(async move {
            type Out = (CompletionResponse, Vec<(CompletionItem, CompletionItem)>);
            let v = match quiet(task.await.map(Some)) {
                Ok(Some(v)) if !v.is_null() => v,
                Ok(_) => return Ok((CompletionResponse::Array(vec![]), vec![])),
                Err(e) => return Err(e),
            };
            let mut resp: CompletionResponse = serde_json::from_value(v)?;
            let items = match &mut resp {
                CompletionResponse::Array(items) => items,
                CompletionResponse::List(list) => &mut list.items,
            };
            rank_completions(items, &word);
            let raw: Vec<CompletionItem> = items.clone();
            for item in items.iter_mut() {
                match item.text_edit.as_mut() {
                    Some(CompletionTextEdit::Edit(e)) => {
                        e.range = range_from_server(&rope, e.range, enc)
                    }
                    Some(CompletionTextEdit::InsertAndReplace(e)) => {
                        e.insert = range_from_server(&rope, e.insert, enc);
                        e.replace = range_from_server(&rope, e.replace, enc);
                    }
                    None => {}
                }
            }
            let shown = match &resp {
                CompletionResponse::Array(items) => items.clone(),
                CompletionResponse::List(list) => list.items.clone(),
            };
            Ok::<Out, anyhow::Error>((resp, shown.into_iter().zip(raw).collect()))
        });
        let last = self.last_completions.clone();
        cx.spawn(async move |_| {
            let (resp, pairs) = converted.await?;
            // Both forms, so an accepted item can be resolved and its extra
            // edits applied (`completion_accepted`).
            *last.borrow_mut() = pairs;
            Ok(resp)
        })
    }

    // SPIKE (spike-ghost-rows): the suggestion as an inline completion.
    fn inline_completion(
        &self,
        _rope: &Rope,
        offset: usize,
        _trigger: lsp_types::InlineCompletionContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<Result<lsp_types::InlineCompletionResponse>> {
        let items: Vec<_> = crate::spike_ghost::item_for(&self.path, offset).into_iter().collect();
        Task::ready(Ok(lsp_types::InlineCompletionResponse::Array(items)))
    }

    fn inline_completion_debounce(&self) -> std::time::Duration {
        std::time::Duration::from_millis(if crate::spike_ghost::active() { 20 } else { 300 })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, cx: &mut App) -> bool {
        let Some(c) = new_text.chars().last() else {
            return false;
        };
        if c.is_alphanumeric() || c == '_' {
            return true;
        }
        self.trigger_characters(cx).iter().any(|t| t.ends_with(c))
    }
}

impl HoverProvider for Providers {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let Some(enc) = self.encoding(cx) else {
            return Task::ready(Ok(None));
        };
        let params =
            json!({ "textDocument": self.doc(), "position": offset_to_server(text, offset, enc) });
        let task = self.request(text, "textDocument/hover", params, cx);
        let rope = text.clone();
        cx.background_executor().spawn(async move {
            let v = quiet(task.await.map(Some))?.unwrap_or(Value::Null);
            if v.is_null() {
                return Ok(None);
            }
            let mut hover: Hover = serde_json::from_value(v)?;
            hover.range = hover.range.map(|r| range_from_server(&rope, r, enc));
            Ok(Some(hover))
        })
    }
}

impl DefinitionProvider for Providers {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        let Some(enc) = self.encoding(cx) else {
            return Task::ready(Ok(vec![]));
        };
        let params =
            json!({ "textDocument": self.doc(), "position": offset_to_server(text, offset, enc) });
        let task = self.request(text, "textDocument/definition", params, cx);
        let rope = text.clone();
        let me = self.path.clone();
        cx.background_executor().spawn(async move {
            let v = quiet(task.await.map(Some))?.unwrap_or(Value::Null);
            if v.is_null() {
                return Ok(vec![]);
            }
            let links = match serde_json::from_value::<GotoDefinitionResponse>(v)? {
                GotoDefinitionResponse::Scalar(l) => vec![link(l)],
                GotoDefinitionResponse::Array(ls) => ls.into_iter().map(link).collect(),
                GotoDefinitionResponse::Link(ls) => ls,
            };
            Ok(links
                .into_iter()
                .map(|mut l| {
                    l.origin_selection_range = l
                        .origin_selection_range
                        .map(|r| range_from_server(&rope, r, enc));
                    // Ranges in other files are converted when that file is open (`show_document`).
                    if uri_path(l.target_uri.as_str()).as_deref() == Some(me.as_path()) {
                        l.target_range = range_from_server(&rope, l.target_range, enc);
                        l.target_selection_range =
                            range_from_server(&rope, l.target_selection_range, enc);
                    }
                    l
                })
                .collect())
        })
    }
}

/// Keep the items whose filter text contains the typed word's characters in
/// order (case-insensitive), best first: prefix matches, then the server's
/// `sortText` order. GPUI Kit shows the list as given.
fn rank_completions(items: &mut Vec<lsp_types::CompletionItem>, word: &str) {
    let word = word.to_lowercase();
    let key = |i: &lsp_types::CompletionItem| {
        i.filter_text
            .clone()
            .unwrap_or_else(|| i.label.clone())
            .to_lowercase()
    };
    let subsequence = |hay: &str| {
        let mut chars = hay.chars();
        word.chars().all(|w| chars.any(|h| h == w))
    };
    items.retain(|i| word.is_empty() || subsequence(&key(i)));
    items.sort_by_cached_key(|i| {
        let k = key(i);
        (
            !k.starts_with(&word),
            i.sort_text.clone().unwrap_or_else(|| i.label.clone()),
        )
    });
}

fn link(l: lsp_types::Location) -> LocationLink {
    LocationLink {
        origin_selection_range: None,
        target_uri: l.uri,
        target_range: l.range,
        target_selection_range: l.range,
    }
}

/// Go to definition in another file: open it in pide (the editor would only jump inside its own file).
pub(crate) fn show_document_handler(
    me: PathBuf,
) -> gpui_kit::component::input::ShowDocumentHandler {
    Rc::new(
        move |params: &lsp_types::ShowDocumentParams, window: &mut Window, cx: &mut App| {
            let Some(path) = uri_path(params.uri.as_str()) else {
                return false;
            };
            if path == me {
                return false; // same file: the editor moves the cursor itself
            }
            // The column is the server's: exact for UTF-32 servers (rust-analyzer), and
            // for UTF-16 ones on lines without characters outside the BMP.
            let pos = params.selection.map(|r| r.start).unwrap_or_default();
            window.defer(cx, move |window, cx| {
                pide_core::opener::open_file_at(path, pos.line, pos.character, window, cx)
            });
            true
        },
    )
}

impl CodeActionProvider for Providers {
    /// GPUI Kit runs a chosen action through the provider with this id.
    fn id(&self) -> SharedString {
        match self.server {
            Some(id) => format!("pide-lsp:{id}").into(),
            None => "pide-lsp".into(),
        }
    }

    fn code_actions(
        &self,
        state: Entity<EditorState>,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeAction>>> {
        let Some(enc) = self.encoding(cx) else {
            return Task::ready(Ok(vec![]));
        };
        let rope = state.read(cx).text().clone();
        let start = offset_to_server(&rope, range.start, enc);
        let end = offset_to_server(&rope, range.end, enc);
        // The server's own diagnostics on these lines (still in its encoding).
        let diagnostics: Vec<lsp_types::Diagnostic> = self
            .store
            .upgrade()
            .and_then(|s| {
                let s = s.read(cx);
                match self.server {
                    Some(id) => s
                        .addon_diagnostics_for(&self.path)
                        .find(|(sid, _)| *sid == id)
                        .map(|(_, d)| d.clone()),
                    None => s.diagnostics_for(&self.path).cloned(),
                }
            })
            .map(|d| {
                d.items
                    .into_iter()
                    .filter(|d| match self.server {
                        // The spell checker offers fixes for every word it's
                        // given: only the one under the cursor.
                        Some(_) => d.range.start <= start && d.range.end >= end,
                        None => d.range.start.line <= end.line && d.range.end.line >= start.line,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if self.server.is_some() && diagnostics.is_empty() {
            return Task::ready(Ok(vec![]));
        }
        let params = json!({
            "textDocument": self.doc(),
            "range": { "start": start, "end": end },
            "context": { "diagnostics": diagnostics, "triggerKind": 1 },
        });
        let task = self.request(&rope, "textDocument/codeAction", params, cx);
        let server_commands = self.server_commands(cx);
        cx.background_executor().spawn(async move {
            let v = quiet(task.await.map(Some))?.unwrap_or(Value::Null);
            if v.is_null() {
                return Ok(vec![]);
            }
            let items: Vec<CodeActionOrCommand> = serde_json::from_value(v)?;
            // Bare commands run through the server's executeCommand, so only
            // the ones it says it runs (others are editor-side, e.g. "run test").
            Ok(items
                .into_iter()
                .filter_map(|i| match i {
                    CodeActionOrCommand::CodeAction(a) => Some(a),
                    CodeActionOrCommand::Command(c) if server_commands.contains(&c.command) => {
                        Some(CodeAction {
                            title: c.title.clone(),
                            command: Some(c),
                            ..Default::default()
                        })
                    }
                    CodeActionOrCommand::Command(_) => None,
                })
                .collect())
        })
    }

    fn perform_code_action(
        &self,
        state: Entity<EditorState>,
        action: CodeAction,
        _push_to_history: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let Some(enc) = self.encoding(cx) else {
            return Task::ready(Ok(()));
        };
        // Called from inside `state`'s update: don't read `state` here.
        let _ = &state;
        let resolve = if action.edit.is_none() && action.data.is_some() {
            Some(self.request_with(
                None,
                "codeAction/resolve",
                serde_json::to_value(&action).unwrap_or_default(),
                cx,
            ))
        } else {
            None
        };
        let server_commands = self.server_commands(cx);
        let store = self.store.clone();
        let path = self.path.clone();
        let server = self.server;
        window.spawn(cx, async move |cx| {
            let action = match resolve {
                Some(t) => serde_json::from_value::<CodeAction>(t.await?)?,
                None => action,
            };
            if let Some(edit) = action.edit {
                cx.update(|window, cx| apply_workspace_edit(edit, enc, window, cx))?;
            }
            // Then its command, if the server runs it (it may send workspace/applyEdit back).
            if let Some(command) = action
                .command
                .filter(|c| server_commands.contains(&c.command))
            {
                let params = json!({ "command": command.command, "arguments": command.arguments });
                let task = cx.update(|_, cx| {
                    store.upgrade().map(|s| {
                        s.update(cx, |s, cx| match server {
                            Some(id) => s.request_to(
                                id,
                                &path,
                                None,
                                "workspace/executeCommand",
                                params,
                                cx,
                            ),
                            None => s.request(&path, None, "workspace/executeCommand", params, cx),
                        })
                    })
                })?;
                if let Some(task) = task {
                    task.await?;
                }
            }
            Ok(())
        })
    }
}

/// All text edits in a workspace edit, by file.
fn edits_by_file(edit: WorkspaceEdit) -> Vec<(PathBuf, Vec<TextEdit>)> {
    let mut out: Vec<(PathBuf, Vec<TextEdit>)> = Vec::new();
    let mut add = |uri: &lsp_types::Uri, edits: Vec<TextEdit>| {
        let Some(path) = uri_path(uri.as_str()) else {
            return;
        };
        match out.iter_mut().find(|(p, _)| *p == path) {
            Some((_, e)) => e.extend(edits),
            None => out.push((path, edits)),
        }
    };
    if let Some(changes) = edit.changes {
        for (uri, edits) in changes {
            add(&uri, edits);
        }
    }
    if let Some(lsp_types::DocumentChanges::Edits(docs)) = edit.document_changes {
        for d in docs {
            let edits = d
                .edits
                .into_iter()
                .map(|e| match e {
                    lsp_types::OneOf::Left(t) => t,
                    lsp_types::OneOf::Right(a) => a.text_edit,
                })
                .collect();
            add(&d.text_document.uri, edits);
        }
    }
    out
}

/// LSP edits refer to the original text: apply them last-first so earlier ones don't shift later ones.
fn sorted_desc(mut edits: Vec<TextEdit>) -> Vec<TextEdit> {
    edits.sort_by(|a, b| {
        (b.range.start.line, b.range.start.character)
            .cmp(&(a.range.start.line, a.range.start.character))
    });
    edits
}

/// Apply a server's workspace edit: through the open editor (undoable) where the
/// file is open in this window, otherwise to the file's buffer, then saved.
pub fn apply_workspace_edit(edit: WorkspaceEdit, enc: Encoding, window: &mut Window, cx: &mut App) {
    let win = window.window_handle().window_id();
    for (path, edits) in edits_by_file(edit) {
        let open = cx
            .global::<crate::OpenEditors>()
            .0
            .iter()
            .filter_map(|w| w.upgrade())
            .find(|p| p.read(cx).window_id == win && p.read(cx).path == path);
        if let Some(panel) = open {
            let editor = panel.read(cx).editor.clone();
            editor.update(cx, |e, cx| {
                let rope = e.text().clone();
                let edits: Vec<TextEdit> = sorted_desc(edits)
                    .into_iter()
                    .map(|mut t| {
                        t.range = range_from_server(&rope, t.range, enc);
                        t
                    })
                    .collect();
                e.apply_lsp_edits(&edits, window, cx);
            });
        } else {
            apply_to_file(path, edits, enc, window, cx);
        }
    }
}

fn apply_to_file(
    path: PathBuf,
    edits: Vec<TextEdit>,
    enc: Encoding,
    window: &mut Window,
    cx: &mut App,
) {
    let workspace = pide_core::Services::of(window, cx).workspace;
    let open = workspace.update(cx, |w, cx| w.open(path.clone(), cx));
    cx.spawn(async move |cx| {
        let buffer = match open.await {
            Ok(b) => b,
            Err(e) => {
                log::error!("apply edit to {}: {e:#}", path.display());
                return;
            }
        };
        let save = cx.update(|cx| {
            let text = buffer.read(cx).text().to_string();
            let new_text = apply_text_edits(&text, edits, enc);
            workspace.update(cx, |w, cx| {
                w.edited(&buffer, Rope::from(new_text.as_str()), cx);
                w.save(&buffer, cx)
            })
        });
        if let Err(e) = save.await {
            log::error!("save {} after edit: {e:#}", path.display());
        }
    })
    .detach();
}

/// Apply LSP edits (server encoding) to plain text.
pub(crate) fn apply_text_edits(text: &str, edits: Vec<TextEdit>, enc: Encoding) -> String {
    let mut out = text.to_string();
    for e in sorted_desc(edits) {
        let range = {
            let lines = TextLines::new(&out);
            let r = range_from_server(&lines, e.range, enc);
            byte_offset(&out, r.start)..byte_offset(&out, r.end)
        };
        out.replace_range(range, &e.new_text);
    }
    out
}

/// 0-based line + character column -> byte offset (clamped).
fn byte_offset(text: &str, pos: lsp_types::Position) -> usize {
    let mut offset = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i == pos.line as usize {
            let body = line.strip_suffix('\n').unwrap_or(line);
            return offset
                + body
                    .char_indices()
                    .nth(pos.character as usize)
                    .map(|(b, _)| b)
                    .unwrap_or(body.len());
        }
        offset += line.len();
    }
    text.len()
}

/// Find Usages (Alt+F7): every reference to the symbol at the cursor, listed
/// in the Usages panel. Line text comes from open buffers or the file on disk.
pub(crate) fn find_usages(window: &mut Window, cx: &mut App) {
    query_locations("textDocument/references", "Usages of", false, window, cx);
}

/// Go to Implementation (Cmd+Alt+B): one result jumps there, several are listed.
pub(crate) fn go_to_implementation(window: &mut Window, cx: &mut App) {
    query_locations(
        "textDocument/implementation",
        "Implementations of",
        true,
        window,
        cx,
    );
}

/// Go to Type Declaration (Ctrl+Shift+B).
pub(crate) fn go_to_type_definition(window: &mut Window, cx: &mut App) {
    query_locations("textDocument/typeDefinition", "Type of", true, window, cx);
}

/// A location response in any of its shapes (`Location`, `Location[]`, `LocationLink[]`).
fn parse_locations(v: Value) -> Vec<lsp_types::Location> {
    match serde_json::from_value::<GotoDefinitionResponse>(v) {
        Ok(GotoDefinitionResponse::Scalar(l)) => vec![l],
        Ok(GotoDefinitionResponse::Array(ls)) => ls,
        Ok(GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .map(|l| lsp_types::Location {
                uri: l.target_uri,
                range: l.target_selection_range,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Ask the server for places related to the symbol at the cursor and list them
/// in the Usages panel (or, with `jump_single`, go straight to a lone result).
fn query_locations(
    method: &'static str,
    title: &'static str,
    jump_single: bool,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::input::RopeExt as _;
    use gpui_kit::component::notification::Notification;
    let Some(panel) = crate::active_panel(window, cx) else {
        return;
    };
    let (path, editor, store) = {
        let p = panel.read(cx);
        (p.path.clone(), p.editor.clone(), p.lsp.clone())
    };
    let Some(store) = store else { return };
    let Some(enc) = store.read(cx).server_for(&path).map(|s| s.encoding) else {
        window.push_notification(
            Notification::warning("No language server is running for this file."),
            cx,
        );
        return;
    };
    let (rope, offset) = {
        let e = editor.read(cx);
        (e.text().clone(), e.cursor())
    };
    let word = rope
        .word_range(offset)
        .map(|r| rope.slice(r).to_string())
        .unwrap_or_default();
    let params = json!({
        "textDocument": { "uri": file_uri(&path) },
        "position": offset_to_server(&rope, offset, enc),
        "context": { "includeDeclaration": true },
    });
    let task = store.update(cx, |s, cx| {
        s.request(&path, Some(&rope), method, params, cx)
    });
    // Text of open buffers (unsaved edits count), for the lines shown.
    let open: std::collections::HashMap<PathBuf, String> = pide_core::Services::of(window, cx)
        .workspace
        .read(cx)
        .buffers()
        .iter()
        .map(|b| (b.read(cx).path.clone(), b.read(cx).text().to_string()))
        .collect();
    window
        .spawn(cx, async move |cx| {
            let v = task.await.unwrap_or(Value::Null);
            let refs = parse_locations(v);
            let found = cx
                .background_executor()
                .spawn(async move {
                    let mut texts: std::collections::HashMap<PathBuf, String> =
                        std::collections::HashMap::new();
                    let mut out = Vec::new();
                    for r in refs {
                        let Some(path) = uri_path(r.uri.as_str()) else {
                            continue;
                        };
                        let text = texts.entry(path.clone()).or_insert_with(|| {
                            open.get(&path)
                                .cloned()
                                .or_else(|| std::fs::read_to_string(&path).ok())
                                .unwrap_or_default()
                        });
                        let lines = TextLines::new(text);
                        let range = range_from_server(&lines, r.range, enc);
                        let line = text
                            .split('\n')
                            .nth(range.start.line as usize)
                            .unwrap_or("")
                            .trim_end_matches('\r')
                            .to_string();
                        let byte = |ch: u32| {
                            line.char_indices()
                                .nth(ch as usize)
                                .map(|(b, _)| b)
                                .unwrap_or(line.len())
                        };
                        let end_ch = if range.end.line == range.start.line {
                            range.end.character
                        } else {
                            line.chars().count() as u32
                        };
                        out.push(pide_core::opener::Location {
                            path,
                            line: range.start.line,
                            character: range.start.character,
                            range: byte(range.start.character)..byte(end_ch),
                            text: line,
                        });
                    }
                    out.sort_by(|a, b| {
                        (&a.path, a.line, a.character).cmp(&(&b.path, b.line, b.character))
                    });
                    out
                })
                .await;
            let _ = cx.update(|window, cx| {
                if found.is_empty() {
                    let what = title.split(' ').next().unwrap_or(title).to_lowercase();
                    window.push_notification(
                        Notification::info(format!("No {what} found for {word}.")),
                        cx,
                    );
                } else if jump_single && found.len() == 1 {
                    let l = &found[0];
                    pide_core::opener::open_file_at(
                        l.path.clone(),
                        l.line,
                        l.character,
                        window,
                        cx,
                    );
                } else {
                    pide_core::opener::show_locations(
                        format!("{title} {word}").into(),
                        found,
                        window,
                        cx,
                    );
                }
            });
        })
        .detach();
}

/// Optimize Imports (Ctrl+Alt+O): the server's `source.organizeImports` code
/// action, or failing that its own organize-imports command (pyright's
/// `pyright.organizeimports`, which sends the edit back with `workspace/applyEdit`).
pub(crate) fn organize_imports(window: &mut Window, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::notification::Notification;
    let Some(panel) = crate::active_panel(window, cx) else {
        return;
    };
    let (path, editor, store) = {
        let p = panel.read(cx);
        (p.path.clone(), p.editor.clone(), p.lsp.clone())
    };
    let Some(store) = store else { return };
    let Some((enc, commands)) = store.read(cx).server_for(&path).map(|s| {
        let commands = s
            .capabilities
            .as_ref()
            .and_then(|c| c.execute_command_provider.as_ref())
            .map(|e| e.commands.clone())
            .unwrap_or_default();
        (s.encoding, commands)
    }) else {
        window.push_notification(
            Notification::warning("No language server is running for this file."),
            cx,
        );
        return;
    };
    let rope = editor.read(cx).text().clone();
    let end = offset_to_server(&rope, rope.len(), enc);
    let params = json!({
        "textDocument": { "uri": file_uri(&path) },
        "range": { "start": { "line": 0, "character": 0 }, "end": end },
        "context": { "diagnostics": [], "only": ["source.organizeImports"], "triggerKind": 1 },
    });
    let actions = store.update(cx, |s, cx| {
        s.request(&path, Some(&rope), "textDocument/codeAction", params, cx)
    });
    let fallback = commands
        .into_iter()
        .find(|c| c.to_lowercase().ends_with("organizeimports"))
        .or_else(|| {
            store
                .read(cx)
                .server_for(&path)
                .and_then(|s| s.spec.organize_imports_command)
                .map(str::to_string)
        });
    let uri = file_uri(&path);
    window
        .spawn(cx, async move |cx| {
            let actions: Vec<CodeActionOrCommand> = actions
                .await
                .ok()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let action = actions.into_iter().find_map(|a| match a {
                CodeActionOrCommand::CodeAction(a) => Some(a),
                CodeActionOrCommand::Command(_) => None,
            });
            let task = cx.update(|_, cx| match (action, fallback) {
                (Some(action), _) => {
                    let resolve = (action.edit.is_none() && action.data.is_some()).then(|| {
                        store.update(cx, |s, cx| {
                            s.request(
                                &path,
                                None,
                                "codeAction/resolve",
                                serde_json::to_value(&action).unwrap_or_default(),
                                cx,
                            )
                        })
                    });
                    Some((Some(action), resolve))
                }
                (None, Some(command)) => {
                    let params = json!({ "command": command, "arguments": [uri] });
                    store
                        .update(cx, |s, cx| {
                            s.request(&path, None, "workspace/executeCommand", params, cx)
                        })
                        .detach();
                    None
                }
                (None, None) => None,
            })?;
            let Some((Some(action), resolve)) = task else {
                return Ok::<(), anyhow::Error>(());
            };
            let action = match resolve {
                Some(t) => serde_json::from_value::<CodeAction>(t.await?)?,
                None => action,
            };
            if let Some(edit) = action.edit {
                cx.update(|window, cx| apply_workspace_edit(edit, enc, window, cx))?;
            }
            Ok(())
        })
        .detach_and_log_err(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(sl: u32, sc: u32, el: u32, ec: u32, t: &str) -> TextEdit {
        TextEdit {
            range: lsp_types::Range::new(
                lsp_types::Position::new(sl, sc),
                lsp_types::Position::new(el, ec),
            ),
            new_text: t.into(),
        }
    }

    #[::core::prelude::v1::test]
    fn completions_filter_by_typed_word_prefix_first() {
        let item = |label: &str, sort: &str| lsp_types::CompletionItem {
            label: label.into(),
            sort_text: Some(sort.into()),
            ..Default::default()
        };
        let mut items = vec![
            item("self::", "1"),
            item("crate::", "2"),
            item("greet", "3"),
            item("program_greeting", "0"),
        ];
        rank_completions(&mut items, "gre");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["greet", "program_greeting"]);
    }

    #[::core::prelude::v1::test]
    fn edits_apply_last_first_with_utf16_columns() {
        let text = "fn add(a: i32) {}\nlet s = \"😀\"; add(1);\n";
        // Rename `add` in two places; the second is after an emoji (2 UTF-16 units).
        let out = apply_text_edits(
            text,
            vec![edit(0, 3, 0, 6, "sum"), edit(1, 14, 1, 17, "sum")],
            Encoding::Utf16,
        );
        assert_eq!(out, "fn sum(a: i32) {}\nlet s = \"😀\"; sum(1);\n");
    }
}

/// Code: Reformat File (Cmd+Alt+L): the server's `textDocument/formatting`,
/// applied as one undoable change.
pub(crate) fn reformat(window: &mut Window, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::notification::Notification;
    let Some(panel) = crate::active_panel(window, cx) else {
        return;
    };
    let (path, editor, store) = {
        let p = panel.read(cx);
        (p.path.clone(), p.editor.clone(), p.lsp.clone())
    };
    let Some(store) = store else { return };
    let Some((enc, can)) = store.read(cx).server_for(&path).map(|s| {
        let can = s
            .capabilities
            .as_ref()
            .and_then(|c| c.document_formatting_provider.as_ref())
            .is_some_and(|p| !matches!(p, lsp_types::OneOf::Left(false)));
        (s.encoding, can)
    }) else {
        window.push_notification(
            Notification::warning("No language server is running for this file."),
            cx,
        );
        return;
    };
    if !can {
        window.push_notification(
            Notification::warning("This file's language server can't format."),
            cx,
        );
        return;
    }
    let rope = editor.read(cx).text().clone();
    let params = json!({
        "textDocument": { "uri": file_uri(&path) },
        "options": { "tabSize": 4, "insertSpaces": true, "trimTrailingWhitespace": true, "insertFinalNewline": true },
    });
    let task = store.update(cx, |s, cx| {
        s.request(&path, Some(&rope), "textDocument/formatting", params, cx)
    });
    window
        .spawn(cx, async move |cx| {
            let result = task.await;
            let _ = cx.update(|window, cx| {
                let edits: Vec<TextEdit> = match result {
                    Ok(v) => serde_json::from_value(v).unwrap_or_default(),
                    Err(e) => {
                        window.push_notification(
                            Notification::error(format!("Reformat failed: {e:#}")),
                            cx,
                        );
                        return;
                    }
                };
                if edits.is_empty() {
                    window.push_notification(Notification::info("Already formatted."), cx);
                    return;
                }
                // The edits are against the text sent; skip if it changed meanwhile.
                let now = editor.read(cx).text().clone();
                if now.len() != rope.len() || now != rope {
                    return;
                }
                let new = apply_text_edits(&rope.to_string(), edits, enc);
                crate::minimal::set_text(&editor, new, false, window, cx);
            });
        })
        .detach();
}

/// Quick Documentation (F1 / Ctrl+J): the hover for the symbol at the cursor,
/// without the mouse.
pub(crate) fn quick_documentation(window: &mut Window, cx: &mut App) {
    use gpui_kit::component::input::RopeExt as _;
    let Some(panel) = crate::active_panel(window, cx) else {
        return;
    };
    let (editor, providers) = {
        let p = panel.read(cx);
        (p.editor.clone(), p.providers.clone())
    };
    let Some(providers) = providers else { return };
    let (rope, offset) = {
        let e = editor.read(cx);
        (e.text().clone(), e.cursor())
    };
    let task = providers.hover(&rope, offset, window, cx);
    window
        .spawn(cx, async move |cx| {
            let Ok(Some(hover)) = task.await else { return };
            let _ = cx.update(|_, cx| {
                editor.update(cx, |e, cx| {
                    // Same text as asked about, or the popup would point at the wrong place.
                    if e.text().len() != rope.len() {
                        return;
                    }
                    let range = hover
                        .range
                        .map(|r| rope.position_to_offset(&r.start)..rope.position_to_offset(&r.end))
                        .or_else(|| rope.word_range(offset))
                        .unwrap_or(offset..offset);
                    e.present_hover(range, hover, cx);
                })
            });
        })
        .detach();
}

/// Format the active file with the server and give back the formatted text
/// (`None`: no server that formats, nothing to change, or it failed). For
/// format-on-save.
pub(crate) fn formatted_text(window: &mut Window, cx: &mut App) -> Option<Task<Option<String>>> {
    let panel = crate::active_panel(window, cx)?;
    let (path, editor, store) = {
        let p = panel.read(cx);
        (p.path.clone(), p.editor.clone(), p.lsp.clone()?)
    };
    let (enc, can) = store.read(cx).server_for(&path).map(|s| {
        let can = s
            .capabilities
            .as_ref()
            .and_then(|c| c.document_formatting_provider.as_ref())
            .is_some_and(|p| !matches!(p, lsp_types::OneOf::Left(false)));
        (s.encoding, can)
    })?;
    if !can {
        return None;
    }
    let rope = editor.read(cx).text().clone();
    let params = json!({
        "textDocument": { "uri": file_uri(&path) },
        "options": { "tabSize": 4, "insertSpaces": true, "trimTrailingWhitespace": true, "insertFinalNewline": true },
    });
    let task = store.update(cx, |s, cx| {
        s.request(&path, Some(&rope), "textDocument/formatting", params, cx)
    });
    Some(cx.background_executor().spawn(async move {
        let edits: Vec<TextEdit> = serde_json::from_value(task.await.ok()?).ok()?;
        if edits.is_empty() {
            return None;
        }
        let before = rope.to_string();
        let after = apply_text_edits(&before, edits, enc);
        (after != before).then_some(after)
    }))
}
