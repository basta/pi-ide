//! Typst highlighting: GPUI Kit has no Typst grammar, so we register one
//! (uben0's tree-sitter-typst, MIT, as `codebook-tree-sitter-typst`) at
//! startup, like LaTeX (`latex.rs`), with our own query mapped to GPUI Kit's
//! theme captures. The grammar's own query is Helix's (first match wins);
//! here later patterns win, so general ones come first.

use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, Context, Global, Window};
use pide_core::opener::PdfRequest;
use pide_core::settings::Settings;

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

/// What asked for an export.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Trigger {
    /// The file changed on disk (pi's edit): refresh an open PDF tab only.
    Disk,
    /// Typing: refresh the PDF quietly (opens it beside if it isn't open).
    Type,
    /// A save: show the PDF and report errors.
    Save,
}

/// Exports of one editor tab: one at a time; changes meanwhile ask for one more.
#[derive(Default)]
pub(crate) struct Exports {
    busy: bool,
    /// Another export is wanted when this one ends (the strongest trigger asked).
    again: Option<Trigger>,
    /// An export as you type is queued for the end of this round of edits.
    queued: bool,
    /// Bumped per export, so a timed-out one that answers late is ignored.
    generation: u64,
    /// An export as you type was held back: the document doesn't compile.
    held: bool,
}

/// tinymist never answers an export of a document that doesn't compile (seen
/// with 0.15): give up on it after this long.
const EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Whether tinymist reports errors in any Typst file (the document can't
/// export then; it doesn't answer the request at all).
fn has_errors(store: &gpui_kit::Entity<pide_lsp::LspStore>, cx: &App) -> bool {
    store.read(cx).diagnostics().iter().any(|(p, d)| {
        is_typst(p)
            && d.items
                .iter()
                .any(|i| i.severity == Some(lsp_types::DiagnosticSeverity::ERROR))
    })
}

/// New diagnostics: an export held back for errors goes out once they're gone.
pub(crate) fn diagnostics_changed(
    panel: &mut EditorPanel,
    window: &mut Window,
    cx: &mut Context<EditorPanel>,
) {
    if panel.typst_exports.held {
        panel.typst_exports.held = false;
        export_on_type(panel, window, cx);
    }
}

/// Told the user once that the PDF needs tinymist.
#[derive(Default)]
struct ToldNoServer(bool);
impl Global for ToldNoServer {}

fn settings(cx: &App) -> (bool, bool) {
    cx.try_global::<Settings>()
        .map(|s| (s.typst_pdf_on_type, s.typst_pdf_follow))
        .unwrap_or((true, true))
}

/// Whether the PDF is exported as you type (`typst.pdf_on_type`).
pub(crate) fn on_type(cx: &App) -> bool {
    settings(cx).0
}

/// The text changed (typing, undo, an outside change): export the PDF from
/// the editor's text, without saving. Edits made in one go (an outside change
/// applied line by line) export once, after the last of them; edits during an
/// export, once it ends. Experimental: `typst.pdf_on_type`.
pub(crate) fn export_on_type(
    panel: &mut EditorPanel,
    window: &mut Window,
    cx: &mut Context<EditorPanel>,
) {
    if panel.typst_exports.queued || panel.buffer.is_none() || !on_type(cx) {
        return;
    }
    panel.typst_exports.queued = true;
    cx.spawn_in(window, async move |this, cx| {
        let _ = this.update_in(cx, |p, window, cx| {
            p.typst_exports.queued = false;
            let text = p.editor.read(cx).text().to_string();
            // Loading the file, or the same text again.
            if p.typst_base.as_deref() != Some(text.as_str()) {
                export(p, Trigger::Type, window, cx);
            }
        });
    })
    .detach();
}

/// After a Typst file is saved.
pub(crate) fn export_on_save(
    panel: &mut EditorPanel,
    window: &mut Window,
    cx: &mut Context<EditorPanel>,
) {
    export(panel, Trigger::Save, window, cx);
}

/// The file changed on disk (pi edited it, or another editor): rebuild the PDF
/// so it doesn't show the old text, quietly (no raise, no toast: Problems has
/// any errors; no PDF tab is opened if none is). Also when export as you type is off.
pub(crate) fn export_on_disk_change(
    panel: &mut EditorPanel,
    window: &mut Window,
    cx: &mut Context<EditorPanel>,
) {
    export(panel, Trigger::Disk, window, cx);
}

/// Projects already asked about ignoring generated PDFs (this run).
#[derive(Default)]
struct AskedIgnore(std::collections::HashSet<PathBuf>);
impl Global for AskedIgnore {}

/// The generated PDF would show up as an unversioned file (a git project, not
/// ignored, not tracked): offer once to add `*.pdf` to the project's .gitignore.
fn offer_ignore(project: &Path, pdf: &Path, window: &mut Window, cx: &mut App) {
    if !cx
        .default_global::<AskedIgnore>()
        .0
        .insert(project.to_path_buf())
    {
        return;
    }
    let (project, pdf) = (project.to_path_buf(), pdf.to_path_buf());
    let check = cx.background_executor().spawn({
        let (project, pdf) = (project.clone(), pdf.clone());
        async move {
            let git = |args: &[&str]| {
                std::process::Command::new("git")
                    .current_dir(&project)
                    .args(args)
                    .arg(&pdf)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .ok()
                    .and_then(|s| s.code())
            };
            // check-ignore: 0 ignored, 1 not ignored, 128 not a repo.
            git(&["check-ignore", "-q"]) == Some(1)
                && git(&["ls-files", "--error-unmatch"]) != Some(0)
        }
    });
    window
        .spawn(cx, async move |cx| {
            if !check.await {
                return;
            }
            let _ = cx.update(|window, cx| {
                let name = pdf
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let note = Notification::info(format!(
                    "{name} is generated from Typst and would be listed as a new file to commit."
                ))
                .action(move |_, _, cx| {
                    let note = cx.entity().downgrade();
                    let project = project.clone();
                    gpui_kit::component::button::Button::new("typst-ignore-pdf")
                        .label("Add *.pdf to .gitignore")
                        .on_click(move |_, window, cx| {
                            let path = project.join(".gitignore");
                            let mut text = std::fs::read_to_string(&path).unwrap_or_default();
                            if !text.is_empty() && !text.ends_with('\n') {
                                text.push('\n');
                            }
                            text.push_str("*.pdf\n");
                            if let Err(e) = std::fs::write(&path, text) {
                                log::warn!("writing {}: {e}", path.display());
                            }
                            let _ = note.update(cx, |n, cx| n.dismiss(window, cx));
                        })
                });
                window.push_notification(note, cx);
            });
        })
        .detach();
}

/// Have tinymist export the document's PDF from the text it has (the editor
/// keeps it in sync, saved or not), then show it beside the source (or reload
/// it), scrolled to what changed since the last export. Compile errors are
/// already in Problems: tinymist publishes them as you type.
fn export(
    panel: &mut EditorPanel,
    trigger: Trigger,
    window: &mut Window,
    cx: &mut Context<EditorPanel>,
) {
    if panel.typst_exports.busy {
        let again = panel.typst_exports.again.get_or_insert(trigger);
        *again = (*again).max(trigger);
        return;
    }
    let on_save = trigger == Trigger::Save;
    let store = pide_lsp::LspStore::for_workspace(&panel.workspace, cx);
    if !on_save && has_errors(&store, cx) {
        panel.typst_exports.held = true;
        return;
    }
    panel.typst_exports.busy = true;
    panel.typst_exports.generation += 1;
    let generation = panel.typst_exports.generation;
    let follow = settings(cx).1;
    let text = panel.editor.read(cx).text().to_string();
    let cursor = panel.editor.read(cx).cursor_position().line as usize;
    let phrases = panel
        .typst_base
        .replace(text.clone())
        .filter(|_| follow)
        .and_then(|old| changed_line(&old, &text, cursor))
        .map(|line| phrases_near(&text, line))
        .unwrap_or_default();
    let path = panel.path.clone();
    let project = panel.workspace.read(cx).root().to_path_buf();
    let find = cx.background_executor().spawn({
        let project = project.clone();
        async move { root_document(&path, &project).map(|r| (path, r)) }
    });
    // Free the tab for the next export if tinymist doesn't answer this one.
    cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(EXPORT_TIMEOUT).await;
        let _ = this.update_in(cx, |p, window, cx| {
            if p.typst_exports.busy && p.typst_exports.generation == generation {
                log::info!("typst export: no answer from tinymist, giving up");
                finished(p, window, cx);
            }
        });
    })
    .detach();
    cx.spawn_in(window, async move |this, cx| {
        if let Some((path, root)) = find.await {
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
            if let Ok(task) = export {
                let result = task.await;
                let _ = cx.update(|window, cx| {
                    show(result, root, &project, phrases, trigger, window, cx)
                });
            }
        }
        let _ = this.update_in(cx, |p, window, cx| {
            if p.typst_exports.generation == generation {
                finished(p, window, cx);
            }
        });
    })
    .detach();
}

/// The export in flight is done (or given up): run the one asked for meanwhile.
fn finished(panel: &mut EditorPanel, window: &mut Window, cx: &mut Context<EditorPanel>) {
    panel.typst_exports.busy = false;
    panel.typst_exports.generation += 1;
    if let Some(trigger) = panel.typst_exports.again.take() {
        export(panel, trigger, window, cx);
    }
}

fn show(
    result: anyhow::Result<serde_json::Value>,
    root: PathBuf,
    project: &Path,
    phrases: Vec<String>,
    trigger: Trigger,
    window: &mut Window,
    cx: &mut App,
) {
    let on_save = trigger == Trigger::Save;
    match result {
        Ok(v) => {
            let pdf = exported_path(&v).unwrap_or_else(|| root.with_extension("pdf"));
            offer_ignore(project, &pdf, window, cx);
            if pdf.is_file() {
                pide_core::opener::show_pdf(
                    PdfRequest {
                        path: pdf,
                        beside: true,
                        reload: true,
                        at: None,
                        find: phrases,
                        // As you type: refresh it where it is, quietly.
                        raise: on_save,
                        // After an outside change: only refresh an open tab.
                        open: trigger != Trigger::Disk,
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
                            "The Typst PDF needs tinymist: install it from Settings → Language Servers.",
                        ),
                        cx,
                    );
                }
            } else if on_save {
                // Usually a compile error, which Problems shows in full.
                log::info!("typst export: {msg}");
                window.push_notification(
                    Notification::warning("Typst couldn't compile the PDF (see Problems)."),
                    cx,
                );
            } else {
                // Mid-word while typing: normal, Problems has it.
                log::debug!("typst export as you type: {msg}");
            }
        }
    }
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
    let find = cx.background_executor().spawn({
        let project = project.clone();
        async move { root_document(&path, &project).map(|r| (path, r)) }
    });
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
                    let w = if w.len() < 3 {
                        &run[run.len() - 3..]
                    } else {
                        w
                    };
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
    if let Some(i) = line
        .find("//")
        .filter(|&i| i == 0 || line[..i].ends_with(char::is_whitespace))
    {
        line = &line[..i];
    }
    // Code lines render nothing as written.
    if let Some(code) = line.strip_prefix('#') {
        let word = code
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("");
        if matches!(
            word,
            "let" | "set" | "show" | "import" | "include" | "if" | "for" | "while"
        ) {
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
        let words =
            |l: &str| -> Vec<String> { prose_runs(l).into_iter().map(|r| r.join(" ")).collect() };
        assert_eq!(
            words("As shown in @intro, $x^2 + y$ is *bold* and _soft_."),
            ["As shown in", "is bold and soft."]
        );
        assert_eq!(words("= Introduction <intro>"), ["Introduction"]);
        assert_eq!(words("- an item with `code`"), ["an item with"]);
        assert!(words("#let total = 3 + 4").is_empty());
        assert!(words("#set page(paper: \"a4\")").is_empty());
        assert_eq!(words("Plain text // a comment"), ["Plain text"]);
        assert_eq!(
            words("See https://typst.app now"),
            ["See https://typst.app now"]
        );
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
