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
    /// The words on the line the note is about (pi's `quote`), when it named
    /// some: only they are marked, not the whole line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
}

/// Part of an anchor's line: the quoted words and where they started (a UTF-8
/// byte offset in the line) when the note was made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub quote: String,
    pub col: usize,
}

impl Span {
    /// Where the quote is in `line` now, as a byte range: the occurrence nearest
    /// to where it was. `None` if the words aren't in the line any more.
    pub fn find_in(&self, line: &str) -> Option<std::ops::Range<usize>> {
        if self.quote.is_empty() {
            return None;
        }
        line.match_indices(self.quote.as_str())
            .map(|(i, _)| i)
            .min_by_key(|i| i.abs_diff(self.col))
            .map(|i| i..i + self.quote.len())
    }
}

impl Anchor {
    pub fn new(line: usize, text: String) -> Self {
        Self {
            line: line.max(1),
            text,
            outdated: false,
            span: None,
        }
    }

    /// With the words the note is about (`None` when `quote` is empty).
    pub fn with_span(mut self, quote: String, col: usize) -> Self {
        self.span = (!quote.is_empty()).then_some(Span { quote, col });
        self
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
        let nearest = |hit: &dyn Fn(&str) -> bool| {
            let mut best: Option<usize> = None;
            for (i, l) in lines.iter().enumerate() {
                if hit(l) {
                    let n = i + 1;
                    if best.is_none_or(|b| n.abs_diff(self.line) < b.abs_diff(self.line)) {
                        best = Some(n);
                    }
                }
            }
            best
        };
        nearest(&|l: &str| l.trim_end() == want).or_else(|| {
            // The line changed (a prose paragraph is one line: any edit in it
            // changes it), but the quoted words may still be there.
            let quote = self.span.as_ref().map(|s| s.quote.as_str()).unwrap_or("");
            if quote.trim().chars().count() < 3 {
                return None;
            }
            nearest(&|l: &str| l.contains(quote))
        })
    }

    /// Move to where the text is now, or mark outdated. Returns whether anything changed.
    pub fn reanchor(&mut self, content: &str) -> bool {
        let before = (self.line, self.outdated, self.text.clone(), self.span.clone());
        match self.locate(content) {
            Some(n) => {
                self.line = n;
                self.outdated = false;
                let now = content.split('\n').nth(n - 1).unwrap_or("");
                if let Some(span) = &mut self.span
                    && let Some(r) = span.find_in(now)
                {
                    span.col = r.start;
                    // Found by its words in a changed line: that line is the anchor now.
                    if now.trim_end() != self.text.trim_end() && !self.text.trim().is_empty() {
                        self.text = now.to_string();
                    }
                }
            }
            None => self.outdated = true,
        }
        before != (self.line, self.outdated, self.text.clone(), self.span.clone())
    }

    /// The marked words' byte range within 1-based line `line` of `content`
    /// (`None`: no span, or its words aren't on that line any more).
    pub fn span_in(&self, content: &str, line: usize) -> Option<std::ops::Range<usize>> {
        let span = self.span.as_ref()?;
        span.find_in(content.split('\n').nth(line.checked_sub(1)?)?)
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
    match parse_tour_note(text) {
        Some((title, _, _, note)) => (title, note),
        None => ("", text),
    }
}

/// A tour stop's note as (title, i, n, note), 1-based `i`; `None` for other notes.
pub fn parse_tour_note(text: &str) -> Option<(&str, usize, usize, &str)> {
    for (at, sep) in text.match_indices(" · ") {
        let rest = &text[at + sep.len()..];
        let Some((count, body)) = rest.split_once(": ") else {
            continue;
        };
        let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        if let Some((i, n)) = count.split_once('/')
            && numeric(i)
            && numeric(n)
            && let (Ok(i), Ok(n)) = (i.parse(), n.parse())
        {
            return Some((&text[..at], i, n, body));
        }
    }
    None
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
    /// The words it's about, when pi quoted some (see [`Anchor::span`]).
    pub span: Option<Span>,
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
            "{}{}{REPLY_INSTRUCTION}",
            self.reply_prefix(root),
            text.trim()
        )
    }

    /// "path:line" relative to `root`, for a reply's location prefix.
    /// (See [`split_reply`] for reading one back.)
    pub fn location(&self, root: &std::path::Path) -> String {
        let rel = self.path.strip_prefix(root).unwrap_or(&self.path);
        format!("{}:{}", rel.display(), self.line)
    }
}

/// Where `stop` is now: its anchor re-found in the open buffer (else the file
/// on disk), as a 1-based line and the character column of its quoted words (0
/// without any), so the cursor lands on them in a long soft-wrapped line.
pub fn live_place(stop: &Stop, anchor: Option<Anchor>, window: &Window, cx: &App) -> (usize, u32) {
    let Some(mut anchor) = anchor else {
        return (stop.line, 0);
    };
    anchor.line = stop.line;
    let services = crate::Services::of(window, cx);
    let content = match services.workspace.read(cx).buffer_for(&stop.path, cx) {
        Some(buffer) => buffer.read(cx).text().to_string(),
        None => match std::fs::read_to_string(&stop.path) {
            Ok(t) => t,
            Err(_) => return (stop.line, 0),
        },
    };
    let line = anchor.locate(&content).unwrap_or(stop.line);
    let col = anchor
        .span_in(&content, line)
        .and_then(|r| {
            let text = content.split('\n').nth(line - 1)?;
            Some(text.get(..r.start)?.chars().count() as u32)
        })
        .unwrap_or(0);
    (line, col)
}

/// Open `stop` in the editor (cursor on its line, or on its quoted words), make
/// it the walkthrough's current stop and mark it seen.
pub fn go_to(agent: &Entity<crate::Agent>, stop: Stop, window: &mut Window, cx: &mut App) {
    let anchor = agent.read(cx).anchor(&stop.at);
    let (line, col) = live_place(&stop, anchor, window, cx);
    agent.update(cx, |a, cx| a.visit_stop(&stop.at, cx));
    crate::opener::open_file_at(stop.path, line.saturating_sub(1) as u32, col, window, cx);
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


/// A reply to a note as sent to pi ([`Stop::reply_prefix`] + your words, maybe
/// [`Stop::reply_prompt`]'s instruction) → the note's location and your words,
/// so the chat shows what you typed rather than the wrapper.
pub fn split_reply(prompt: &str) -> Option<(&str, &str)> {
    let (loc, rest) = prompt.split_once(" (about your note \"")?;
    if loc.contains(char::is_whitespace) || !loc.contains(':') {
        return None;
    }
    let (_, words) = rest.split_once("\"): ")?;
    let words = words
        .strip_suffix(REPLY_INSTRUCTION)
        .unwrap_or(words)
        .trim_end();
    Some((loc, words))
}

const REPLY_INSTRUCTION: &str =
    "\n\n(Answer this in a short reply. Don't change code unless I ask you to.)";

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn replies_read_back_as_your_words() {
        let p = format!("src/report.rs:5 (about your note \"An empty store: prints a line.\"): Keep it{REPLY_INSTRUCTION}");
        assert_eq!(split_reply(&p), Some(("src/report.rs:5", "Keep it")));
        assert_eq!(split_reply("src/a.rs:3 (about your note \"x\"): yes"), Some(("src/a.rs:3", "yes")));
        assert_eq!(split_reply("just a question (about your note \"x\"): y"), None);
    }

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

    #[::core::prelude::v1::test]
    fn spans_follow_their_words() {
        let para = "The solution has two parts. Then two more.";
        let a = Anchor::new(2, para.into()).with_span("two".into(), 17);
        // The nearest occurrence to where it was.
        assert_eq!(a.span_in(&format!("x\n{para}"), 2), Some(17..20));
        // Words added before it in the same paragraph: the line changed, the
        // quote still places it, and reanchor takes the new line as the anchor.
        let a = Anchor::new(2, para.into()).with_span("has two parts".into(), 13);
        let now = "x\ny\nSo the solution has two parts. Then two more.";
        assert_eq!(a.locate(now), Some(3));
        let mut b = a.clone();
        assert!(b.reanchor(now));
        assert_eq!((b.line, b.span.as_ref().unwrap().col, b.outdated), (3, 16, false));
        assert_eq!(b.text, "So the solution has two parts. Then two more.");
        // The words are gone too: outdated.
        let mut c = a.clone();
        c.reanchor("x\nThe solution has three steps.");
        assert!(c.outdated);
        // An empty quote is no span; a too short one doesn't place a changed line.
        assert!(Anchor::new(1, "a".into()).with_span(String::new(), 0).span.is_none());
        assert_eq!(Anchor::new(1, "a b".into()).with_span("b".into(), 2).locate("q\nx b"), None);
    }
}
