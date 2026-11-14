//! pi's reasons kept with a commit (collaborator mode): when you commit files pi
//! explained, the explanations are saved by commit hash, so Git Log can show
//! why after the session is long closed. `state/commit-notes/<root>/<hash>.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Why one committed file changed, per pi.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileReason {
    /// Relative to the project root.
    pub path: String,
    /// The session's name, and its pi session file (to open it again).
    pub session: String,
    pub session_file: Option<PathBuf>,
    pub summary: Option<String>,
    /// (1-based line at commit time, note).
    pub notes: Vec<(usize, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CommitNotes {
    pub files: Vec<FileReason>,
}

impl CommitNotes {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

fn file(root: &Path, hash: &str) -> PathBuf {
    let key = crate::buffer::hash_bytes(root.to_string_lossy().as_bytes());
    crate::paths::state_dir()
        .join("commit-notes")
        .join(format!("{key:016x}"))
        .join(format!("{hash}.json"))
}

/// Save a commit's notes (blocking: off the UI thread). `hash` is the full hash.
pub fn save(root: &Path, hash: &str, notes: &CommitNotes) -> std::io::Result<()> {
    save_at(&file(root, hash), notes)
}

/// A commit's notes, if pi explained any of its files (blocking).
pub fn load(root: &Path, hash: &str) -> Option<CommitNotes> {
    load_at(&file(root, hash))
}

fn save_at(path: &Path, notes: &CommitNotes) -> std::io::Result<()> {
    if notes.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(notes).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, path)
}

fn load_at(path: &Path) -> Option<CommitNotes> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// What the open sessions say about `rels` (paths relative to the git
/// `workdir`): for each file, the session that explained it (the active one
/// first). Paths are stored relative to the project `root` (what the editor and
/// Git Log use; the same as `workdir` unless the project is a repo's subfolder).
/// Files pi didn't explain are left out.
pub fn collect(
    sessions: &crate::Sessions,
    workdir: &Path,
    root: &Path,
    rels: &[String],
    cx: &gpui_kit::App,
) -> CommitNotes {
    let active = sessions.active();
    let mut agents = vec![active.clone()];
    agents.extend(sessions.all().iter().filter(|a| **a != active).cloned());
    let mut files = Vec::new();
    for rel in rels {
        let path = workdir.join(rel);
        let rel = &path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| rel.clone());
        let found = agents.iter().find_map(|agent| {
            let a = agent.read(cx);
            let c = a
                .changes
                .iter()
                .find(|c| c.path == path && !c.reverted && (c.summary.is_some() || !c.notes.is_empty()))?;
            Some(FileReason {
                path: rel.clone(),
                session: a.name.to_string(),
                session_file: a.session_file.clone(),
                summary: c.summary.clone(),
                notes: c
                    .notes
                    .iter()
                    .filter(|n| !n.dismissed)
                    .map(|n| (n.anchor.line, n.note.clone()))
                    .collect(),
            })
        });
        files.extend(found);
    }
    CommitNotes { files }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn commit_notes_round_trip() {
        let path = std::env::temp_dir().join(format!("pide-cn-{}/abc.json", std::process::id()));
        let notes = CommitNotes {
            files: vec![FileReason {
                path: "src/a.rs".into(),
                session: "fix it".into(),
                session_file: Some("/s.jsonl".into()),
                summary: Some("why".into()),
                notes: vec![(3, "n".into())],
            }],
        };
        save_at(&path, &notes).unwrap();
        assert_eq!(load_at(&path), Some(notes));
        save_at(&path.with_file_name("empty.json"), &CommitNotes::default()).unwrap();
        assert_eq!(load_at(&path.with_file_name("empty.json")), None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
