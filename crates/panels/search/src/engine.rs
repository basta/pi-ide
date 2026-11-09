//! The search itself: plain functions over files and strings, run on a
//! background thread. No GPUI here, so it's easy to test.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};

/// Files larger than this are skipped (minified bundles, data dumps).
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Stop collecting after this many matches.
pub const MAX_MATCHES: usize = 10_000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub text: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl Query {
    pub fn compile(&self) -> Result<Regex, String> {
        let mut pattern = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        if self.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            .multi_line(true)
            .build()
            .map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    /// 0-based line.
    pub line: u32,
    /// 0-based character column of the match start.
    pub character: u32,
    /// The whole line (trimmed of the line break).
    pub text: String,
    /// Byte range of the match inside `text`.
    pub range: std::ops::Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileMatches {
    pub path: PathBuf,
    pub matches: Vec<Match>,
}

#[derive(Debug, Default)]
pub struct Results {
    pub files: Vec<FileMatches>,
    pub total: usize,
    pub truncated: bool,
    pub files_searched: usize,
}

/// Matches in one text, line by line (a match never spans lines).
pub fn find_in_text(re: &Regex, text: &str, limit: usize) -> Vec<Match> {
    let mut out = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        for m in re.find_iter(line) {
            if m.start() == m.end() {
                continue; // empty matches (e.g. `^`) aren't useful results
            }
            out.push(Match {
                line: i as u32,
                character: line[..m.start()].chars().count() as u32,
                text: line.to_string(),
                range: m.start()..m.end(),
            });
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}

/// Search the project. `open` holds the current text of open buffers (their
/// unsaved edits win over the disk).
pub fn search(root: &Path, query: &Query, open: &HashMap<PathBuf, String>) -> Result<Results, String> {
    let re = query.compile()?;
    let mut results = Results::default();
    let walker = ignore::WalkBuilder::new(root).hidden(false).filter_entry(|e| e.file_name() != ".git").build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path().to_path_buf();
        let text = match open.get(&path) {
            Some(t) => t.clone(),
            None => {
                if entry.metadata().map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&path) else { continue };
                if bytes.iter().take(8000).any(|&b| b == 0) {
                    continue; // binary
                }
                let Ok(t) = String::from_utf8(bytes) else { continue };
                t
            }
        };
        results.files_searched += 1;
        let matches = find_in_text(&re, &text, MAX_MATCHES - results.total);
        if !matches.is_empty() {
            results.total += matches.len();
            results.files.push(FileMatches { path, matches });
        }
        if results.total >= MAX_MATCHES {
            results.truncated = true;
            break;
        }
    }
    results.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

/// Replace every match in `text`. `$1`-style groups work in regex mode.
pub fn replace_all(query: &Query, text: &str, replacement: &str) -> Result<(String, usize), String> {
    let re = query.compile()?;
    let n = re.find_iter(text).filter(|m| m.start() != m.end()).count();
    let out = if query.regex {
        re.replace_all(text, replacement).into_owned()
    } else {
        re.replace_all(text, regex::NoExpand(replacement)).into_owned()
    };
    Ok((out, n))
}

/// Replace in a file on disk (not open in the editor). Keeps the file's bytes
/// otherwise as they were. Returns how many were replaced.
pub fn replace_in_file(path: &Path, query: &Query, replacement: &str) -> Result<usize, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (new, n) = replace_all(query, &text, replacement)?;
    if n > 0 {
        std::fs::write(path, new).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> Query {
        Query { text: text.into(), ..Default::default() }
    }

    #[test]
    fn plain_text_is_literal_and_case_insensitive_by_default() {
        let re = q("a.b").compile().unwrap();
        let m = find_in_text(&re, "xa.b\naxb\nA.B", 100);
        assert_eq!(m.iter().map(|m| m.line).collect::<Vec<_>>(), [0, 2]);
        assert_eq!((m[0].character, m[0].range.clone()), (1, 1..4));
    }

    #[test]
    fn options_regex_case_whole_word() {
        let mut query = Query { text: r"fo+".into(), regex: true, ..Default::default() };
        assert_eq!(find_in_text(&query.compile().unwrap(), "foo FOOO f", 100).len(), 2);
        query.case_sensitive = true;
        assert_eq!(find_in_text(&query.compile().unwrap(), "foo FOOO f", 100).len(), 1);
        let ww = Query { text: "add".into(), whole_word: true, ..Default::default() };
        assert_eq!(find_in_text(&ww.compile().unwrap(), "add adder add_x (add)", 100).len(), 2);
        assert!(Query { text: "(".into(), regex: true, ..Default::default() }.compile().is_err());
    }

    #[test]
    fn columns_count_characters_and_crlf_is_trimmed() {
        let re = q("x").compile().unwrap();
        let m = find_in_text(&re, "čšx\r\nx", 100);
        assert_eq!((m[0].character, m[0].text.as_str()), (2, "čšx"));
        assert_eq!(m[1].line, 1);
    }

    #[test]
    fn replace_literal_and_regex_groups() {
        assert_eq!(replace_all(&q("$x"), "a $x b $x", "y").unwrap(), ("a y b y".into(), 2));
        let re = Query { text: r"(\w+)@(\w+)".into(), regex: true, case_sensitive: true, ..Default::default() };
        assert_eq!(replace_all(&re, "me@host", "$2/$1").unwrap().0, "host/me");
        // Literal mode doesn't expand `$1`.
        assert_eq!(replace_all(&q("a"), "a", "$1").unwrap().0, "$1");
    }

    #[test]
    fn search_walks_respects_gitignore_and_prefers_open_buffers() {
        let dir = std::env::temp_dir().join(format!("pide-search-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target\n").unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn needle() {}\n").unwrap();
        std::fs::write(dir.join("src/b.rs"), "nothing\n").unwrap();
        std::fs::write(dir.join("target/x.rs"), "needle\n").unwrap();
        std::fs::write(dir.join("bin.dat"), b"needle\0\0").unwrap();
        let mut open = HashMap::new();
        open.insert(dir.join("src/b.rs"), "unsaved needle here".to_string());
        let r = search(&dir, &q("needle"), &open).unwrap();
        let files: Vec<String> = r.files.iter().map(|f| f.path.strip_prefix(&dir).unwrap().display().to_string()).collect();
        assert_eq!(files, ["src/a.rs", "src/b.rs"]);
        assert_eq!(r.total, 2);
        assert_eq!(replace_in_file(&dir.join("src/a.rs"), &q("needle"), "pin").unwrap(), 1);
        assert_eq!(std::fs::read_to_string(dir.join("src/a.rs")).unwrap(), "fn pin() {}\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
