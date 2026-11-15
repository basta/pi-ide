//! Typst highlighting: GPUI Kit has no Typst grammar, so we register one
//! (uben0's tree-sitter-typst, MIT, as `codebook-tree-sitter-typst`) at
//! startup, like LaTeX (`latex.rs`), with our own query mapped to GPUI Kit's
//! theme captures. The grammar's own query is Helix's (first match wins);
//! here later patterns win, so general ones come first.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};
use gpui_kit::component::notification::Notification;
use gpui_kit::{Context, Global, Window};
use pide_core::opener::PdfRequest;

use crate::EditorPanel;

pub const NAME: &str = "typst";

pub const HIGHLIGHTS: &str = r##"
(ident) @variable
(number) @number
(string) @string
(bool) @boolean
[(none) (auto)] @constant
(escape) @string.escape
["(" ")" "{" "}" "[" "]"] @punctuation.bracket
["," ";" ":"] @punctuation.delimiter
"#" @punctuation.special

(call item: (ident) @function)
(call item: (field field: (ident) @function))
(tagged field: (ident) @property)

[
  "let" "if" "else" "while" "for" "in" "import" "include"
  "show" "set" "return" "break" "continue" "context" "as"
  "and" "or" "not"
] @keyword

[(add) (sub) (mul) (div) (cmp)] @operator

(item "-" @punctuation.list_marker)
(term ["/" ":"] @punctuation.list_marker)
(heading) @title
(emph) @emphasis
(strong) @emphasis.strong
(url) @link_uri
[(label) (ref)] @attribute
[(shorthand) (linebreak) (symbol)] @string.special.symbol
(math) @string.special
[(raw_span) (raw_blck)] @text.literal

(comment) @comment
"##;

/// File extensions that open as Typst.
pub fn is_typst_ext(ext: &str) -> bool {
    ext == "typ"
}

pub fn is_typst(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(is_typst_ext)
}

pub fn init() {
    LanguageRegistry::singleton().register(
        NAME,
        &LanguageConfig::new(
            NAME,
            codebook_tree_sitter_typst::LANGUAGE.into(),
            vec![],
            HIGHLIGHTS,
            "",
            "",
        ),
    );
}

/// Documents exporting right now (a second save waits for the first).
#[derive(Default)]
struct Exporting(HashSet<PathBuf>);
impl Global for Exporting {}

/// Told the user once that the PDF needs tinymist.
#[derive(Default)]
struct ToldNoServer(bool);
impl Global for ToldNoServer {}

/// After a Typst file is saved: have tinymist export its document's PDF, then
/// show it beside the source (or reload it), scrolled to what changed since
/// the last export. Compile errors are already in Problems: tinymist
/// publishes them as you type.
pub(crate) fn export_on_save(
    panel: &mut EditorPanel,
    window: &mut Window,
    cx: &mut Context<EditorPanel>,
) {
    let text = panel.editor.read(cx).text().to_string();
    let cursor = panel.editor.read(cx).cursor_position().line as usize;
    let phrases = panel
        .typst_base
        .replace(text.clone())
        .and_then(|old| changed_line(&old, &text, cursor))
        .map(|line| phrases_near(&text, line))
        .unwrap_or_default();
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
            .update(|_, cx| cx.default_global::<Exporting>().0.insert(root.clone()))
            .unwrap_or(false);
        if !started {
            return;
        }
        let export = cx.update(|_, cx| {
            store.update(cx, |s, cx| {
                s.request(
                    &path,
                    None,
                    "workspace/executeCommand",
                    serde_json::json!({
                        "command": "tinymist.exportPdf",
                        "arguments": [root.to_string_lossy()],
                    }),
                    cx,
                )
            })
        });
        let result = match export {
            Ok(task) => task.await,
            Err(_) => return,
        };
        let _ = cx.update(|window, cx| {
            cx.default_global::<Exporting>().0.remove(&root);
            match result {
                Ok(v) => {
                    let pdf = exported_path(&v).unwrap_or_else(|| root.with_extension("pdf"));
                    if pdf.is_file() {
                        pide_core::opener::show_pdf(
                            PdfRequest {
                                path: pdf,
                                beside: true,
                                reload: true,
                                at: None,
                                find: phrases,
                            },
                            window,
                            cx,
                        );
                    }
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    if msg.contains("no language server running") {
                        let told = &mut cx.default_global::<ToldNoServer>().0;
                        if !*told {
                            *told = true;
                            window.push_notification(
                                Notification::info(
                                    "The Typst PDF on save needs tinymist: install it from Settings → Language Servers.",
                                ),
                                cx,
                            );
                        }
                    } else {
                        // Usually a compile error, which Problems shows in full.
                        log::info!("typst export: {msg}");
                        window.push_notification(
                            Notification::warning("Typst couldn't compile the PDF (see Problems)."),
                            cx,
                        );
                    }
                }
            }
        });
    })
    .detach();
}

/// Where pinning the document stands, per editor tab.
#[derive(Clone, Copy, Default, PartialEq)]
pub(crate) enum Pin {
    #[default]
    None,
    InFlight,
    Done,
    /// The server wasn't up yet: retry when its state changes.
    Failed,
}

/// Tell tinymist which document to compile (`tinymist.pinMain`): the one the
/// file belongs to. Without it tinymist compiles whichever file was focused
/// last, so after opening an included chapter it stops recompiling the main
/// file and its errors never reach Problems.
pub(crate) fn pin_main(
    panel: &mut EditorPanel,
    window: &mut Window,
    cx: &mut Context<EditorPanel>,
) {
    if panel.typst_pin == Pin::InFlight {
        return;
    }
    panel.typst_pin = Pin::InFlight;
    let path = panel.path.clone();
    let project = panel.workspace.read(cx).root().to_path_buf();
    let store = pide_lsp::LspStore::for_workspace(&panel.workspace, cx);
    let find = cx
        .background_executor()
        .spawn(async move { root_document(&path, &project).map(|r| (path, r)) });
    cx.spawn_in(window, async move |this, cx| {
        let pinned = match find.await {
            Some((path, root)) => {
                let req = cx.update(|_, cx| {
                    store.update(cx, |s, cx| {
                        s.request(
                            &path,
                            None,
                            "workspace/executeCommand",
                            serde_json::json!({
                                "command": "tinymist.pinMain",
                                "arguments": [root.to_string_lossy()],
                            }),
                            cx,
                        )
                    })
                });
                match req {
                    Ok(task) => task.await.is_ok(),
                    Err(_) => return,
                }
            }
            None => false,
        };
        let _ = this.update(cx, |p, _| {
            p.typst_pin = if pinned { Pin::Done } else { Pin::Failed };
        });
    })
    .detach();
}

/// The line (0-based, in `new`) of the change nearest the cursor, if any.
fn changed_line(old: &str, new: &str, cursor: usize) -> Option<usize> {
    let diff = similar::TextDiff::from_lines(old, new);
    let last = new.lines().count().saturating_sub(1);
    diff.ops()
        .iter()
        .filter(|op| op.tag() != similar::DiffTag::Equal)
        .map(|op| {
            let r = op.new_range();
            // A deletion: the line after it (or the last one).
            let (start, end) = (r.start.min(last), r.end.max(r.start + 1).min(last + 1));
            let line = cursor.clamp(start, end.saturating_sub(1).max(start));
            (line, line.abs_diff(cursor))
        })
        .min_by_key(|&(_, d)| d)
        .map(|(l, _)| l)
}

/// Phrases to look for in the PDF: plain prose from the changed line, then
/// from the lines around it (nearest first). Markup is dropped; anything Typst
/// renders differently (code, math, quotes, dashes) splits a phrase.
fn phrases_near(text: &str, line: usize) -> Vec<String> {
    const MAX: usize = 12;
    let lines: Vec<&str> = text.lines().collect();
    let mut order = vec![line];
    for d in 1..=6 {
        order.push(line + d);
        if let Some(l) = line.checked_sub(d) {
            order.push(l);
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut single: Vec<String> = Vec::new();
    for l in order {
        let Some(src) = lines.get(l) else { continue };
        for run in prose_runs(src) {
            if run.len() >= 3 {
                // Short windows: a phrase must not cross a line break in the PDF.
                for w in run.chunks(4) {
                    let w = if w.len() < 3 { &run[run.len() - 3..] } else { w };
                    out.push(w.join(" "));
                }
            } else if run.join(" ").chars().count() >= 10 {
                out.push(run.join(" "));
            } else if let Some(w) = run.iter().find(|w| w.chars().count() >= 8) {
                single.push(w.clone());
            }
        }
    }
    out.extend(single);
    out.dedup();
    out.truncate(MAX);
    out
}

/// The runs of plain words in a Typst source line.
fn prose_runs(line: &str) -> Vec<Vec<String>> {
    let mut line = line.trim_start();
    if let Some(i) = line.find("//").filter(|&i| i == 0 || line[..i].ends_with(char::is_whitespace)) {
        line = &line[..i];
    }
    // Code lines render nothing as written.
    if let Some(code) = line.strip_prefix('#') {
        let word = code.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("");
        if matches!(word, "let" | "set" | "show" | "import" | "include" | "if" | "for" | "while") {
            return vec![];
        }
    }
    let line = line.trim_start_matches('=').trim_start();
    let line = ["- ", "+ ", "/ "]
        .iter()
        .find_map(|m| line.strip_prefix(m))
        .unwrap_or(line);
    let mut runs = vec![];
    let mut cur: Vec<String> = vec![];
    let mut in_math = false;
    for token in line.split_whitespace() {
        let dollars = token.matches('$').count();
        let breaks = in_math
            || dollars > 0
            || token.starts_with(['#', '@', '<', '\\'])
            || token.contains(['"', '\'', '`', '(', ')', '[', ']', '{', '}', '~', '<', '>'])
            || token.contains("--")
            || token.contains("...");
        if dollars % 2 == 1 {
            in_math = !in_math;
        }
        let word: String = token.chars().filter(|c| !matches!(c, '*' | '_')).collect();
        if breaks || !word.chars().any(char::is_alphanumeric) {
            if !cur.is_empty() {
                runs.push(std::mem::take(&mut cur));
            }
            continue;
        }
        cur.push(word);
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    runs
}

/// The PDF tinymist wrote: `{ "path": … }` (0.13+), or a plain path (older).
fn exported_path(v: &serde_json::Value) -> Option<PathBuf> {
    v.get("path")
        .and_then(|p| p.as_str())
        .or_else(|| v.as_str())
        .map(PathBuf::from)
}

/// The document a Typst file belongs to: the nearest `.typ` (same folder or
/// above, up to the project) that `#include`s it, else the file itself. Blocking.
pub fn root_document(file: &Path, project: &Path) -> Option<PathBuf> {
    let mut at = file.parent();
    while let Some(d) = at {
        let found = std::fs::read_dir(d)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "typ") && p != file)
            .find(|p| {
                let rel = file.strip_prefix(d).unwrap_or(file);
                std::fs::read_to_string(p).is_ok_and(|t| includes(&t, rel))
            });
        if found.is_some() {
            return found;
        }
        if d == project {
            break;
        }
        at = d.parent().filter(|p| p.starts_with(project));
    }
    Some(file.to_path_buf())
}

/// Whether `text` has `#include "rel"` (also `./rel`), outside `//` comments.
fn includes(text: &str, rel: &Path) -> bool {
    let rel = rel.to_string_lossy().replace('\\', "/");
    text.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .any(|l| {
            l.match_indices("include").any(|(i, _)| {
                let arg = l[i + "include".len()..].trim_start();
                let Some(arg) = arg.strip_prefix('"') else {
                    return false;
                };
                let arg = arg.split('"').next().unwrap_or("");
                arg.strip_prefix("./").unwrap_or(arg) == rel
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"#set page(paper: "a4")
#import "@preview/cetz:0.3.0": canvas
// a comment
= Introduction <intro>
As shown in @intro, $x^2 + y$ is *bold* and _soft_.
- an item with `code`
#let total = 3 + 4
#if total > 5 [big] else [small]
"##;

    fn captures() -> Vec<(String, String)> {
        let language: tree_sitter::Language = codebook_tree_sitter_typst::LANGUAGE.into();
        let query = tree_sitter::Query::new(&language, HIGHLIGHTS).expect("query compiles");
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(SAMPLE, None).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut out = Vec::new();
        use tree_sitter::StreamingIterator as _;
        let mut it = cursor.captures(&query, tree.root_node(), SAMPLE.as_bytes());
        while let Some((m, ix)) = it.next() {
            let c = m.captures[*ix];
            let name = query.capture_names()[c.index as usize].to_string();
            out.push((name, SAMPLE[c.node.byte_range()].to_string()));
        }
        out
    }

    fn has(caps: &[(String, String)], name: &str, text: &str) -> bool {
        caps.iter().any(|(n, t)| n == name && t.trim_end() == text)
    }

    #[test]
    fn query_compiles_and_highlights() {
        let caps = captures();
        assert!(has(&caps, "comment", "// a comment"), "{caps:?}");
        assert!(has(&caps, "title", "= Introduction <intro>"), "{caps:?}");
        // After the heading: the label inside it keeps its own color.
        assert_eq!(
            caps.iter()
                .rev()
                .find(|(_, t)| t == "<intro>")
                .map(|(n, _)| n.as_str()),
            Some("attribute")
        );
        assert!(has(&caps, "attribute", "@intro"));
        assert!(has(&caps, "function", "page"));
        assert!(has(&caps, "property", "paper"));
        assert!(has(&caps, "string", "\"a4\""));
        assert!(has(&caps, "keyword", "import"));
        assert!(has(&caps, "keyword", "let"));
        assert!(has(&caps, "keyword", "if"));
        assert!(has(&caps, "number", "3"));
        assert!(has(&caps, "string.special", "$x^2 + y$"));
        assert!(has(&caps, "emphasis.strong", "*bold*"));
        assert!(has(&caps, "emphasis", "_soft_"));
        assert!(has(&caps, "text.literal", "`code`"));
    }

    #[test]
    fn include_lines() {
        let main = "#include \"chapters/one.typ\"\n// #include \"chapters/old.typ\"\n#include \"./two.typ\"";
        assert!(includes(main, Path::new("chapters/one.typ")));
        assert!(!includes(main, Path::new("chapters/old.typ")));
        assert!(includes(main, Path::new("two.typ")));
        assert!(!includes(main, Path::new("one.typ")));
    }

    #[test]
    fn finds_the_including_document() {
        let dir = std::env::temp_dir().join(format!("pide-typst-root-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("chapters")).unwrap();
        std::fs::write(dir.join("main.typ"), "#include \"chapters/one.typ\"\n").unwrap();
        std::fs::write(dir.join("chapters/one.typ"), "= One\n").unwrap();
        std::fs::write(dir.join("notes.typ"), "= Notes\n").unwrap();
        assert_eq!(
            root_document(&dir.join("chapters/one.typ"), &dir),
            Some(dir.join("main.typ"))
        );
        assert_eq!(
            root_document(&dir.join("notes.typ"), &dir),
            Some(dir.join("notes.typ"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn change_near_the_cursor() {
        let old = "a\nb\nc\nd\ne\n";
        assert_eq!(changed_line(old, old, 0), None);
        assert_eq!(changed_line(old, "a\nB\nc\nd\ne\n", 4), Some(1));
        // Two changes: the one nearer the cursor.
        assert_eq!(changed_line(old, "A\nb\nc\nd\nE\n", 3), Some(4));
        assert_eq!(changed_line(old, "A\nb\nc\nd\nE\n", 1), Some(0));
        // Lines added: the cursor's own line inside them.
        assert_eq!(changed_line(old, "a\nb\nx\ny\nz\nc\nd\ne\n", 3), Some(3));
        // A deletion: the line after it.
        assert_eq!(changed_line(old, "a\nb\nd\ne\n", 0), Some(2));
        assert_eq!(changed_line(old, "a\nb\nc\nd\n", 3), Some(3));
    }

    #[test]
    fn prose_without_markup() {
        let words = |l: &str| -> Vec<String> {
            prose_runs(l).into_iter().map(|r| r.join(" ")).collect()
        };
        assert_eq!(
            words("As shown in @intro, $x^2 + y$ is *bold* and _soft_."),
            ["As shown in", "is bold and soft."]
        );
        assert_eq!(words("= Introduction <intro>"), ["Introduction"]);
        assert_eq!(words("- an item with `code`"), ["an item with"]);
        assert!(words("#let total = 3 + 4").is_empty());
        assert!(words("#set page(paper: \"a4\")").is_empty());
        assert_eq!(words("Plain text // a comment"), ["Plain text"]);
        assert_eq!(words("See https://typst.app now"), ["See https://typst.app now"]);
    }

    #[test]
    fn phrases_changed_line_first() {
        let text = "= Results\nWe measured the speed of light twice today.\n#let x = 1\n";
        let p = phrases_near(text, 2);
        assert_eq!(p[0], "We measured the speed");
        assert_eq!(p[1], "of light twice today.");
        assert_eq!(p.len(), 2, "{p:?}"); // "Results" is too short alone
    }

    #[test]
    fn export_response() {
        let v = serde_json::json!({ "path": "/a/main.pdf", "data": null });
        assert_eq!(exported_path(&v), Some(PathBuf::from("/a/main.pdf")));
        assert_eq!(
            exported_path(&serde_json::json!("/a/b.pdf")),
            Some(PathBuf::from("/a/b.pdf"))
        );
        assert_eq!(exported_path(&serde_json::Value::Null), None);
    }
}
