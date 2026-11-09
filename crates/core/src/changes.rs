//! What pi changed in a session (spec M2): per file, the content from just
//! before pi first touched it, and the tool calls that touched it.

use std::path::{Path, PathBuf};

use gpui_kit::SharedString;

#[derive(Clone, Debug, PartialEq)]
pub struct Touch {
    pub tool_call_id: String,
    pub tool: String,
}

#[derive(Clone, Debug)]
pub struct FileChange {
    /// Absolute path.
    pub path: PathBuf,
    /// Content before pi first touched the file in this session; `None` = it didn't exist.
    pub before: Option<String>,
    pub touches: Vec<Touch>,
    /// (+lines, −lines) of before → now; `None` until computed.
    pub stats: Option<(usize, usize)>,
    /// The file is gone now (pi or someone deleted it).
    pub deleted_now: bool,
    pub reviewed: bool,
    pub reverted: bool,
}

impl FileChange {
    pub fn badge(&self) -> &'static str {
        if self.deleted_now {
            "D"
        } else if self.before.is_none() {
            "A"
        } else {
            "M"
        }
    }

    pub fn tools_label(&self) -> SharedString {
        let mut tools: Vec<&str> = Vec::new();
        for t in &self.touches {
            if !tools.contains(&t.tool.as_str()) {
                tools.push(&t.tool);
            }
        }
        format!("{} ×{}", tools.join(", "), self.touches.len()).into()
    }
}

/// Lines added/removed between two texts.
pub fn line_stats(before: &str, after: &str) -> (usize, usize) {
    let diff = similar::TextDiff::from_lines(before, after);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

/// Read a file for a "before" snapshot: `None` if it doesn't exist; lossy for non-UTF-8.
pub fn snapshot(path: &Path) -> Option<String> {
    match std::fs::read(path) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(_) => None,
    }
}

/// Undo pi's changes to `path`: write `before` back, or delete a file pi created.
/// Blocking: call off the UI thread.
pub fn revert(path: &Path, before: Option<&str>) -> std::io::Result<()> {
    match before {
        Some(text) => std::fs::write(path, text),
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        },
    }
}

/// One changed region: line ranges (0-based, end exclusive) in before and after.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub before: std::ops::Range<usize>,
    pub after: std::ops::Range<usize>,
}

/// The changed regions between two texts, line by line (no context).
pub fn hunks(before: &str, after: &str) -> Vec<Hunk> {
    let diff = similar::TextDiff::from_lines(before, after);
    diff.grouped_ops(0)
        .into_iter()
        .filter_map(|group| {
            let first = group.first()?;
            let last = group.last()?;
            Some(Hunk {
                before: first.old_range().start..last.old_range().end,
                after: first.new_range().start..last.new_range().end,
            })
        })
        .collect()
}

/// `after` with one hunk put back the way it was in `before`.
pub fn revert_hunk(before: &str, after: &str, hunk: &Hunk) -> String {
    let old: Vec<&str> = before.split_inclusive('\n').collect();
    let new: Vec<&str> = after.split_inclusive('\n').collect();
    let mut out = String::with_capacity(after.len());
    for l in &new[..hunk.after.start.min(new.len())] {
        out.push_str(l);
    }
    for l in &old[hunk.before.start.min(old.len())..hunk.before.end.min(old.len())] {
        out.push_str(l);
    }
    for l in &new[hunk.after.end.min(new.len())..] {
        out.push_str(l);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn hunks_revert_one_at_a_time() {
        let before = "a\nb\nc\nd\ne\nf\ng\n";
        let after = "a\nB\nc\nd\ne\nf\ng\nh\n";
        let hs = hunks(before, after);
        assert_eq!(
            hs,
            vec![
                Hunk {
                    before: 1..2,
                    after: 1..2
                },
                Hunk {
                    before: 7..7,
                    after: 7..8
                }
            ]
        );
        assert_eq!(
            revert_hunk(before, after, &hs[0]),
            "a\nb\nc\nd\ne\nf\ng\nh\n"
        );
        assert_eq!(revert_hunk(before, after, &hs[1]), "a\nB\nc\nd\ne\nf\ng\n");
        // A deletion comes back.
        let hs = hunks("x\ny\nz\n", "x\nz\n");
        assert_eq!(revert_hunk("x\ny\nz\n", "x\nz\n", &hs[0]), "x\ny\nz\n");
    }

    #[::core::prelude::v1::test]
    fn stats_count_lines() {
        assert_eq!(line_stats("a\nb\nc\n", "a\nB\nc\nd\n"), (2, 1));
        assert_eq!(line_stats("", "x\n"), (1, 0));
    }

    #[::core::prelude::v1::test]
    fn revert_restores_or_deletes() {
        let dir = std::env::temp_dir().join(format!("pide-revert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "after").unwrap();
        revert(&f, Some("before")).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "before");
        revert(&f, None).unwrap();
        assert!(!f.exists());
        revert(&f, None).unwrap(); // already gone is fine
        let _ = std::fs::remove_dir_all(dir);
    }
}
