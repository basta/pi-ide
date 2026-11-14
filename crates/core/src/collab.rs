//! Collaborator mode (docs/COLLAB.md): what pi points you at in the code.
//! Pins (`ide_pin`), per-file reasons and hunk notes (`ide_explain_change`, kept
//! on [`crate::changes::FileChange`]), a checklist (`ide_progress`), and the
//! walkthrough that steps through all of it.

use std::path::PathBuf;

use gpui_kit::{App, Entity, Window};
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
    /// The pinned lines as they were (at most 8), to show with the note.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snippet: Vec<String>,
    /// You stepped onto it (walkthrough) or opened it.
    #[serde(default)]
    pub seen: bool,
    /// You dismissed it: no mark or bar any more.
    #[serde(default)]
    pub dismissed: bool,
    /// Your replies on it and pi's answers, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thread: Vec<Reply>,
}

/// One message in a note's thread (a reply in the editor's margin card).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// pi's answer (else yours).
    #[serde(default)]
    pub pi: bool,
    pub text: String,
}

/// The title "Tell it as a story" asks pi to give its tour; Walk Through Changes
/// follows the newest such tour when there is one.
pub const STORY_TITLE: &str = "The change, step by step";

/// Pins from an `ide_tour` have ids `tour:<tool call id>:<n>`.
pub const TOUR_PREFIX: &str = "tour:";

/// A tour stop's note is "<title> · <i>/<n>: <note>"; split it into (title, note)
/// at the first " · i/n: " (either part may contain " · " or ": " itself).
pub fn split_tour_note(text: &str) -> (&str, &str) {
    for (at, sep) in text.match_indices(" · ") {
        let rest = &text[at + sep.len()..];
        let Some((count, body)) = rest.split_once(": ") else {
            continue;
        };
        let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        if count.split_once('/').is_some_and(|(i, n)| numeric(i) && numeric(n)) {
            return (&text[..at], body);
        }
    }
    ("", text)
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
    /// See [`Pin::thread`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thread: Vec<Reply>,
}

/// One `ide_progress` step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressStep {
    pub text: String,
    pub done: bool,
}

/// Answers pide's extension's questions (`{"kind": "diagnostics", ...}`) with JSON.
/// The app installs one per project (it can see the language servers); runs on
/// the UI thread, so it must only read in-memory state.
pub type QueryHandler = std::rc::Rc<dyn Fn(&serde_json::Value, &mut App) -> serde_json::Value>;

/// A "catch me up" briefing: what happened in a session, for someone who
/// didn't watch it. Written by the model on the side (`Agent::ask_aside`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatchUp {
    /// Markdown.
    pub text: String,
    /// When it was written (Unix seconds).
    pub at: u64,
}

/// The instruction for a briefing; `record` is the session digest the panel builds.
pub fn catch_up_prompt(record: &str) -> String {
    format!(
        "You are catching a developer up on a coding session that an AI agent (pi) ran in their \
         project while they weren't watching. They want to understand the code, not just trust it. \
         From the record below, write a short briefing in Markdown with these parts, each a few \
         bullets: **Asked** (what they asked for), **Done** (what pi changed and why, by file), \
         **Needs you** (open questions, pins, unfinished plan steps, failures; say \"nothing\" if \
         none), **Start here** (the one or two places to read first). Under 200 words. Use only \
         facts from the record; don't invent details. Reply with only the briefing.\n\n\
         <record>\n{record}\n</record>"
    )
}

/// What a walkthrough stop refers to inside an [`crate::Agent`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
    /// Last line of the range it covers (1-based, ≥ `line`; a pin's range).
    pub end_line: usize,
    pub note: String,
    /// The file's one-line reason, if pi gave one.
    pub summary: Option<String>,
    pub outdated: bool,
    pub seen: bool,
    /// Replies on it (see [`Pin::thread`]).
    pub thread: Vec<Reply>,
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

    /// The start of a reply about this stop: where, and which note it answers.
    pub fn reply_prefix(&self, root: &std::path::Path) -> String {
        let note = self.label();
        let note = if note.chars().count() > 160 {
            format!("{}…", note.chars().take(160).collect::<String>())
        } else {
            note
        };
        format!("{} (about your note \"{}\"): ", self.location(root), note.replace('\n', " "))
    }

    /// The prompt for a reply typed in the note's card: where, which note,
    /// your text, and to answer without changing code unless asked.
    pub fn reply_prompt(&self, root: &std::path::Path, text: &str) -> String {
        format!(
            "{}{}\n\n(Answer this in a short reply. Don't change code unless I ask you to.)",
            self.reply_prefix(root),
            text.trim()
        )
    }

    /// "path:line" relative to `root`, for a reply's location prefix.
    pub fn location(&self, root: &std::path::Path) -> String {
        let rel = self.path.strip_prefix(root).unwrap_or(&self.path);
        format!("{}:{}", rel.display(), self.line)
    }
}

/// Where `stop` is now: its anchor re-found in the open buffer, if the file is open.
pub fn live_line(stop: &Stop, anchor_text: Option<&str>, window: &Window, cx: &App) -> usize {
    let Some(text) = anchor_text else {
        return stop.line;
    };
    let services = crate::Services::of(window, cx);
    let Some(buffer) = services.workspace.read(cx).buffer_for(&stop.path, cx) else {
        return stop.line;
    };
    let content = buffer.read(cx).text().to_string();
    Anchor::new(stop.line, text.to_string())
        .locate(&content)
        .unwrap_or(stop.line)
}

/// Open `stop` in the editor (cursor on its line), make it the walkthrough's
/// current stop and mark it seen.
pub fn go_to(agent: &Entity<crate::Agent>, stop: Stop, window: &mut Window, cx: &mut App) {
    let text = agent.read(cx).anchor_text(&stop.at);
    let line = live_line(&stop, text.as_deref(), window, cx);
    agent.update(cx, |a, cx| a.visit_stop(&stop.at, cx));
    crate::opener::open_file_at(stop.path, line.saturating_sub(1) as u32, 0, window, cx);
}

/// Step the walkthrough of `agent` by `delta` (1 next, −1 back; from no current
/// stop: the first unseen one). Returns false if there is nothing to walk.
pub fn walk(agent: &Entity<crate::Agent>, delta: isize, window: &mut Window, cx: &mut App) -> bool {
    match agent.read(cx).step_stop(delta) {
        Some(stop) => {
            go_to(agent, stop, window, cx);
            true
        }
        None => false,
    }
}

/// Start the walkthrough of `agent` from its first unseen stop (or the first).
pub fn start_walk(agent: &Entity<crate::Agent>, window: &mut Window, cx: &mut App) -> bool {
    agent.update(cx, |a, _| a.current_stop = None);
    // The change told as a story walks in the story's order, while it has stops to see.
    if let Some(story) = crate::walk::story_with_unseen(&agent.read(cx).pins) {
        return start_tour(agent, &story, window, cx);
    }
    walk(agent, 1, window, cx)
}

/// Walk the tour made by tool call `call_id`: go to its first unseen stop (or
/// its first). False if the agent has no such tour (yet).
pub fn start_tour(agent: &Entity<crate::Agent>, call_id: &str, window: &mut Window, cx: &mut App) -> bool {
    let prefix = format!("{TOUR_PREFIX}{call_id}:");
    let stops: Vec<Stop> = {
        let a = agent.read(cx);
        a.pins
            .iter()
            .filter(|p| p.id.starts_with(&prefix) && !p.dismissed)
            .filter_map(|p| a.pin_stop_by_id(&p.id))
            .collect()
    };
    let Some(stop) = stops.iter().find(|s| !s.seen).or(stops.first()).cloned() else {
        return false;
    };
    go_to(agent, stop, window, cx);
    true
}

/// Open sessions with changes or pins you haven't stepped through yet.
pub fn sessions_to_review(sessions: &crate::Sessions, cx: &App) -> Vec<Entity<crate::Agent>> {
    sessions
        .all()
        .iter()
        .filter(|a| a.read(cx).needs_review())
        .cloned()
        .collect()
}

/// Show a session that needs review (the active one, else the newest) and
/// start its walkthrough.
pub fn review_next(window: &mut Window, cx: &mut App) -> bool {
    let sessions = crate::Services::of(window, cx).sessions;
    // The one you're looking at first, then the newest: usually the one that just finished.
    let active = sessions.read(cx).active();
    let list = sessions_to_review(sessions.read(cx), cx);
    let Some(agent) = crate::walk::pick_review(&active, &list) else {
        return false;
    };
    sessions.update(cx, |s, cx| s.activate(&agent, cx));
    start_walk(&agent, window, cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn tour_notes_split_at_the_counter() {
        assert_eq!(split_tour_note("Resume: flow · 2/8: reads a · b: c"), ("Resume: flow", "reads a · b: c"));
        assert_eq!(split_tour_note("plain note"), ("", "plain note"));
    }

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
