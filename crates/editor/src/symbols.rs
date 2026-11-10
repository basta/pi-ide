//! File Structure (Cmd+F12) and Go to Symbol (Cmd+Alt+O): symbols from the
//! language server in a search popup; Enter jumps there.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, WindowExt as _, h_flex};
use gpui_kit::*;
use lsp_types::{DocumentSymbolResponse, SymbolKind};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use pide_lsp::convert::{TextLines, from_server};
use pide_lsp::process::{file_uri, uri_path};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct Symbol {
    pub name: String,
    /// Where it sits ("impl Shell", a file path), shown dimmed.
    pub detail: String,
    pub kind: &'static str,
    pub path: PathBuf,
    /// Server position (converted when jumping).
    pub line: u32,
    pub character: u32,
}

fn kind_label(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::FUNCTION => "fn",
        SymbolKind::METHOD => "method",
        SymbolKind::STRUCT => "struct",
        SymbolKind::CLASS => "class",
        SymbolKind::ENUM => "enum",
        SymbolKind::INTERFACE => "trait",
        SymbolKind::CONSTANT => "const",
        SymbolKind::FIELD | SymbolKind::PROPERTY => "field",
        SymbolKind::MODULE | SymbolKind::NAMESPACE => "mod",
        SymbolKind::VARIABLE => "var",
        SymbolKind::TYPE_PARAMETER => "type",
        SymbolKind::ENUM_MEMBER => "variant",
        _ => "",
    }
}

/// A document's symbols, nested ones flattened with their parent as detail.
pub(crate) fn flatten(resp: DocumentSymbolResponse, path: &std::path::Path) -> Vec<Symbol> {
    let mut out = Vec::new();
    match resp {
        DocumentSymbolResponse::Flat(list) => {
            for s in list {
                out.push(Symbol {
                    name: s.name,
                    detail: s.container_name.unwrap_or_default(),
                    kind: kind_label(s.kind),
                    path: path.to_path_buf(),
                    line: s.location.range.start.line,
                    character: s.location.range.start.character,
                });
            }
        }
        DocumentSymbolResponse::Nested(list) => {
            fn walk(
                list: Vec<lsp_types::DocumentSymbol>,
                parent: &str,
                path: &std::path::Path,
                out: &mut Vec<Symbol>,
            ) {
                for s in list {
                    out.push(Symbol {
                        name: s.name.clone(),
                        detail: parent.to_string(),
                        kind: kind_label(s.kind),
                        path: path.to_path_buf(),
                        line: s.selection_range.start.line,
                        character: s.selection_range.start.character,
                    });
                    if let Some(children) = s.children {
                        walk(children, &s.name, path, out);
                    }
                }
            }
            walk(list, "", path, &mut out);
        }
    }
    out
}

fn rank(items: &[Symbol], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..items.len().min(200)).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            pattern
                .score(
                    nucleo_matcher::Utf32Str::new(&s.name, &mut buf),
                    &mut matcher,
                )
                .map(|sc| (i, sc))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(200);
    scored.into_iter().map(|(i, _)| i).collect()
}

/// Cmd+F12: this file's symbols.
pub(crate) fn file_structure(window: &mut Window, cx: &mut App) {
    let Some(panel) = crate::active_panel(window, cx) else {
        return;
    };
    let (path, editor, store) = {
        let p = panel.read(cx);
        (p.path.clone(), p.editor.clone(), p.lsp.clone())
    };
    let Some(store) = store.filter(|s| s.read(cx).server_for(&path).is_some()) else {
        window.push_notification(
            Notification::warning("No language server is running for this file."),
            cx,
        );
        return;
    };
    let rope = editor.read(cx).text().clone();
    let params = json!({ "textDocument": { "uri": file_uri(&path) } });
    let task = store.update(cx, |s, cx| {
        s.request(
            &path,
            Some(&rope),
            "textDocument/documentSymbol",
            params,
            cx,
        )
    });
    window
        .spawn(cx, async move |cx| {
            let v = task.await.unwrap_or(Value::Null);
            let items = serde_json::from_value::<DocumentSymbolResponse>(v)
                .map(|r| flatten(r, &path))
                .unwrap_or_default();
            let _ = cx.update(|window, cx| {
                open_popup(items, None, "Go to symbol in this file", store, window, cx)
            });
        })
        .detach();
}

/// Cmd+Alt+O: symbols across the project (asks the server as you type).
pub(crate) fn go_to_symbol(window: &mut Window, cx: &mut App) {
    let Some(panel) = crate::active_panel(window, cx) else {
        return;
    };
    let (path, store) = {
        let p = panel.read(cx);
        (p.path.clone(), p.lsp.clone())
    };
    let Some(store) = store.filter(|s| s.read(cx).server_for(&path).is_some()) else {
        window.push_notification(
            Notification::warning("Open a file with a running language server first."),
            cx,
        );
        return;
    };
    open_popup(
        vec![],
        Some(path),
        "Go to symbol in project",
        store,
        window,
        cx,
    );
}

/// The popup. `workspace_from`: search the whole project through this file's server.
fn open_popup(
    initial: Vec<Symbol>,
    workspace_from: Option<PathBuf>,
    placeholder: &'static str,
    store: Entity<pide_lsp::LspStore>,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }
    let items = Rc::new(RefCell::new(initial));
    let order = Rc::new(RefCell::new(rank(&items.borrow(), "")));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(std::cell::Cell::new(true));
    let pending: Rc<RefCell<Option<Task<()>>>> = Rc::default();
    window.open_dialog(cx, move |dialog, _, _| {
        let (items, order, state, focus_once, pending, store, from) = (
            items.clone(),
            order.clone(),
            state.clone(),
            focus_once.clone(),
            pending.clone(),
            store.clone(),
            workspace_from.clone(),
        );
        dialog
            .text_sm()
            .close_button(false)
            .p_0()
            .content(move |content, window, cx| {
                if focus_once.replace(false) {
                    let state = state.clone();
                    window.defer(cx, move |window, cx| {
                        state.read(cx).focus_handle(cx).focus(window, cx)
                    });
                }
                let muted = cx.theme().muted_foreground;
                let rows: Vec<CommandItem> = order
                    .borrow()
                    .iter()
                    .map(|&i| {
                        let s = items.borrow()[i].clone();
                        CommandItem::new().label(s.name.clone()).child(move |_, _| {
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(52.))
                                        .flex_none()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(s.kind),
                                )
                                .child(div().flex_none().child(s.name.clone()))
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(s.detail.clone()),
                                )
                        })
                    })
                    .collect();
                let (items_q, order_q, pending_q, store_q, from_q) = (
                    items.clone(),
                    order.clone(),
                    pending.clone(),
                    store.clone(),
                    from.clone(),
                );
                let (items_c, order_c, store_c) = (items.clone(), order.clone(), store.clone());
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder(placeholder)
                        .max_h(px(460.))
                        .filterable(false)
                        .items(rows)
                        .on_query(move |q, window, cx| {
                            match &from_q {
                                None => {
                                    *order_q.borrow_mut() = rank(&items_q.borrow(), q);
                                    window.refresh();
                                }
                                Some(path) => {
                                    let task = store_q.update(cx, |s, cx| {
                                        s.request(
                                            path,
                                            None,
                                            "workspace/symbol",
                                            json!({ "query": q }),
                                            cx,
                                        )
                                    });
                                    let (items, order, q) =
                                        (items_q.clone(), order_q.clone(), q.to_string());
                                    *pending_q.borrow_mut() =
                                        Some(window.spawn(cx, async move |cx| {
                                            let v = task.await.unwrap_or(Value::Null);
                                            let list: Vec<lsp_types::SymbolInformation> =
                                                serde_json::from_value(v.clone())
                                                    .or_else(|_| {
                                                        // WorkspaceSymbol form: location may be a uri only.
                                                        serde_json::from_value::<
                                                            Vec<lsp_types::WorkspaceSymbol>,
                                                        >(
                                                            v
                                                        )
                                                        .map(|ws| {
                                                            ws.into_iter()
                                                                .filter_map(|w| {
                                                                    match w.location {
                                                        lsp_types::OneOf::Left(location) => {
                                                            #[allow(deprecated)]
                                                            Some(lsp_types::SymbolInformation {
                                                                name: w.name,
                                                                kind: w.kind,
                                                                tags: w.tags,
                                                                deprecated: None,
                                                                location,
                                                                container_name: w.container_name,
                                                            })
                                                        }
                                                        lsp_types::OneOf::Right(_) => None,
                                                    }
                                                                })
                                                                .collect()
                                                        })
                                                    })
                                                    .unwrap_or_default();
                                            let mut found = Vec::new();
                                            for s in list {
                                                let Some(p) = uri_path(s.location.uri.as_str())
                                                else {
                                                    continue;
                                                };
                                                found.push(Symbol {
                                                    name: s.name,
                                                    detail: s.container_name.unwrap_or_default(),
                                                    kind: kind_label(s.kind),
                                                    path: p,
                                                    line: s.location.range.start.line,
                                                    character: s.location.range.start.character,
                                                });
                                            }
                                            let ranked = rank(&found, &q);
                                            *items.borrow_mut() = found;
                                            *order.borrow_mut() = ranked;
                                            let _ = cx.update(|window, _| window.refresh());
                                        }));
                                }
                            }
                        })
                        .on_confirm(move |ix, window, cx| {
                            let s = order_c
                                .borrow()
                                .get(ix.row)
                                .map(|&i| items_c.borrow()[i].clone());
                            window.close_dialog(cx);
                            let Some(s) = s else { return };
                            // Server column -> characters, using the file's text (read off the UI thread).
                            let enc = store_c
                                .read(cx)
                                .server_for(&s.path)
                                .map(|x| x.encoding)
                                .unwrap_or_default();
                            let path = s.path.clone();
                            let read = cx.background_executor().spawn(async move {
                                let text = std::fs::read_to_string(&path).unwrap_or_default();
                                from_server(
                                    &TextLines::new(&text),
                                    lsp_types::Position::new(s.line, s.character),
                                    enc,
                                )
                            });
                            let path = s.path;
                            window
                                .spawn(cx, async move |cx| {
                                    let pos = read.await;
                                    let _ = cx.update(|window, cx| {
                                        pide_core::opener::open_file_at(
                                            path,
                                            pos.line,
                                            pos.character,
                                            window,
                                            cx,
                                        )
                                    });
                                })
                                .detach();
                        }),
                )
            })
    });
}
