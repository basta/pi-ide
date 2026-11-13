//! The walkthrough over what pi changed and pointed at (collaborator mode,
//! docs/COLLAB.md), as plain functions on a session's change list and pins, so
//! it can be tested without a GPUI context. [`crate::Agent`] delegates here.
//!
//! [`StopRef`]s are indices: `Pin(i)` into the pins, `Hunk { file, note }` and
//! `File(i)` into the changes. Pins and changes are only ever appended (a
//! replaced pin is hidden, not removed), so a `Pin`/`File` index stays valid;
//! a file's notes can be replaced by a new explanation, which clears a current
//! stop that pointed into them.

use std::path::{Path, PathBuf};

use crate::changes::FileChange;
use crate::collab::{Anchor, HunkNote, Pin, Stop, StopRef};

/// Every place the walkthrough stops at, in order: per changed file (first
/// touched first) its hunk notes, or one stop at its first change when pi gave
/// no notes, and the pins on that file, all by line; then pins on other files.
/// Reverted files, deleted files' file stop, and dismissed notes are left out.
pub fn stops(changes: &[FileChange], pins: &[Pin]) -> Vec<Stop> {
    let mut out = Vec::new();
    let mut pin_done = vec![false; pins.len()];
    for (fi, c) in changes.iter().enumerate() {
        if c.reverted {
            continue;
        }
        let mut file_stops: Vec<Stop> = Vec::new();
        if c.notes.is_empty() && !c.deleted_now {
            file_stops.push(Stop {
                at: StopRef::File(fi),
                path: c.path.clone(),
                line: c.first_line.unwrap_or(1),
                note: String::new(),
                summary: c.summary.clone(),
                outdated: false,
                seen: c.seen,
            });
        }
        for (ni, n) in c.notes.iter().enumerate().filter(|(_, n)| !n.dismissed) {
            file_stops.push(Stop {
                at: StopRef::Hunk { file: fi, note: ni },
                path: c.path.clone(),
                line: n.anchor.line,
                note: n.note.clone(),
                summary: c.summary.clone(),
                outdated: n.anchor.outdated,
                seen: n.seen,
            });
        }
        for (pi, p) in pins.iter().enumerate() {
            // Tour stops keep the tour's order: never regrouped into a file.
            if p.path == c.path && !is_tour(p) {
                pin_done[pi] = true;
                if !p.dismissed {
                    file_stops.push(pin_stop(pins, pi));
                }
            }
        }
        file_stops.sort_by_key(|s| s.line); // stable: equal lines keep their order
        out.extend(file_stops);
    }
    for (pi, p) in pins.iter().enumerate() {
        if !pin_done[pi] && !p.dismissed {
            out.push(pin_stop(pins, pi));
        }
    }
    out
}

/// A stop of an `ide_tour` (walked in the tour's own order).
pub fn is_tour(p: &Pin) -> bool {
    p.id.starts_with(crate::collab::TOUR_PREFIX)
}

/// A stop that is part of a tour (explains code, isn't a change to review).
pub fn is_tour_stop(pins: &[Pin], stop: &Stop) -> bool {
    matches!(stop.at, StopRef::Pin(i) if pins.get(i).is_some_and(is_tour))
}

/// Notes on changes that you haven't stepped through: unseen stops that aren't
/// part of a tour (the Commit window's "◆ n pi notes").
pub fn unseen_change_notes(changes: &[FileChange], pins: &[Pin]) -> Vec<Stop> {
    stops(changes, pins)
        .into_iter()
        .filter(|s| !s.seen && !is_tour_stop(pins, s))
        .collect()
}

/// Which of `candidates` to review first: `active` if it's one of them, else
/// the last (the newest session).
pub fn pick_review<T: PartialEq + Clone>(active: &T, candidates: &[T]) -> Option<T> {
    if candidates.contains(active) {
        Some(active.clone())
    } else {
        candidates.last().cloned()
    }
}

/// The stop for `pins[i]`.
pub fn pin_stop(pins: &[Pin], i: usize) -> Stop {
    let p = &pins[i];
    Stop {
        at: StopRef::Pin(i),
        path: p.path.clone(),
        line: p.anchor.line,
        note: p.note.clone(),
        summary: None,
        outdated: p.anchor.outdated,
        seen: p.seen,
    }
}

/// The anchor text behind a stop (to re-find it in an edited buffer).
pub fn anchor_text(changes: &[FileChange], pins: &[Pin], at: &StopRef) -> Option<String> {
    match *at {
        StopRef::Pin(i) => pins.get(i).map(|p| p.anchor.text.clone()),
        StopRef::Hunk { file, note } => changes
            .get(file)
            .and_then(|c| c.notes.get(note))
            .map(|n| n.anchor.text.clone()),
        StopRef::File(_) => None,
    }
}

/// The next (`delta` 1) or previous (−1) stop from `current`, clamped at the
/// ends; the first unseen stop (else the first) when there is no current one or
/// it no longer exists.
pub fn step(stops: &[Stop], current: Option<&StopRef>, delta: isize) -> Option<Stop> {
    if stops.is_empty() {
        return None;
    }
    let cur = current.and_then(|c| stops.iter().position(|s| &s.at == c));
    let i = match cur {
        None => stops.iter().position(|s| !s.seen).unwrap_or(0),
        Some(i) => (i as isize + delta).clamp(0, stops.len() as isize - 1) as usize,
    };
    stops.get(i).cloned()
}

/// Make `at` the current stop and mark it seen; a file whose stops are now all
/// seen is ticked reviewed.
pub fn visit(
    changes: &mut [FileChange],
    pins: &mut [Pin],
    current: &mut Option<StopRef>,
    at: &StopRef,
) {
    let path = path_of(changes, pins, at);
    set_flag(changes, pins, at, false);
    *current = Some(at.clone());
    if let Some(path) = path {
        review_if_walked(changes, pins, &path);
    }
}

/// Hide a pin or hunk note (counts as seen; a file stop is only marked seen).
/// The bar moves on if it was the current stop; a file whose remaining stops
/// are all seen is ticked reviewed.
pub fn dismiss(
    changes: &mut [FileChange],
    pins: &mut [Pin],
    current: &mut Option<StopRef>,
    at: &StopRef,
) {
    let path = path_of(changes, pins, at); // before: a dismissed stop leaves `stops()`
    set_flag(changes, pins, at, true);
    if current.as_ref() == Some(at) {
        *current = None;
    }
    if let Some(path) = path {
        review_if_walked(changes, pins, &path);
    }
}

/// pi changed a file again: it needs another look (unticked, its notes unseen).
pub fn touched_again(c: &mut FileChange) {
    c.reviewed = false;
    c.seen = false;
    c.notes.iter_mut().for_each(|n| n.seen = false);
}

/// `ide_explain_change` for `path`: set the file's reason and notes; for a file
/// with no change record (changed outside edit/write/watched bash), keep the
/// reasons as pins instead, hiding an earlier explanation's pins. `now_ms`
/// makes those pins' ids unique across saves.
pub fn explain(
    changes: &mut [FileChange],
    pins: &mut Vec<Pin>,
    current: &mut Option<StopRef>,
    path: &Path,
    summary: &str,
    hunks: &[HunkNote],
    now_ms: u128,
) {
    if let Some(fi) = changes.iter().position(|c| c.path == path) {
        let c = &mut changes[fi];
        c.summary = (!summary.trim().is_empty()).then(|| summary.to_string());
        c.notes = hunks.to_vec();
        c.seen = false;
        if matches!(current, Some(StopRef::Hunk { file, .. } | StopRef::File(file)) if *file == fi)
        {
            *current = None; // it pointed into the old notes
        }
        return;
    }
    let prefix = format!("explain:{}:", path.display());
    for p in pins.iter_mut().filter(|p| p.id.starts_with(&prefix)) {
        p.dismissed = true;
        p.seen = true;
    }
    let pin = |id: String, anchor: Anchor, note: String, snippet: Vec<String>| Pin {
        id,
        path: path.to_path_buf(),
        end_line: anchor.line,
        anchor,
        note,
        snippet,
        seen: false,
        dismissed: false,
    };
    if hunks.is_empty() {
        pins.push(pin(
            format!("{prefix}{now_ms}"),
            Anchor::new(1, String::new()),
            summary.to_string(),
            Vec::new(),
        ));
    } else {
        for (i, h) in hunks.iter().enumerate() {
            pins.push(pin(
                format!("{prefix}{now_ms}-{i}"),
                h.anchor.clone(),
                format!("{summary}: {}", h.note),
                vec![h.anchor.text.clone()],
            ));
        }
    }
}

fn path_of(changes: &[FileChange], pins: &[Pin], at: &StopRef) -> Option<PathBuf> {
    match *at {
        StopRef::Pin(i) => pins.get(i).map(|p| p.path.clone()),
        StopRef::Hunk { file, .. } | StopRef::File(file) => {
            changes.get(file).map(|c| c.path.clone())
        }
    }
}

/// All of a changed file's stops are seen: tick it reviewed (the reverse holds
/// too: ticking a file marks its stops seen, `Agent::set_reviewed`).
fn review_if_walked(changes: &mut [FileChange], pins: &[Pin], path: &Path) {
    let tour = |s: &Stop| matches!(s.at, StopRef::Pin(i) if pins.get(i).is_some_and(is_tour));
    if stops(changes, pins)
        .iter()
        .filter(|s| s.path == path && !tour(s))
        .all(|s| s.seen)
        && let Some(c) = changes.iter_mut().find(|c| c.path == path && !c.reverted)
    {
        c.reviewed = true;
    }
}

fn set_flag(changes: &mut [FileChange], pins: &mut [Pin], at: &StopRef, dismiss: bool) {
    match *at {
        StopRef::Pin(i) => {
            if let Some(p) = pins.get_mut(i) {
                p.seen = true;
                p.dismissed |= dismiss;
            }
        }
        StopRef::Hunk { file, note } => {
            if let Some(n) = changes.get_mut(file).and_then(|c| c.notes.get_mut(note)) {
                n.seen = true;
                n.dismissed |= dismiss;
            }
        }
        StopRef::File(i) => {
            if let Some(c) = changes.get_mut(i) {
                c.seen = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::Touch;

    fn change(path: &str) -> FileChange {
        let mut c = FileChange::new(
            path.into(),
            Some(String::new()),
            Touch {
                tool_call_id: "t".into(),
                tool: "edit".into(),
            },
        );
        c.first_line = Some(3);
        c
    }
    fn note(line: usize, text: &str) -> HunkNote {
        HunkNote {
            anchor: Anchor::new(line, text.into()),
            note: text.into(),
            seen: false,
            dismissed: false,
        }
    }
    fn pin(id: &str, path: &str, line: usize) -> Pin {
        Pin {
            id: id.into(),
            path: path.into(),
            anchor: Anchor::new(line, String::new()),
            end_line: line,
            note: id.into(),
            snippet: vec![],
            seen: false,
            dismissed: false,
        }
    }
    fn at(s: &[Stop]) -> Vec<(String, usize)> {
        s.iter()
            .map(|s| (s.path.display().to_string(), s.line))
            .collect()
    }

    #[::core::prelude::v1::test]
    fn order_is_files_then_lines_then_loose_pins() {
        let mut a = change("/a");
        a.notes = vec![note(9, "a9"), note(2, "a2")];
        let b = change("/b"); // no notes: one stop at its first change
        let pins = vec![pin("p-elsewhere", "/z", 1), pin("p-a", "/a", 5)];
        let s = stops(&[a, b], &pins);
        assert_eq!(
            at(&s),
            [("/a", 2), ("/a", 5), ("/a", 9), ("/b", 3), ("/z", 1)]
                .map(|(p, l)| (p.to_string(), l))
        );
        assert_eq!(s[3].at, StopRef::File(1));
    }

    #[::core::prelude::v1::test]
    fn reverted_deleted_and_dismissed_are_left_out() {
        let mut gone = change("/gone");
        gone.reverted = true;
        let mut deleted = change("/deleted");
        deleted.deleted_now = true;
        let mut a = change("/a");
        a.notes = vec![note(1, "x"), note(2, "y")];
        a.notes[0].dismissed = true;
        let mut p = pin("p", "/z", 1);
        p.dismissed = true;
        assert_eq!(
            at(&stops(&[gone, deleted, a], &[p])),
            [("/a".to_string(), 2)]
        );
    }

    #[::core::prelude::v1::test]
    fn stepping_starts_at_first_unseen_and_clamps() {
        let mut a = change("/a");
        a.notes = vec![note(1, "x"), note(2, "y"), note(3, "z")];
        a.notes[0].seen = true;
        let s = stops(&[a], &[]);
        assert_eq!(step(&s, None, 1).unwrap().line, 2);
        assert_eq!(step(&s, Some(&s[2].at), 1).unwrap().line, 3);
        assert_eq!(step(&s, Some(&s[0].at), -1).unwrap().line, 1);
        assert!(step(&[], None, 1).is_none());
        // A current stop that no longer exists: back to the first unseen.
        let stale = StopRef::Hunk { file: 0, note: 9 };
        assert_eq!(step(&s, Some(&stale), 1).unwrap().line, 2);
    }

    #[::core::prelude::v1::test]
    fn walking_a_files_last_note_ticks_it_reviewed() {
        let mut changes = vec![change("/a")];
        changes[0].notes = vec![note(1, "x"), note(2, "y")];
        let mut pins = vec![pin("p", "/a", 5)];
        let mut cur = None;
        for s in stops(&changes, &pins) {
            assert!(!changes[0].reviewed);
            visit(&mut changes, &mut pins, &mut cur, &s.at);
        }
        assert!(changes[0].reviewed);
        assert_eq!(cur, Some(StopRef::Pin(0)));
    }

    #[::core::prelude::v1::test]
    fn dismissing_moves_on_and_can_tick() {
        let mut changes = vec![change("/a")];
        changes[0].notes = vec![note(1, "x"), note(2, "y")];
        let mut pins = vec![];
        let mut cur = None;
        let first = StopRef::Hunk { file: 0, note: 0 };
        visit(&mut changes, &mut pins, &mut cur, &first);
        dismiss(&mut changes, &mut pins, &mut cur, &first);
        assert_eq!(cur, None);
        assert_eq!(stops(&changes, &pins).len(), 1);
        assert!(!changes[0].reviewed);
        dismiss(&mut changes, &mut pins, &mut cur, &StopRef::Hunk { file: 0, note: 1 });
        assert!(changes[0].reviewed);
    }

    #[::core::prelude::v1::test]
    fn a_file_changed_again_needs_another_look() {
        let mut changes = vec![change("/a")];
        changes[0].notes = vec![note(1, "x")];
        let mut cur = None;
        visit(&mut changes, &mut [], &mut cur, &StopRef::Hunk { file: 0, note: 0 });
        assert!(changes[0].reviewed);
        touched_again(&mut changes[0]);
        assert!(!changes[0].reviewed);
        assert!(stops(&changes, &[]).iter().all(|s| !s.seen));
    }

    #[::core::prelude::v1::test]
    fn a_new_explanation_resets_a_stop_into_old_notes() {
        let mut changes = vec![change("/a")];
        changes[0].notes = vec![note(1, "x"), note(2, "y")];
        let mut pins = vec![];
        let mut cur = Some(StopRef::Hunk { file: 0, note: 1 });
        explain(&mut changes, &mut pins, &mut cur, Path::new("/a"), "why", &[note(4, "z")], 1);
        assert_eq!(cur, None);
        assert_eq!(changes[0].summary.as_deref(), Some("why"));
        assert_eq!(at(&stops(&changes, &pins)), [("/a".to_string(), 4)]);
    }

    #[::core::prelude::v1::test]
    fn explaining_an_untracked_file_replaces_its_pins() {
        let mut pins = vec![];
        let mut cur = None;
        explain(&mut [], &mut pins, &mut cur, Path::new("/u"), "s", &[note(3, "n")], 1);
        explain(&mut [], &mut pins, &mut cur, Path::new("/u"), "s2", &[], 2);
        let s = stops(&[], &pins);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].note, "s2");
        assert_eq!(pins.len(), 2); // the old one is hidden, not removed (indices stay valid)
        assert_ne!(pins[0].id, pins[1].id);
    }

    #[::core::prelude::v1::test]
    fn tour_stops_keep_their_order() {
        let mut a = change("/a");
        a.notes = vec![note(1, "x")];
        let pins = vec![
            pin("tour:c:0", "/a", 9),
            pin("tour:c:1", "/b", 2),
            pin("tour:c:2", "/a", 3),
        ];
        assert_eq!(
            at(&stops(&[a], &pins)),
            [("/a", 1), ("/a", 9), ("/b", 2), ("/a", 3)].map(|(p, l)| (p.to_string(), l))
        );
    }

    #[::core::prelude::v1::test]
    fn unseen_change_notes_leave_out_tours_and_seen() {
        let mut a = change("/a");
        a.notes = vec![note(5, "x"), note(50, "y")];
        a.notes[1].seen = true;
        // A tour stop earlier in the same file must not be picked first.
        let pins = vec![pin("tour:c:0", "/a", 2), pin("p", "/a", 9)];
        let s = unseen_change_notes(&[a], &pins);
        assert_eq!(at(&s), [("/a".to_string(), 5), ("/a".to_string(), 9)]);
    }

    #[::core::prelude::v1::test]
    fn review_picks_active_else_newest() {
        assert_eq!(pick_review(&2, &[1, 2, 3]), Some(2));
        assert_eq!(pick_review(&9, &[1, 2, 3]), Some(3));
        assert_eq!(pick_review(&9, &[] as &[i32]), None);
    }

    #[::core::prelude::v1::test]
    fn outdated_anchors_still_stop() {
        let mut a = change("/a");
        a.notes = vec![note(7, "gone")];
        a.notes[0].anchor.reanchor("other\ntext\n");
        let s = stops(&[a], &[]);
        assert!(s[0].outdated);
        assert_eq!(s[0].line, 7);
    }
}
