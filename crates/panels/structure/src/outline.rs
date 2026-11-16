//! The Structure panel's model: a file's outline as a flat list in document
//! order, each entry with its depth. From the language server's document
//! symbols, or, for prose without one (Markdown, or before the server is up),
//! from the headings found by scanning the text. Plain functions, unit-tested.

use std::collections::HashSet;
use std::path::Path;

use lsp_types::{DocumentSymbolResponse, Position, SymbolKind};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    /// "fn", "struct", …; empty for headings.
    pub kind: &'static str,
    pub heading: bool,
    pub depth: usize,
    /// Where to jump: 0-based line and character column.
    pub line: u32,
    pub character: u32,
    /// The lines it spans (inclusive), for "where is the cursor".
    pub start_line: u32,
    pub end_line: u32,
}

/// Files whose outline is their headings.
pub fn is_prose(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("typ" | "tex" | "md" | "markdown")
    )
}

fn kind_label(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::FUNCTION => "fn",
        SymbolKind::METHOD => "method",
        SymbolKind::CONSTRUCTOR => "new",
        SymbolKind::STRUCT => "struct",
        SymbolKind::CLASS => "class",
        SymbolKind::ENUM => "enum",
        SymbolKind::INTERFACE => "trait",
        SymbolKind::CONSTANT => "const",
        SymbolKind::FIELD | SymbolKind::PROPERTY => "field",
        SymbolKind::MODULE | SymbolKind::NAMESPACE | SymbolKind::PACKAGE => "mod",
        SymbolKind::VARIABLE => "var",
        SymbolKind::TYPE_PARAMETER => "type",
        SymbolKind::ENUM_MEMBER => "variant",
        SymbolKind::OBJECT => "object",
        SymbolKind::KEY => "key",
        _ => "",
    }
}

/// In prose the code names mislead (texlab reports figures as methods):
/// only the kinds that read right there.
fn prose_kind(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::FUNCTION => "fn",
        SymbolKind::VARIABLE => "var",
        SymbolKind::CONSTANT => "label",
        _ => "",
    }
}

/// tinymist reports Typst headings as namespaces, texlab LaTeX sections as modules.
fn is_heading_kind(k: SymbolKind) -> bool {
    k == SymbolKind::NAMESPACE || k == SymbolKind::MODULE
}

/// A heading's text without inline markup (`Deep *bold* part`).
fn plain(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '*' | '_' | '`'))
        .collect::<String>()
        .trim()
        .to_string()
}

/// The server's symbols. `to_chars` turns a server position into a character
/// column (the server may count UTF-16 units).
pub fn from_lsp(
    resp: DocumentSymbolResponse,
    prose: bool,
    to_chars: &dyn Fn(Position) -> Position,
) -> Vec<Entry> {
    let mut out = Vec::new();
    let entry = |name: &str, kind: SymbolKind, at: Position, start: u32, end: u32, depth| {
        let heading = prose && is_heading_kind(kind);
        let at = to_chars(at);
        Entry {
            name: if heading { plain(name) } else { name.trim().to_string() },
            kind: if heading {
                ""
            } else if prose {
                prose_kind(kind)
            } else {
                kind_label(kind)
            },
            heading,
            depth,
            line: at.line,
            character: at.character,
            start_line: start,
            end_line: end.max(start),
        }
    };
    match resp {
        DocumentSymbolResponse::Nested(list) => {
            fn walk(
                list: Vec<lsp_types::DocumentSymbol>,
                depth: usize,
                out: &mut Vec<Entry>,
                entry: &dyn Fn(&str, SymbolKind, Position, u32, u32, usize) -> Entry,
            ) {
                let mut list = list;
                list.sort_by_key(|s| (s.range.start.line, s.range.start.character));
                for s in list {
                    out.push(entry(
                        &s.name,
                        s.kind,
                        s.selection_range.start,
                        s.range.start.line,
                        s.range.end.line,
                        depth,
                    ));
                    if let Some(children) = s.children {
                        walk(children, depth + 1, out, entry);
                    }
                }
            }
            walk(list, 0, &mut out, &entry);
        }
        DocumentSymbolResponse::Flat(mut list) => {
            // No nesting given: a symbol is inside the ones whose range holds it.
            list.sort_by_key(|s| (s.location.range.start.line, s.location.range.start.character));
            let mut open: Vec<lsp_types::Range> = Vec::new();
            for s in list {
                let r = s.location.range;
                open.retain(|o| o.start <= r.start && o.end >= r.end);
                out.push(entry(&s.name, s.kind, r.start, r.start.line, r.end.line, open.len()));
                open.push(r);
            }
        }
    }
    out
}

/// Only the headings (prose files: drop labels, figures, `#let`s).
pub fn headings_only(entries: &[Entry]) -> Vec<Entry> {
    entries.iter().filter(|e| e.heading).cloned().collect()
}

/// Headings found by reading the text: Typst `= …`, LaTeX `\section{…}` and
/// friends, Markdown `# …`. For prose files without a language server.
pub fn scan_headings(text: &str, path: &Path) -> Vec<Entry> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut found: Vec<(usize, u32, u32, String)> = Vec::new(); // (level, line, col, name)
    let mut in_block = false;
    for (i, raw) in text.lines().enumerate() {
        let line = i as u32;
        match ext {
            "typ" => {
                let t = raw.trim_start();
                if t.starts_with("```") {
                    in_block = !in_block;
                    continue;
                }
                if in_block {
                    continue;
                }
                let level = t.chars().take_while(|c| *c == '=').count();
                let rest = &t[level..];
                if level > 0 && rest.starts_with(' ') {
                    // `= Title <label>`: the label isn't part of the name.
                    let name = rest.split(" <").next().unwrap_or(rest);
                    let col = (raw.len() - t.len() + level + 1) as u32;
                    found.push((level, line, col, plain(name)));
                }
            }
            "md" | "markdown" => {
                let t = raw.trim_start();
                if t.starts_with("```") || t.starts_with("~~~") {
                    in_block = !in_block;
                    continue;
                }
                if in_block {
                    continue;
                }
                let level = t.chars().take_while(|c| *c == '#').count();
                let rest = &t[level..];
                if (1..=6).contains(&level) && (rest.starts_with(' ') || rest.is_empty()) {
                    let name = rest.trim().trim_end_matches('#').trim();
                    if !name.is_empty() {
                        let col = (raw.len() - t.len() + level + 1) as u32;
                        found.push((level, line, col, plain(name)));
                    }
                }
            }
            "tex" => {
                let l = uncommented_tex(raw);
                const CMDS: [(&str, usize); 7] = [
                    ("\\part", 0),
                    ("\\chapter", 1),
                    ("\\section", 2),
                    ("\\subsection", 3),
                    ("\\subsubsection", 4),
                    ("\\paragraph", 5),
                    ("\\subparagraph", 6),
                ];
                for (at, _) in l.match_indices('\\') {
                    let Some((cmd, level)) = CMDS
                        .iter()
                        .filter(|(c, _)| l[at..].starts_with(c))
                        .max_by_key(|(c, _)| c.len())
                    else {
                        continue;
                    };
                    let mut rest = &l[at + cmd.len()..];
                    // `\sectionmark` and the like aren't headings.
                    if rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
                        continue;
                    }
                    rest = rest.strip_prefix('*').unwrap_or(rest).trim_start();
                    if rest.starts_with('[') {
                        rest = rest.split_once(']').map(|(_, r)| r).unwrap_or("").trim_start();
                    }
                    let Some(arg) = rest.strip_prefix('{').and_then(braced) else {
                        continue;
                    };
                    let col = l[..at].chars().count() as u32;
                    found.push((*level, line, col, arg.trim().to_string()));
                }
            }
            _ => return vec![],
        }
    }
    let last = text.lines().count().saturating_sub(1) as u32;
    let mut out = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for (i, (level, line, col, name)) in found.iter().enumerate() {
        while stack.last().is_some_and(|l| l >= level) {
            stack.pop();
        }
        let end = found[i + 1..]
            .iter()
            .find(|(l, ..)| l <= level)
            .map(|(_, ln, ..)| ln.saturating_sub(1))
            .unwrap_or(last);
        out.push(Entry {
            name: name.clone(),
            kind: "",
            heading: true,
            depth: stack.len(),
            line: *line,
            character: *col,
            start_line: *line,
            end_line: end.max(*line),
        });
        stack.push(*level);
    }
    out
}

/// The text of a `{…}` argument whose `{` was just consumed (nested braces kept).
fn braced(s: &str) -> Option<&str> {
    let mut depth = 1;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn uncommented_tex(line: &str) -> &str {
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        if c == '%' && prev != '\\' {
            return &line[..i];
        }
        prev = c;
    }
    line
}

/// A stable id per entry for remembering what's collapsed: its name and its
/// ancestors' names.
pub fn ids(entries: &[Entry]) -> Vec<String> {
    let mut path: Vec<&str> = Vec::new();
    entries
        .iter()
        .map(|e| {
            path.truncate(e.depth);
            path.push(&e.name);
            path.join("\u{1f}")
        })
        .collect()
}

pub fn has_children(entries: &[Entry], i: usize) -> bool {
    entries.get(i + 1).is_some_and(|n| n.depth > entries[i].depth)
}

/// The entries shown: everything but what's inside a collapsed entry.
pub fn visible(entries: &[Entry], ids: &[String], collapsed: &HashSet<String>) -> Vec<usize> {
    let mut out = Vec::new();
    let mut hide_below: Option<usize> = None;
    for (i, e) in entries.iter().enumerate() {
        if let Some(d) = hide_below {
            if e.depth > d {
                continue;
            }
            hide_below = None;
        }
        out.push(i);
        if collapsed.contains(&ids[i]) && has_children(entries, i) {
            hide_below = Some(e.depth);
        }
    }
    out
}

/// The entry the cursor is in: the deepest one spanning its line, else the
/// last one starting at or above it.
pub fn current(entries: &[Entry], line: u32) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, e) in entries.iter().enumerate() {
        if e.start_line <= line && line <= e.end_line && best.is_none_or(|b| e.depth > entries[b].depth) {
            best = Some(i);
        }
    }
    best.or_else(|| entries.iter().rposition(|e| e.start_line <= line))
}

/// `i`, or its nearest ancestor that's shown (it's inside a collapsed one).
pub fn shown_ancestor(entries: &[Entry], rows: &[usize], mut i: usize) -> Option<usize> {
    loop {
        if rows.contains(&i) {
            return Some(i);
        }
        let d = entries[i].depth;
        i = (0..i).rev().find(|&j| entries[j].depth < d)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(e: &[Entry]) -> Vec<(usize, &str)> {
        e.iter().map(|e| (e.depth, e.name.as_str())).collect()
    }

    #[test]
    fn typst_headings() {
        let text = "#set page(height: 10cm)\n= Introduction\nText.\n== Background <bg>\n```\n= not a heading\n```\n=== Deep *bold* part\n= Method\n==Not one\n";
        let e = scan_headings(text, Path::new("a.typ"));
        assert_eq!(
            names(&e),
            [(0, "Introduction"), (1, "Background"), (2, "Deep bold part"), (0, "Method")]
        );
        assert_eq!((e[0].line, e[0].character), (1, 2));
        // A section runs until the next one at its level or above.
        assert_eq!((e[0].start_line, e[0].end_line), (1, 7));
        assert_eq!(e[3].end_line, 9);
    }

    #[test]
    fn latex_headings() {
        let text = "\\documentclass{article}\n\\begin{document}\n\\section{Intro to $\\mathbf{x}$}\\label{s}\n% \\section{Old}\n\\subsection*{Background}\n\\sectionmark{no}\n\\subsubsection[short]{Deep}\n\\section{Method}\n";
        let e = scan_headings(text, Path::new("a.tex"));
        assert_eq!(
            names(&e),
            [(0, "Intro to $\\mathbf{x}$"), (1, "Background"), (2, "Deep"), (0, "Method")]
        );
    }

    #[test]
    fn markdown_headings() {
        let text = "# Title\n\n## Part ##\n```\n# code\n```\n#nope\n### Sub\n";
        let e = scan_headings(text, Path::new("README.md"));
        assert_eq!(names(&e), [(0, "Title"), (1, "Part"), (2, "Sub")]);
        assert!(scan_headings(text, Path::new("a.rs")).is_empty());
    }

    #[test]
    fn lsp_nested_and_headings_only() {
        // tinymist's answer for a small document (recorded shape).
        let v = serde_json::json!([
            {"name": "Introduction", "kind": 3, "range": {"start": {"line": 1, "character": 0}, "end": {"line": 6, "character": 0}},
             "selectionRange": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 14}},
             "children": [
                {"name": "helper", "kind": 12, "range": {"start": {"line": 3, "character": 5}, "end": {"line": 3, "character": 11}},
                 "selectionRange": {"start": {"line": 3, "character": 5}, "end": {"line": 3, "character": 11}}},
                {"name": "Deep *bold* part", "kind": 3, "range": {"start": {"line": 4, "character": 0}, "end": {"line": 6, "character": 0}},
                 "selectionRange": {"start": {"line": 4, "character": 4}, "end": {"line": 4, "character": 20}}}
             ]}
        ]);
        let resp: DocumentSymbolResponse = serde_json::from_value(v).unwrap();
        let e = from_lsp(resp, true, &|p| p);
        assert_eq!(names(&e), [(0, "Introduction"), (1, "helper"), (1, "Deep bold part")]);
        assert_eq!(e[1].kind, "fn");
        assert_eq!(e[2].kind, "");
        assert!(e[0].heading && !e[1].heading);
        assert_eq!(names(&headings_only(&e)), [(0, "Introduction"), (1, "Deep bold part")]);
        // In code, a namespace is a module, not a heading.
        let resp: DocumentSymbolResponse =
            serde_json::from_value(serde_json::json!([{"name": "m", "kind": 3,
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 0}},
                "selectionRange": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 5}}}]))
            .unwrap();
        let e = from_lsp(resp, false, &|p| p);
        assert_eq!((e[0].heading, e[0].kind), (false, "mod"));
    }

    #[test]
    fn collapse_and_cursor() {
        let text = "= A\n== A1\ntext\n== A2\n= B\n";
        let e = scan_headings(text, Path::new("a.typ"));
        let ids = ids(&e);
        let mut collapsed = HashSet::new();
        assert_eq!(visible(&e, &ids, &collapsed), [0, 1, 2, 3]);
        assert_eq!(current(&e, 2), Some(1));
        assert_eq!(current(&e, 4), Some(3));
        collapsed.insert(ids[0].clone());
        let rows = visible(&e, &ids, &collapsed);
        assert_eq!(rows, [0, 3]);
        // The cursor in A1, which is hidden: A shows as current.
        assert_eq!(shown_ancestor(&e, &rows, 1), Some(0));
    }
}
