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

/// A tour (`ide_tour`) in some session.
#[derive(Clone, Debug, PartialEq)]
pub struct TourInfo {
    pub title: String,
    /// The tool call id (its pins are `tour:<call id>:<n>`).
    pub call_id: String,
    pub stops: usize,
    pub seen: usize,
}

/// The tours among a session's pins, in the order they were given.
pub fn tours(pins: &[crate::collab::Pin]) -> Vec<TourInfo> {
    let mut out: Vec<TourInfo> = Vec::new();
    for p in pins.iter().filter(|p| !p.dismissed) {
        let Some(rest) = p.id.strip_prefix(crate::collab::TOUR_PREFIX) else {
            continue;
        };
        let call = rest.rsplit_once(':').map_or(rest, |(c, _)| c);
        match out.iter_mut().find(|t| t.call_id == call) {
            Some(t) => {
                t.stops += 1;
                t.seen += p.seen as usize;
            }
            None => out.push(TourInfo {
                title: match crate::collab::split_tour_note(&p.note).0 {
                    "" => "Tour".to_string(),
                    t => t.to_string(),
                },
                call_id: call.to_string(),
                stops: 1,
                seen: p.seen as usize,
            }),
        }
    }
    out
}

/// Tours saved by this project's sessions (session file → its tours), skipping
/// `skip`. Blocking: call off the UI thread.
pub fn saved_tours(root: &Path, skip: &[PathBuf]) -> Vec<(PathBuf, Vec<TourInfo>)> {
    crate::sessions::list_saved(root)
        .into_iter()
        .filter(|s| !skip.contains(&s.path))
        .filter_map(|s| {
            let t = tours(&crate::changes::load(&s.path).pins);
            (!t.is_empty()).then_some((s.path, t))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::{Anchor, Pin};

    #[::core::prelude::v1::test]
    fn tours_group_their_pins() {
        let pin = |id: &str, note: &str, seen: bool| Pin {
            id: id.into(),
            path: "/a".into(),
            anchor: Anchor::new(1, String::new()),
            end_line: 1,
            note: note.into(),
            snippet: vec![],
            seen,
            dismissed: false,
            thread: Vec::new(),
        };
        let t = tours(&[
            pin("tour:functions.ide_tour:3:0", "Resume: flow · 1/2: a", true),
            pin("x", "not a tour", false),
            pin(
                "tour:functions.ide_tour:3:1",
                "Resume: flow · 2/2: b",
                false,
            ),
            pin("tour:c2:0", "Other · 1/1: c", false),
        ]);
        assert_eq!(t.len(), 2);
        assert_eq!(
            (
                t[0].title.as_str(),
                t[0].call_id.as_str(),
                t[0].stops,
                t[0].seen
            ),
            ("Resume: flow", "functions.ide_tour:3", 2, 1)
        );
        assert_eq!(t[1].title, "Other");
    }
}
