//! The Review tab's model (collaborator mode, docs/COLLAB.md "Review"): every
//! open session's changes and notes in one place, as plain data built off the
//! UI thread. Per session, per changed file: the diff from the text before pi
//! first touched it, pi's reason, and the notes that sit in it (placed on the
//! line they're about when that line shows in the diff); plus what needs you.
//!
//! Nothing new is stored: the input is each session's change list and pins,
//! and actions go back through [`crate::Agent`] and [`crate::walk`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::changes::{FileChange, Hunk};
use crate::collab::{Pin, Stop, StopRef};

/// Lines of unchanged code shown around each change.
pub const CONTEXT: usize = 3;

/// One open session, as the Review tab reads it.
pub struct SessionInput<'a> {
    /// The session's index in [`crate::Sessions::all`] when it was read.
    pub index: usize,
    pub name: String,
    pub busy: bool,
    pub changes: &'a [FileChange],
    pub pins: &'a [Pin],
}

#[derive(Clone, Debug, Default)]
pub struct Review {
    pub sessions: Vec<SessionReview>,
    /// Open questions first, then files pi didn't explain.
    pub needs: Vec<Need>,
}

impl Review {
    pub fn is_empty(&self) -> bool {
        self.sessions
            .iter()
            .all(|s| s.files.is_empty() && s.loose.is_empty())
    }

    pub fn files(&self) -> impl Iterator<Item = (&SessionReview, &FileReview)> {
        self.sessions
            .iter()
            .flat_map(|s| s.files.iter().map(move |f| (s, f)))
    }

    /// The file shown under diff path `key`.
    pub fn file(&self, key: &str) -> Option<(&SessionReview, &FileReview)> {
        self.files().find(|(_, f)| f.key == key)
    }

    /// (seen, all) over the notes in changed files and the pins.
    pub fn seen_counts(&self) -> (usize, usize) {
        let mut seen = 0;
        let mut all = 0;
        for s in &self.sessions {
            let stops = s
                .files
                .iter()
                .flat_map(|f| f.notes.iter().map(|n| &n.stop))
                .chain(s.loose.iter());
            for st in stops {
                all += 1;
                seen += st.seen as usize;
            }
        }
        (seen, all)
    }

    /// (reviewed, all) changed files.
    pub fn reviewed_counts(&self) -> (usize, usize) {
        let files: Vec<_> = self.files().filter(|(_, f)| !f.reverted).collect();
        (
            files.iter().filter(|(_, f)| f.reviewed).count(),
            files.len(),
        )
    }
}

#[derive(Clone, Debug)]
pub struct SessionReview {
    pub index: usize,
    pub name: String,
    pub busy: bool,
    pub files: Vec<FileReview>,
    /// pi's notes on files this session didn't change (not tours).
    pub loose: Vec<Stop>,
}

#[derive(Clone, Debug)]
pub struct FileReview {
    pub path: PathBuf,
    /// Path relative to the project root.
    pub rel: String,
    /// The diff's file path: `rel`, made unique when two sessions changed the
    /// same file (the diff keys its files by path).
    pub key: String,
    /// Unified diff (with [`CONTEXT`] lines), "" when there's no difference.
    pub patch: String,
    pub before: String,
    pub now: String,
    pub hunks: Vec<Hunk>,
    pub stats: (usize, usize),
    pub summary: Option<String>,
    pub reviewed: bool,
    pub reverted: bool,
    pub created: bool,
    pub deleted: bool,
    /// The notes in this file, by line: hunk notes, the file's own stop when
    /// pi gave no hunk notes, and pins.
    pub notes: Vec<NoteAt>,
}

impl FileReview {
    pub fn unseen(&self) -> usize {
        self.notes.iter().filter(|n| !n.stop.seen).count()
    }
}

#[derive(Clone, Debug)]
pub struct NoteAt {
    pub stop: Stop,
    /// Where the note's line is now (1-based), re-found by its text.
    pub line: usize,
    /// `line` shows in the diff (inside a change or its context), so the note
    /// can sit under it; else it goes under the file's header.
    pub in_diff: bool,
    pub is_pin: bool,
    /// A pin you haven't resolved or replied to.
    pub open: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NeedKind {
    /// A pin pi left that you haven't answered or resolved.
    Question,
    /// A changed file without a reason.
    NoReason,
}

#[derive(Clone, Debug)]
pub struct Need {
    pub kind: NeedKind,
    /// Index into [`Review::sessions`].
    pub session: usize,
    pub session_name: String,
    pub path: PathBuf,
    pub rel: String,
    /// The diff key of the file it's in, when that file is shown.
    pub key: Option<String>,
    pub line: usize,
    pub text: String,
    pub stop: Option<Stop>,
}

/// Build the review from the open sessions; `read` gives a file's text now
/// (`None`: it doesn't exist). Blocking: call off the UI thread.
pub fn build(
    sessions: &[SessionInput],
    root: &Path,
    read: impl Fn(&Path) -> Option<String>,
) -> Review {
    let rel_of = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    let mut keys: HashSet<String> = HashSet::new();
    let mut out = Review::default();
    let mut questions = Vec::new();
    let mut no_reason = Vec::new();
    for (si, s) in sessions.iter().enumerate() {
        let stops = crate::walk::stops(s.changes, s.pins);
        let changed: HashSet<&Path> = s
            .changes
            .iter()
            .filter(|c| !c.reverted)
            .map(|c| c.path.as_path())
            .collect();
        let mut files = Vec::new();
        for c in s.changes {
            let rel = rel_of(&c.path);
            let mut key = rel.clone();
            let mut n = 2;
            while !keys.insert(key.clone()) {
                key = format!("{rel}~{n}");
                n += 1;
            }
            let now_opt = read(&c.path);
            let before = c.before.clone().unwrap_or_default();
            let now = now_opt.clone().unwrap_or_default();
            let patch = if c.reverted || before == now {
                String::new()
            } else {
                unified(&before, &now, &key, c.before.is_none(), now_opt.is_none())
            };
            let hunks = crate::changes::hunks(&before, &now);
            let shown: Vec<std::ops::Range<usize>> = hunks
                .iter()
                .map(|h| h.after.start.saturating_sub(CONTEXT)..h.after.end + CONTEXT)
                .collect();
            let mut notes: Vec<NoteAt> = stops
                .iter()
                .filter(|st| st.path == c.path && !is_tour_pin(s.pins, st))
                .map(|st| {
                    let line = locate(s.changes, s.pins, st, &now);
                    // A file's own stop (no hunk notes) belongs to the whole file.
                    let in_diff = !c.reverted
                        && !matches!(st.at, StopRef::File(_))
                        && shown.iter().any(|r| r.contains(&(line.saturating_sub(1))));
                    let (is_pin, open) = match st.at {
                        StopRef::Pin(i) => (true, crate::walk::pin_open(&s.pins[i])),
                        _ => (false, false),
                    };
                    NoteAt {
                        stop: st.clone(),
                        line,
                        in_diff,
                        is_pin,
                        open,
                    }
                })
                .collect();
            notes.sort_by_key(|n| n.line);
            if !c.reverted && c.summary.is_none() && c.notes.is_empty() {
                no_reason.push(Need {
                    kind: NeedKind::NoReason,
                    session: si,
                    session_name: s.name.clone(),
                    path: c.path.clone(),
                    rel: rel.clone(),
                    key: Some(key.clone()),
                    line: c.first_line.unwrap_or(1),
                    text: format!("{rel} changed with no reason given"),
                    stop: None,
                });
            }
            for n in notes.iter().filter(|n| n.open) {
                questions.push(Need {
                    kind: NeedKind::Question,
                    session: si,
                    session_name: s.name.clone(),
                    path: c.path.clone(),
                    rel: rel.clone(),
                    key: Some(key.clone()),
                    line: n.line,
                    text: n.stop.note.clone(),
                    stop: Some(n.stop.clone()),
                });
            }
            files.push(FileReview {
                path: c.path.clone(),
                rel,
                key,
                patch,
                stats: crate::changes::line_stats(&before, &now),
                before,
                now,
                hunks,
                summary: c.summary.clone(),
                reviewed: c.reviewed,
                reverted: c.reverted,
                created: c.before.is_none(),
                deleted: now_opt.is_none() && c.before.is_some(),
                notes,
            });
        }
        let loose: Vec<Stop> = stops
            .into_iter()
            .filter(|st| matches!(st.at, StopRef::Pin(_)))
            .filter(|st| !changed.contains(st.path.as_path()) && !is_tour_pin(s.pins, st))
            .filter(|st| !s.changes.iter().any(|c| c.path == st.path))
            .collect();
        for st in &loose {
            if let StopRef::Pin(i) = st.at
                && crate::walk::pin_open(&s.pins[i])
            {
                questions.push(Need {
                    kind: NeedKind::Question,
                    session: si,
                    session_name: s.name.clone(),
                    path: st.path.clone(),
                    rel: rel_of(&st.path),
                    key: None,
                    line: st.line,
                    text: st.note.clone(),
                    stop: Some(st.clone()),
                });
            }
        }
        out.sessions.push(SessionReview {
            index: s.index,
            name: s.name.clone(),
            busy: s.busy,
            files,
            loose,
        });
    }
    out.needs = questions;
    out.needs.append(&mut no_reason);
    out
}

fn is_tour_pin(pins: &[Pin], st: &Stop) -> bool {
    crate::walk::is_tour_stop(pins, st)
}

/// The stop's line in `now`: its anchor re-found by text, else the saved line.
fn locate(changes: &[FileChange], pins: &[Pin], st: &Stop, now: &str) -> usize {
    crate::walk::anchor(changes, pins, &st.at)
        .and_then(|a| a.locate(now))
        .unwrap_or(st.line)
        .max(1)
}

fn unified(before: &str, now: &str, key: &str, created: bool, deleted: bool) -> String {
    let a = if created {
        "/dev/null".to_string()
    } else {
        format!("a/{key}")
    };
    let b = if deleted {
        "/dev/null".to_string()
    } else {
        format!("b/{key}")
    };
    similar::TextDiff::from_lines(before, now)
        .unified_diff()
        .context_radius(CONTEXT)
        .header(&a, &b)
        .to_string()
}

/// What "Ask pi why" asks pi about files it changed without a reason.
pub fn ask_why_prompt(files: &[String]) -> String {
    format!(
        "Don't change any code. For each of these files you changed, call \
         ide_explain_change with a one-line summary of why, and hunk notes \
         where the reason isn't obvious: {}",
        files.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::Touch;
    use crate::collab::{Anchor, HunkNote, Reply};

    fn change(path: &str, before: &str, summary: Option<&str>) -> FileChange {
        let mut c = FileChange::new(
            PathBuf::from(path),
            Some(before.to_string()),
            Touch {
                tool_call_id: "t1".into(),
                tool: "edit".into(),
            },
        );
        c.summary = summary.map(str::to_string);
        c
    }

    fn anchor(line: usize, text: &str) -> Anchor {
        Anchor {
            line,
            text: text.into(),
            outdated: false,
            span: None,
        }
    }

    fn pin(id: &str, path: &str, line: usize, text: &str, note: &str) -> Pin {
        Pin {
            id: id.into(),
            path: PathBuf::from(path),
            anchor: anchor(line, text),
            end_line: line,
            note: note.into(),
            snippet: vec![],
            seen: false,
            dismissed: false,
            thread: vec![],
        }
    }

    const BEFORE: &str = "fn main() {\n    let a = 1;\n    run(a);\n}\n\nfn run(a: i32) {}\n";
    const NOW: &str = "fn main() {\n    let a = 1;\n    let verbose = true;\n    run(a);\n}\n\nfn run(a: i32) {}\n";

    fn read(p: &Path) -> Option<String> {
        match p.to_str()? {
            "/p/src/main.rs" => Some(NOW.to_string()),
            "/p/src/lib.rs" => Some("new\n".to_string()),
            "/p/src/other.rs" => Some("x\n".to_string()),
            _ => None,
        }
    }

    #[test]
    fn notes_sit_on_their_line_and_questions_lead() {
        let mut c = change("/p/src/main.rs", BEFORE, Some("Adds a flag."));
        c.notes.push(HunkNote {
            // pi's line number is off by one; the text finds it.
            anchor: anchor(2, "    let verbose = true;"),
            note: "Why the flag".into(),
            seen: false,
            dismissed: false,
            thread: vec![],
        });
        let lib = change("/p/src/lib.rs", "old\n", None);
        let pins = vec![
            pin(
                "p1",
                "/p/src/main.rs",
                4,
                "    run(a);",
                "Should run take a flag?",
            ),
            pin("p2", "/p/src/other.rs", 1, "x", "Look here"),
        ];
        let changes = vec![c, lib];
        let r = build(
            &[SessionInput {
                index: 0,
                name: "s".into(),
                busy: false,
                changes: &changes,
                pins: &pins,
            }],
            Path::new("/p"),
            read,
        );
        let s = &r.sessions[0];
        assert_eq!(s.files.len(), 2);
        let main = &s.files[0];
        assert_eq!(main.key, "src/main.rs");
        assert!(main.patch.contains("+    let verbose = true;"));
        assert_eq!(main.stats, (1, 0));
        let lines: Vec<(usize, bool)> = main.notes.iter().map(|n| (n.line, n.in_diff)).collect();
        assert_eq!(lines, vec![(3, true), (4, true)]);
        assert!(main.notes[1].is_pin && main.notes[1].open);
        // Loose pin on a file the session didn't change.
        assert_eq!(s.loose.len(), 1);
        // Needs you: both pins (open), then lib.rs without a reason.
        let kinds: Vec<(NeedKind, &str)> = r
            .needs
            .iter()
            .map(|n| (n.kind.clone(), n.rel.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (NeedKind::Question, "src/main.rs"),
                (NeedKind::Question, "src/other.rs"),
                (NeedKind::NoReason, "src/lib.rs"),
            ]
        );
        // main.rs's note and pin, lib.rs's own stop, the loose pin.
        assert_eq!(r.seen_counts(), (0, 4));
    }

    #[test]
    fn answered_pin_is_not_a_question_and_keys_stay_unique() {
        let mut p = pin("p1", "/p/src/main.rs", 4, "    run(a);", "Q?");
        p.thread.push(Reply {
            pi: false,
            text: "yes".into(),
        });
        let pins = vec![p];
        let a = vec![change("/p/src/main.rs", BEFORE, Some("A"))];
        let b = vec![change("/p/src/main.rs", BEFORE, Some("B"))];
        let r = build(
            &[
                SessionInput {
                    index: 0,
                    name: "one".into(),
                    busy: false,
                    changes: &a,
                    pins: &pins,
                },
                SessionInput {
                    index: 1,
                    name: "two".into(),
                    busy: true,
                    changes: &b,
                    pins: &[],
                },
            ],
            Path::new("/p"),
            read,
        );
        assert!(r.needs.is_empty());
        let keys: Vec<&str> = r.files().map(|(_, f)| f.key.as_str()).collect();
        assert_eq!(keys, vec!["src/main.rs", "src/main.rs~2"]);
    }

    #[test]
    fn reverted_file_has_no_patch_and_notes_go_under_the_header() {
        let mut c = change("/p/src/main.rs", BEFORE, Some("A"));
        c.reverted = true;
        let pins = vec![pin("p1", "/p/src/main.rs", 4, "    run(a);", "Q?")];
        let changes = vec![c];
        let r = build(
            &[SessionInput {
                index: 0,
                name: "s".into(),
                busy: false,
                changes: &changes,
                pins: &pins,
            }],
            Path::new("/p"),
            read,
        );
        let f = &r.sessions[0].files[0];
        assert!(f.patch.is_empty());
        assert!(f.notes.iter().all(|n| !n.in_diff));
        assert_eq!(r.reviewed_counts(), (0, 0));
    }
}
