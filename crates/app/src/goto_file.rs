//! Go to File (Cmd+Shift+O, spec M4): fuzzy search over the project's files,
//! respecting .gitignore. The file list is built off the UI thread and kept per
//! window; it refreshes when files appear or disappear.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme, WindowExt as _, h_flex};
use gpui_kit::*;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use pide_core::{Services, WorkspaceEvent};

/// Stop listing past this many files (a huge tree is still searchable, just not all of it).
const MAX_FILES: usize = 200_000;
/// Rows shown at once.
const SHOWN: usize = 80;

struct Index {
    files: Rc<Vec<String>>,
    stale: bool,
    loading: bool,
    _sub: Subscription,
}

#[derive(Default)]
struct Indexes(HashMap<WindowId, Index>);
impl Global for Indexes {}

/// Relative paths of the project's files, gitignore-aware, sorted shortest first.
pub fn list_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != ".DS_Store")
        .build();
    for entry in walker.flatten() {
        if out.len() >= MAX_FILES {
            break;
        }
        if entry.file_type().is_some_and(|t| t.is_file())
            && let Ok(rel) = entry.path().strip_prefix(root)
        {
            out.push(rel.to_string_lossy().to_string());
        }
    }
    out.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    out
}

/// Best matches for `query`: nucleo's fuzzy score, with a bonus when the file
/// name itself matches (typing `main` prefers `src/main.rs` over `domain/x.rs`).
pub fn rank(files: &[String], query: &str, limit: usize) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..files.len().min(limit)).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let name_pattern = Pattern::parse(
        query.rsplit('/').next().unwrap_or(query),
        CaseMatching::Smart,
        Normalization::Smart,
    );
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32)> = files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let s = pattern.score(nucleo_matcher::Utf32Str::new(f, &mut buf), &mut matcher)?;
            let name = f.rsplit('/').next().unwrap_or(f);
            let bonus = name_pattern
                .score(nucleo_matcher::Utf32Str::new(name, &mut buf), &mut matcher)
                .unwrap_or(0);
            Some((i, s + bonus))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(files[a.0].len().cmp(&files[b.0].len())));
    scored.truncate(limit);
    scored.into_iter().map(|(i, _)| i).collect()
}

/// Start (or refresh) the window's file list in the background.
fn refresh(window: &Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let Some(services) = Services::for_window(id, cx) else {
        return;
    };
    let root = services.workspace.read(cx).root().to_path_buf();
    if !cx.default_global::<Indexes>().0.contains_key(&id) {
        let sub = cx.subscribe(&services.workspace, move |_, ev: &WorkspaceEvent, cx| {
            if matches!(ev, WorkspaceEvent::TreeChanged)
                && let Some(ix) = cx.default_global::<Indexes>().0.get_mut(&id)
            {
                ix.stale = true;
            }
        });
        cx.default_global::<Indexes>().0.insert(
            id,
            Index {
                files: Rc::new(vec![]),
                stale: true,
                loading: false,
                _sub: sub,
            },
        );
    }
    let ix = cx.default_global::<Indexes>().0.get_mut(&id).unwrap();
    if ix.loading || !ix.stale {
        return;
    }
    ix.loading = true;
    let task = cx
        .background_executor()
        .spawn(async move { list_files(&root) });
    cx.spawn(async move |cx| {
        let files = task.await;
        cx.update(|cx| {
            if let Some(ix) = cx.default_global::<Indexes>().0.get_mut(&id) {
                ix.files = Rc::new(files);
                ix.stale = false;
                ix.loading = false;
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Build the index for a project window early, so the first Cmd+Shift+O is instant.
pub fn warm(window: &Window, cx: &mut App) {
    refresh(window, cx);
}

pub fn open(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    refresh(window, cx);
    let id = window.window_handle().window_id();
    let Some(root) =
        Services::for_window(id, cx).map(|s| s.workspace.read(cx).root().to_path_buf())
    else {
        return;
    };
    let query = Rc::new(RefCell::new(String::new()));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_once = Rc::new(std::cell::Cell::new(true));
    // The rows for the current query, recomputed when it or the index changes.
    let cache: Rc<RefCell<(String, usize, Vec<usize>)>> =
        Rc::new(RefCell::new((String::from("\u{0}"), usize::MAX, vec![])));

    window.open_dialog(cx, move |dialog, _, _| {
        let (query, state, focus_once, cache, root) = (
            query.clone(),
            state.clone(),
            focus_once.clone(),
            cache.clone(),
            root.clone(),
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
                let (files, loading) = cx
                    .try_global::<Indexes>()
                    .and_then(|ix| ix.0.get(&id))
                    .map(|ix| (ix.files.clone(), ix.loading && ix.files.is_empty()))
                    .unwrap_or_default();
                let q = query.borrow().clone();
                {
                    let mut c = cache.borrow_mut();
                    if c.0 != q || c.1 != files.len() {
                        *c = (q.clone(), files.len(), rank(&files, &q, SHOWN));
                    }
                }
                let order = cache.borrow().2.clone();
                let muted = cx.theme().muted_foreground;
                let items: Vec<CommandItem> = order
                    .iter()
                    .map(|&i| {
                        let rel = files[i].clone();
                        let (dir, name) = match rel.rsplit_once('/') {
                            Some((d, n)) => (d.to_string(), n.to_string()),
                            None => (String::new(), rel.clone()),
                        };
                        CommandItem::new().label(rel.clone()).child(move |_, _| {
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(div().flex_none().child(name.clone()))
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(dir.clone()),
                                )
                        })
                    })
                    .collect();
                let (query_q, cache_c, files_c, root_c) =
                    (query.clone(), cache.clone(), files.clone(), root.clone());
                let popup_h = (52. + files.len().min(SHOWN) as f32 * 32.).clamp(160., 460.);
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder(if loading {
                            "Listing files…".to_string()
                        } else {
                            format!("Go to file ({} files)", files.len())
                        })
                        // One height while you type (it grew and shrank with the
                        // matches): what the whole list needs, up to 460.
                        .h(px(popup_h))
                        .max_h(px(popup_h - 52.))
                        .filterable(false)
                        .items(items)
                        .on_query(move |q, window, _| {
                            *query_q.borrow_mut() = q.to_string();
                            window.refresh();
                        })
                        .on_confirm(move |ix, window, cx| {
                            let path = cache_c
                                .borrow()
                                .2
                                .get(ix.row)
                                .map(|&i| root_c.join(&files_c[i]));
                            window.close_dialog(cx);
                            if let Some(path) = path {
                                pide_core::opener::open_file(path, window, cx);
                            }
                        }),
                )
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        let mut v: Vec<String> = [
            "src/main.rs",
            "src/domain/mainline.rs",
            "crates/core/src/buffer.rs",
            "README.md",
            "docs/GAPS.md",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        v.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        v
    }

    #[::core::prelude::v1::test]
    fn file_name_matches_rank_first() {
        let f = files();
        assert_eq!(f[rank(&f, "main", 10)[0]], "src/main.rs");
        assert_eq!(f[rank(&f, "buf", 10)[0]], "crates/core/src/buffer.rs");
        assert_eq!(f[rank(&f, "core/buf", 10)[0]], "crates/core/src/buffer.rs");
        assert!(rank(&f, "zzz", 10).is_empty());
        assert_eq!(rank(&f, "", 2).len(), 2);
    }

    #[::core::prelude::v1::test]
    fn listing_respects_gitignore() {
        let dir = std::env::temp_dir().join(format!("pide-goto-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target\n").unwrap();
        std::fs::write(dir.join("src/a.rs"), "").unwrap();
        std::fs::write(dir.join("target/debug/x"), "").unwrap();
        std::fs::write(dir.join(".git/HEAD"), "").unwrap();
        let mut files = list_files(&dir);
        files.sort();
        assert_eq!(
            files,
            vec![".gitignore".to_string(), "src/a.rs".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
