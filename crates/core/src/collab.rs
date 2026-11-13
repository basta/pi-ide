//! Collaborator mode (docs/COLLAB.md): what pi points you at in the code.
//! Pins (`ide_pin`), per-file reasons and hunk notes (`ide_explain_change`, kept
//! on [`crate::changes::FileChange`]), a checklist (`ide_progress`), and the
//! walkthrough that steps through all of it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A place in a file: 1-based line plus that line's text when it was made, so it
/// can be found again after the file changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub line: usize,
    pub text: String,
    /// The text wasn't found any more; `line` is where it used to be.
    #[serde(default)]
    pub outdated: bool,
}

impl Anchor {
    pub fn new(line: usize, text: String) -> Self {
        Self {
            line: line.max(1),
            text,
            outdated: false,
        }
    }

    /// Where the anchor is in `content` now (1-based), without changing it.
    /// The recorded line if its text still matches, else the nearest line with the
    /// same text; `None` if the text is gone. An empty or blank anchor text always
    /// keeps its line (too common to search for).
    pub fn locate(&self, content: &str) -> Option<usize> {
        let lines: Vec<&str> = content.split('\n').collect();
        let want = self.text.trim_end();
        let at = |n: usize| lines.get(n.wrapping_sub(1)).map(|l| l.trim_end());
        if want.trim().is_empty() || at(self.line) == Some(want) {
            return Some(self.line.min(lines.len().max(1)));
        }
        let mut best: Option<usize> = None;
        for (i, l) in lines.iter().enumerate() {
            if l.trim_end() == want {
                let n = i + 1;
                if best.is_none_or(|b| n.abs_diff(self.line) < b.abs_diff(self.line)) {
                    best = Some(n);
                }
            }
        }
        best
    }

    /// Move to where the text is now, or mark outdated. Returns whether anything changed.
    pub fn reanchor(&mut self, content: &str) -> bool {
        let before = (self.line, self.outdated);
        match self.locate(content) {
            Some(n) => {
                self.line = n;
                self.outdated = false;
            }
            None => self.outdated = true,
        }
        before != (self.line, self.outdated)
    }
}

/// `ide_pin`: pi points you at code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    /// The tool call id (unique per session).
    pub id: String,
    /// Absolute path.
    pub path: PathBuf,
    pub anchor: Anchor,
    /// Last line of the range (1-based), as given.
    pub end_line: usize,
    pub note: String,
    /// You stepped onto it (walkthrough) or opened it.
    #[serde(default)]
    pub seen: bool,
    /// You dismissed it: no mark or bar any more.
    #[serde(default)]
    pub dismissed: bool,
}

/// One of `ide_explain_change`'s hunk notes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HunkNote {
    pub anchor: Anchor,
    pub note: String,
    #[serde(default)]
    pub seen: bool,
    #[serde(default)]
    pub dismissed: bool,
}

/// One `ide_progress` step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressStep {
    pub text: String,
    pub done: bool,
}

/// What a walkthrough stop refers to inside an [`crate::Agent`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopRef {
    /// `agent.pins[i]`.
    Pin(usize),
    /// `agent.changes[file].notes[note]`.
    Hunk { file: usize, note: usize },
    /// A changed file without hunk notes: one stop at its first change.
    File(usize),
}

/// One place the walkthrough stops at.
#[derive(Clone, Debug, PartialEq)]
pub struct Stop {
    pub at: StopRef,
    pub path: PathBuf,
    /// 1-based.
    pub line: usize,
    pub note: String,
    /// The file's one-line reason, if pi gave one.
    pub summary: Option<String>,
    pub outdated: bool,
    pub seen: bool,
}

impl Stop {
    /// Text for the note bar and the chat: "<note>" or the file's summary.
    pub fn label(&self) -> String {
        if self.note.is_empty() {
            self.summary
                .clone()
                .unwrap_or_else(|| "pi changed this file.".to_string())
        } else {
            self.note.clone()
        }
    }

    /// "path:line" relative to `root`, for a reply's location prefix.
    pub fn location(&self, root: &std::path::Path) -> String {
        let rel = self.path.strip_prefix(root).unwrap_or(&self.path);
        format!("{}:{}", rel.display(), self.line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn anchors_follow_their_text() {
        let a = Anchor::new(2, "fn b() {".into());
        assert_eq!(a.locate("fn a() {}\nfn b() {\n"), Some(2));
        // Two lines inserted above: found two lines lower.
        assert_eq!(a.locate("// x\n// y\nfn a() {}\nfn b() {\n"), Some(4));
        // Nearest match wins.
        assert_eq!(a.locate("fn b() {\nx\ny\nz\nfn b() {\n"), Some(1));
        // Gone: outdated, line kept.
        let mut a2 = a.clone();
        assert!(a2.reanchor("fn c() {}\n"));
        assert!(a2.outdated);
        assert_eq!(a2.line, 2);
        // Back again: not outdated.
        assert!(a2.reanchor("\n\n\nfn b() {\n"));
        assert_eq!((a2.line, a2.outdated), (4, false));
        // Trailing whitespace doesn't matter; blank anchors keep their line.
        assert_eq!(Anchor::new(1, "x  ".into()).locate("y\nx\n"), Some(2));
        assert_eq!(Anchor::new(3, "".into()).locate("a\nb\nc\n"), Some(3));
    }
}
