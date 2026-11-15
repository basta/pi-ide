//! LaTeX build and SyncTeX. Saving a LaTeX file asks texlab to build the
//! document (`textDocument/build`: latexmk -pdf -synctex=1); errors from the
//! LaTeX log come back from texlab as diagnostics (Problems). When the build
//! is done the PDF opens beside the source, or reloads. `latex.forward_search`
//! scrolls the PDF to the cursor's line (`synctex view`); the PDF panel does
//! the way back (Cmd+click → `synctex edit`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use pide_core::commands::{CommandRegistry, CommandSpec};
use pide_core::opener::{PdfRequest, PdfSpot};
use pide_core::Services;

use crate::EditorPanel;

pub fn init(cx: &mut App) {
    CommandRegistry::register(
        cx,
        CommandSpec::new("latex.forward_search", "LaTeX: Show in PDF (SyncTeX)")
            .key("cmd-alt-shift-j")
            .in_context(pide_core::commands::EDITOR),
        forward_search,
    );
}

pub fn is_latex(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(crate::latex::is_latex_ext)
}

/// Documents building right now (a second save waits for the first).
#[derive(Default)]
struct Building(HashSet<PathBuf>);
impl Global for Building {}

/// Told the user once that building needs texlab.
#[derive(Default)]
struct ToldNoServer(bool);
impl Global for ToldNoServer {}

/// After a LaTeX file is saved: build its document, then show the PDF.
pub(crate) fn build_on_save(panel: &EditorPanel, window: &mut Window, cx: &mut Context<EditorPanel>) {
    let path = panel.path.clone();
    let project = panel.workspace.read(cx).root().to_path_buf();
    let store = pide_lsp::LspStore::for_workspace(&panel.workspace, cx);
    let find = cx
        .background_executor()
        .spawn(async move { root_document(&path, &project).map(|r| (path, r)) });
    cx.spawn_in(window, async move |_, cx| {
        let Some((path, root)) = find.await else {
            return;
        };
        let started = cx
            .update(|_, cx| cx.default_global::<Building>().0.insert(root.clone()))
            .unwrap_or(false);
        if !started {
            return;
        }
        let uri = pide_lsp::process::file_uri(&path);
        let build = cx.update(|_, cx| {
            store.update(cx, |s, cx| {
                s.request(
                    &path,
                    None,
                    "textDocument/build",
                    serde_json::json!({ "textDocument": { "uri": uri } }),
                    cx,
                )
            })
        });
        let result = match build {
            Ok(task) => task.await,
            Err(_) => return,
        };
        let _ = cx.update(|window, cx| {
            cx.default_global::<Building>().0.remove(&root);
            let status = match &result {
                Ok(v) => v["status"].as_u64(),
                Err(_) => None,
            };
            let pdf = root.with_extension("pdf");
            match (status, &result) {
                // 0 = success, 1 = LaTeX errors (a PDF may still be there).
                (Some(s @ (0 | 1)), _) => {
                    if pdf.is_file() {
                        pide_core::opener::show_pdf(
                            PdfRequest {
                                path: pdf,
                                beside: true,
                                reload: true,
                                at: None,
                                find: Vec::new(),
                                raise: true,
                            },
                            window,
                            cx,
                        );
                    }
                    if s == 1 {
                        window.push_notification(
                            Notification::warning("LaTeX build finished with errors (see Problems)."),
                            cx,
                        );
                    }
                }
                (Some(2), _) => window.push_notification(
                    Notification::error("The LaTeX build couldn't run. Is latexmk installed?"),
                    cx,
                ),
                (Some(_), _) => {} // cancelled
                (None, Err(e)) => {
                    let msg = format!("{e:#}");
                    if msg.contains("no language server running") {
                        let told = &mut cx.default_global::<ToldNoServer>().0;
                        if !*told {
                            *told = true;
                            window.push_notification(
                                Notification::info(
                                    "Building LaTeX on save needs texlab: install it from Settings → Language Servers.",
                                ),
                                cx,
                            );
                        }
                    } else {
                        window.push_notification(Notification::error(msg), cx);
                    }
                }
                (None, Ok(_)) => {}
            }
        });
    })
    .detach();
}

/// Editor command: show the cursor's place in the document's PDF.
fn forward_search(window: &mut Window, cx: &mut App) {
    let Some(ctx) = pide_core::context::current(window, cx) else {
        return;
    };
    let project = Services::of(window, cx).workspace.read(cx).root().to_path_buf();
    let file = project.join(&ctx.path);
    if !is_latex(&file) {
        window.push_notification(Notification::info("Open a LaTeX file first."), cx);
        return;
    }
    let line = ctx.start.0;
    let work = cx.background_executor().spawn(async move {
        let root = root_document(&file, &project)
            .ok_or_else(|| "Can't tell which document includes this file.".to_string())?;
        let pdf = root.with_extension("pdf");
        if !pdf.is_file() {
            return Err("There's no PDF yet: save to build it.".to_string());
        }
        let spot = synctex_view(&pdf, &file, line)
            .ok_or_else(|| "SyncTeX doesn't know this line (rebuild?).".to_string())?;
        Ok((pdf, spot))
    });
    window
        .spawn(cx, async move |cx| {
            let r = work.await;
            let _ = cx.update(|window, cx| match r {
                Ok((pdf, spot)) => pide_core::opener::show_pdf(
                    PdfRequest {
                        path: pdf,
                        beside: true,
                        reload: false,
                        at: Some(spot),
                        find: Vec::new(),
                        raise: true,
                    },
                    window,
                    cx,
                ),
                Err(e) => window.push_notification(Notification::info(e), cx),
            });
        })
        .detach();
}

/// The document a LaTeX file belongs to (the file `latexmk` builds). Blocking.
///
/// A `% !TEX root = …` comment wins; a file with `\documentclass` is its own
/// document; otherwise the nearest document (same folder or above, up to the
/// project) that `\input`s / `\include`s it, the most recently built first.
pub fn root_document(file: &Path, project: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(file).ok()?;
    let dir = file.parent()?;
    if let Some(r) = magic_root(&text) {
        return Some(normalize(&dir.join(r)));
    }
    if declares_document(&text) {
        return Some(file.to_path_buf());
    }
    let mut at = Some(dir);
    while let Some(d) = at {
        let mut found: Vec<PathBuf> = std::fs::read_dir(d)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "tex") && p != file)
            .filter(|p| {
                std::fs::read_to_string(p).is_ok_and(|t| {
                    declares_document(&t) && includes(&t, file.strip_prefix(d).unwrap_or(file))
                })
            })
            .collect();
        found.sort_by_key(|p| {
            std::cmp::Reverse(
                std::fs::metadata(p.with_extension("pdf"))
                    .and_then(|m| m.modified())
                    .ok(),
            )
        });
        if let Some(r) = found.into_iter().next() {
            return Some(r);
        }
        if d == project {
            break;
        }
        at = d.parent().filter(|p| p.starts_with(project));
    }
    None
}

/// `% !TEX root = ../main.tex` (any case, spaces optional), in the first lines.
fn magic_root(text: &str) -> Option<String> {
    text.lines().take(20).find_map(|l| {
        let l = l.trim_start().strip_prefix('%')?.trim_start();
        let l = l.strip_prefix('!')?.trim_start();
        let (key, value) = l.split_once('=')?;
        let key: String = key.split_whitespace().collect::<Vec<_>>().join(" ");
        key.eq_ignore_ascii_case("tex root")
            .then(|| value.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

fn uncommented(line: &str) -> &str {
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        if c == '%' && prev != '\\' {
            return &line[..i];
        }
        prev = c;
    }
    line
}

fn declares_document(text: &str) -> bool {
    text.lines()
        .any(|l| uncommented(l).trim_start().starts_with("\\documentclass"))
}

/// Whether `text` inputs `rel` (`sections/intro.tex` as `\input{sections/intro}`).
fn includes(text: &str, rel: &Path) -> bool {
    let rel = rel.to_string_lossy().replace('\\', "/");
    let stem = rel.strip_suffix(".tex").unwrap_or(&rel);
    text.lines().map(uncommented).any(|l| {
        ["\\input{", "\\include{", "\\subfile{", "\\import{"]
            .iter()
            .any(|cmd| {
                l.match_indices(cmd).any(|(i, _)| {
                    let arg = &l[i + cmd.len()..];
                    let arg = arg.split('}').next().unwrap_or("").trim();
                    let arg = arg.strip_prefix("./").unwrap_or(arg);
                    arg == stem || arg == rel
                })
            })
    })
}

/// Drop `.` and `..` without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn synctex() -> Command {
    Command::new(pide_runtime::process::find_command("synctex").unwrap_or_else(|| "synctex".into()))
}

/// Where a source line (1-based) is in the PDF. Blocking.
pub fn synctex_view(pdf: &Path, file: &Path, line: u32) -> Option<PdfSpot> {
    let out = synctex()
        .arg("view")
        .arg("-i")
        .arg(format!("{line}:0:{}", file.display()))
        .arg("-o")
        .arg(pdf)
        .output()
        .ok()?;
    parse_view(&String::from_utf8_lossy(&out.stdout))
}

fn parse_view(out: &str) -> Option<PdfSpot> {
    let field = |name: &str| {
        out.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':').map(str::trim))
    };
    let page: usize = field("Page")?.parse().ok()?;
    Some(PdfSpot {
        page: page.checked_sub(1)?,
        x: field("x")?.parse().ok()?,
        y: field("y")?.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn magic_comment() {
        assert_eq!(magic_root("% !TEX root = ../main.tex\n").as_deref(), Some("../main.tex"));
        assert_eq!(magic_root("%!tex root=main.tex").as_deref(), Some("main.tex"));
        assert_eq!(magic_root("% TEX root = x"), None);
    }

    #[::core::prelude::v1::test]
    fn documents_and_inputs() {
        assert!(declares_document("% x\n\\documentclass[a4]{article}\n"));
        assert!(!declares_document("% \\documentclass{article}\n"));
        let main = "\\input{sections/intro}\n% \\input{sections/old}\n\\include{./ch2.tex}";
        assert!(includes(main, Path::new("sections/intro.tex")));
        assert!(!includes(main, Path::new("sections/old.tex")));
        assert!(includes(main, Path::new("ch2.tex")));
        assert!(!includes(main, Path::new("intro.tex")));
    }

    #[::core::prelude::v1::test]
    fn finds_the_including_document() {
        let dir = std::env::temp_dir().join(format!("pide-tex-root-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sections")).unwrap();
        std::fs::write(dir.join("report.tex"), "\\documentclass{article}\n").unwrap();
        std::fs::write(
            dir.join("findings.tex"),
            "\\documentclass{article}\n\\input{sections/one}\n",
        )
        .unwrap();
        std::fs::write(dir.join("sections/one.tex"), "\\section{One}\n").unwrap();
        std::fs::write(dir.join("sections/two.tex"), "% !TEX root = ../report.tex\n").unwrap();
        assert_eq!(
            root_document(&dir.join("sections/one.tex"), &dir),
            Some(dir.join("findings.tex"))
        );
        assert_eq!(
            root_document(&dir.join("sections/two.tex"), &dir),
            Some(dir.join("report.tex"))
        );
        assert_eq!(
            root_document(&dir.join("report.tex"), &dir),
            Some(dir.join("report.tex"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[::core::prelude::v1::test]
    fn view_output() {
        let out = "SyncTeX result begin\nOutput:report.pdf\nPage:2\nx:71.99\ny:312.14\nh:72\nv:314\nSyncTeX result end\n";
        assert_eq!(
            parse_view(out),
            Some(PdfSpot {
                page: 1,
                x: 71.99,
                y: 312.14
            })
        );
        assert_eq!(parse_view("SyncTeX Warning: No tag"), None);
    }
}
