//! Notes that outlive sessions (collaborator mode): every session's pins, hunk
//! notes and reasons on one file, from open sessions in memory and from closed
//! ones' saved state (re-anchored against the file on disk).

use std::path::{Path, PathBuf};

use crate::collab::Stop;

/// A note on a file from some session.
#[derive(Clone, Debug, PartialEq)]
pub struct FileNote {
    /// 1-based.
    pub line: usize,
    pub note: String,
    /// The session's name.
    pub session: String,
    pub outdated: bool,
}

impl FileNote {
    pub fn from_stop(stop: &Stop, session: &str) -> Self {
        Self {
            line: stop.line,
            note: stop.label(),
            session: session.to_string(),
            outdated: stop.outdated,
        }
    }
}

/// Notes on `path` saved by this project's closed sessions (all saved sessions
/// whose file isn't in `skip`). Blocking: reads every saved session's state, so
/// call it off the UI thread.
pub fn saved_notes(root: &Path, path: &Path, skip: &[PathBuf]) -> Vec<FileNote> {
    let mut out = Vec::new();
    for s in crate::sessions::list_saved(root) {
        if skip.contains(&s.path) {
            continue;
        }
        let saved = crate::changes::load(&s.path);
        if saved.is_empty() {
            continue;
        }
        let name = s
            .name
            .clone()
            .unwrap_or_else(|| crate::agent::name_from_prompt(&s.preview));
        for stop in crate::walk::stops(&saved.changes, &saved.pins) {
            if stop.path == path {
                out.push(FileNote::from_stop(&stop, &name));
            }
        }
    }
    out
}
