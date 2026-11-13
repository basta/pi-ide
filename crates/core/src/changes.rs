//! What pi changed in a session (spec M2): per file, the content from just
//! before pi first touched it, and the tool calls that touched it.

use std::path::{Path, PathBuf};

use gpui_kit::SharedString;

use crate::collab::{CatchUp, HunkNote, Pin, ProgressStep};

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
    /// pi's one-line reason (`ide_explain_change`).
    pub summary: Option<String>,
    /// pi's notes on changed places (`ide_explain_change`).
    pub notes: Vec<HunkNote>,
    /// First changed line now (1-based), with the stats; the walkthrough's stop
    /// for a file without notes.
    pub first_line: Option<usize>,
    /// That stop was stepped onto (walkthrough).
    pub seen: bool,
}

impl FileChange {
    pub fn new(path: PathBuf, before: Option<String>, touch: Touch) -> Self {
        Self {
            path,
            before,
            touches: vec![touch],
            stats: None,
            deleted_now: false,
            reviewed: false,
            reverted: false,
            summary: None,
            notes: Vec::new(),
            first_line: None,
            seen: false,
        }
    }

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

/// Where a pi session's change list is kept, so it survives a restart.
fn store_file(session_file: &Path) -> PathBuf {
    let key = crate::buffer::hash_bytes(session_file.to_string_lossy().as_bytes());
    crate::paths::state_dir()
        .join("changes")
        .join(format!("{key:016x}.json"))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    path: PathBuf,
    before: Option<String>,
    touches: Vec<(String, String)>,
    reviewed: bool,
    reverted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    notes: Vec<HunkNote>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    seen: bool,
}

/// What's saved per pi session: its changes and what pi pointed at (collaborator mode).
#[derive(Clone, Debug, Default)]
pub struct Saved {
    pub changes: Vec<FileChange>,
    pub pins: Vec<Pin>,
    pub progress: Vec<ProgressStep>,
    pub catch_up: Option<CatchUp>,
}

impl Saved {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
            && self.pins.is_empty()
            && self.progress.is_empty()
            && self.catch_up.is_none()
    }
}

/// The file: an object since collaborator mode; a bare change list before.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum StoredFile {
    Full {
        changes: Vec<Stored>,
        #[serde(default)]
        pins: Vec<Pin>,
        #[serde(default)]
        progress: Vec<ProgressStep>,
        #[serde(default)]
        catch_up: Option<CatchUp>,
    },
    Changes(Vec<Stored>),
}

/// Write a session's saved state (blocking: call off the UI thread).
pub fn save(session_file: &Path, saved: &Saved) -> std::io::Result<()> {
    save_at(&store_file(session_file), saved)
}

fn save_at(file: &Path, saved: &Saved) -> std::io::Result<()> {
    let file = file.to_path_buf();
    let changes = &saved.changes;
    if saved.is_empty() {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let stored: Vec<Stored> = changes
        .iter()
        .map(|c| Stored {
            path: c.path.clone(),
            before: c.before.clone(),
            touches: c
                .touches
                .iter()
                .map(|t| (t.tool_call_id.clone(), t.tool.clone()))
                .collect(),
            reviewed: c.reviewed,
            reverted: c.reverted,
            summary: c.summary.clone(),
            notes: c.notes.clone(),
            seen: c.seen,
        })
        .collect();
    let doc = serde_json::json!({
        "changes": stored,
        "pins": saved.pins,
        "progress": saved.progress,
        "catch_up": saved.catch_up,
    });
    std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")))?;
    let tmp = file.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec(&doc).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(tmp, file)
}

/// A saved session's state (empty if none); stats are left to compute.
/// Pins and hunk notes are re-anchored against the files on disk (blocking:
/// call off the UI thread).
pub fn load(session_file: &Path) -> Saved {
    let mut saved = load_at(&store_file(session_file));
    reanchor_from_disk(&mut saved);
    saved
}

/// Find every pin and hunk note again in its file as it is now; mark the ones
/// whose text is gone outdated.
pub fn reanchor_from_disk(saved: &mut Saved) {
    let mut cache: std::collections::HashMap<PathBuf, Option<String>> = Default::default();
    let mut text = |p: &Path| {
        cache
            .entry(p.to_path_buf())
            .or_insert_with(|| snapshot(p))
            .clone()
    };
    for c in &mut saved.changes {
        let now = text(&c.path);
        for n in &mut c.notes {
            match &now {
                Some(t) => {
                    n.anchor.reanchor(t);
                }
                None => n.anchor.outdated = true,
            }
        }
    }
    for p in &mut saved.pins {
        match text(&p.path) {
            Some(t) => {
                p.anchor.reanchor(&t);
            }
            None => p.anchor.outdated = true,
        }
    }
}

fn load_at(file: &Path) -> Saved {
    let Ok(bytes) = std::fs::read(file) else {
        return Saved::default();
    };
    let (stored, pins, progress, catch_up) = match serde_json::from_slice::<StoredFile>(&bytes) {
        Ok(StoredFile::Full {
            changes,
            pins,
            progress,
            catch_up,
        }) => (changes, pins, progress, catch_up),
        Ok(StoredFile::Changes(c)) => (c, Vec::new(), Vec::new(), None),
        Err(e) => {
            log::warn!("{}: {e}", file.display());
            Default::default()
        }
    };
    let changes = stored
        .into_iter()
        .map(|s| FileChange {
            path: s.path,
            before: s.before,
            touches: s
                .touches
                .into_iter()
                .map(|(tool_call_id, tool)| Touch { tool_call_id, tool })
                .collect(),
            stats: None,
            deleted_now: false,
            reviewed: s.reviewed,
            reverted: s.reverted,
            summary: s.summary,
            notes: s.notes,
            first_line: None,
            seen: s.seen,
        })
        .collect();
    Saved {
        changes,
        pins,
        progress,
        catch_up,
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
    fn change_list_round_trips() {
        let file = std::env::temp_dir().join(format!("pide-changes-{}/x.json", std::process::id()));
        let mut c = FileChange::new(
            "/p/a.rs".into(),
            Some("old\n".into()),
            Touch {
                tool_call_id: "t1".into(),
                tool: "edit".into(),
            },
        );
        c.stats = Some((1, 1));
        c.reviewed = true;
        c.summary = Some("why".into());
        c.notes = vec![HunkNote {
            anchor: crate::collab::Anchor::new(3, "x".into()),
            note: "n".into(),
            seen: true,
            dismissed: false,
        }];
        let pin = Pin {
            id: "t2".into(),
            path: "/p/a.rs".into(),
            anchor: crate::collab::Anchor::new(1, "fn a".into()),
            end_line: 2,
            note: "look".into(),
            seen: false,
            dismissed: false,
        };
        let progress = vec![ProgressStep {
            text: "one".into(),
            done: true,
        }];
        save_at(
            &file,
            &Saved {
                changes: vec![c.clone()],
                pins: vec![pin.clone()],
                progress: progress.clone(),
                catch_up: Some(CatchUp {
                    text: "brief".into(),
                    at: 7,
                }),
            },
        )
        .unwrap();
        let saved = load_at(&file);
        assert_eq!(saved.pins, vec![pin]);
        assert_eq!(saved.progress, progress);
        assert_eq!(saved.catch_up.as_ref().map(|c| c.at), Some(7));
        assert_eq!(saved.changes[0].summary.as_deref(), Some("why"));
        assert_eq!(saved.changes[0].notes, c.notes);
        let back = saved.changes;
        assert_eq!(back.len(), 1);
        assert_eq!(
            (
                back[0].path.clone(),
                back[0].before.clone(),
                back[0].touches.clone(),
                back[0].reviewed
            ),
            (c.path, c.before, c.touches, true)
        );
        assert_eq!(back[0].stats, None);
        // The format before collaborator mode (a bare list) still loads.
        std::fs::write(
            &file,
            r#"[{"path":"/p/b.rs","before":null,"touches":[["t","write"]],"reviewed":false,"reverted":false}]"#,
        )
        .unwrap();
        assert_eq!(load_at(&file).changes[0].path, PathBuf::from("/p/b.rs"));
        save_at(&file, &Saved::default()).unwrap();
        assert!(load_at(&file).is_empty());
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

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
