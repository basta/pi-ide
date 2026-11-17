//! Collaborator mode in the editor (docs/COLLAB.md): the active pi session's pins
//! and hunk notes on this file as gutter marks (drawn over the git marks),
//! scrollbar marks and a tint on every note's lines (the current one stronger).
//! The notes themselves float over the code: `pi_float`.
//! All drawing is GPUI Kit's decorations; we only compute where.

use gpui_kit::component::input::{
    GutterDecoration, GutterDecorationCollection, OverviewRulerDecoration,
    OverviewRulerDecorationCollection, OverviewRulerLane, Position, RangeDecoration,
    RangeDecorationCollection, RangeDecorationStyle, Rope, RopeExt as _,
};
use gpui_kit::*;
use pide_core::collab::{Anchor, Stop};
use pide_core::{Agent, Services, SessionsEvent};

use crate::EditorPanel;

/// What this tab shows of the active session.
#[derive(Default)]
pub(crate) struct PiNotes {
    pub(crate) agent: Option<Entity<Agent>>,
    /// This file's stops, at their lines in the current text.
    pub(crate) stops: Vec<Stop>,
    gutter: Option<GutterDecorationCollection>,
    ruler: Option<OverviewRulerDecorationCollection>,
    line: Option<RangeDecorationCollection>,
    /// What the marks were last made from: every open session's notes revision
    /// and the active one. Agents notify on every streamed token; this skips that.
    last: Option<(Vec<(EntityId, u64)>, EntityId)>,
    /// Re-finding the anchors after you type (debounced).
    _typed: Option<Task<()>>,
    _sessions_sub: Option<Subscription>,
    /// Any session changed (notes of the other open sessions show too, dimmer).
    _sessions_obs: Option<Subscription>,
    sessions: Option<Entity<pide_core::Sessions>>,
    /// The open card's reply box and what the overlay last drew from (`pi_float`).
    pub(crate) reply: Option<Entity<gpui_kit::component::input::TextareaState>>,
    pub(crate) _reply_sub: Option<Subscription>,
    pub(crate) geom: crate::pi_float::Geom,
    /// The current note you collapsed (✕ / Esc): its card stays closed.
    pub(crate) collapsed: Option<pide_core::collab::StopRef>,
    /// Rendered markdown of notes and pi's answers in the active card, by text.
    pub(crate) markdown: std::collections::HashMap<String, Entity<gpui_kit::component::text::TextViewState>>,
}

impl EditorPanel {
    /// Follow the window's active pi session (call once the file is attached).
    pub(crate) fn watch_pi(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sessions = Services::of(window, cx).sessions;
        self.pi._sessions_sub = Some(cx.subscribe(&sessions, |this, sessions, ev, cx| {
            let SessionsEvent::Changed = ev;
            let active = sessions.read(cx).active();
            this.follow_agent(active, cx);
        }));
        self.pi._sessions_obs = Some(cx.observe(&sessions, |this, _, cx| this.refresh_pi_marks(cx)));
        self.pi.sessions = Some(sessions.clone());
        self.make_reply_box(window, cx);
        let active = sessions.read(cx).active();
        self.follow_agent(active, cx);
    }

    fn follow_agent(&mut self, agent: Entity<Agent>, cx: &mut Context<Self>) {
        if self.pi.agent.as_ref() == Some(&agent) {
            return;
        }
        // (No observer of its own: the Sessions observer fires for every agent.)
        self.pi.agent = Some(agent);
        self.pi_marks_stale();
        self.refresh_pi_marks(cx);
    }

    /// The text changed: find the notes' lines again once typing pauses.
    pub(crate) fn pi_text_changed(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.pi.agent.clone() else {
            return;
        };
        // The lenses follow their lines right away (an Enter above a note moves
        // it down now, not 0.4 s later); the marks are redone once typing pauses.
        if !self.pi.stops.is_empty() {
            let content = self.editor.read(cx).text().to_string();
            let a = agent.read(cx);
            for s in &mut self.pi.stops {
                if let Some(n) = relocate(a.anchor(&s.at), s.line, &content) {
                    let moved = n as isize - s.line as isize;
                    s.end_line = (s.end_line as isize + moved).max(n as isize) as usize;
                    s.line = n;
                }
            }
        }
        self.pi._typed = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.pi_marks_stale();
                this.refresh_pi_marks(cx);
            });
        }));
    }

    /// Make the next refresh redo the marks even if the notes didn't change.
    pub(crate) fn pi_marks_stale(&mut self) {
        self.pi.last = None;
    }

    /// Recompute this file's stops and their marks. The gutter collection is made
    /// again each time so it stays above the git marks (later collections draw on top).
    pub(crate) fn refresh_pi_marks(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.pi.agent.clone() else {
            return;
        };
        if self.buffer.is_none() {
            return;
        }
        let all: Vec<Entity<Agent>> = self
            .pi
            .sessions
            .as_ref()
            .map(|s| s.read(cx).all().to_vec())
            .unwrap_or_else(|| vec![agent.clone()]);
        let key = (
            all.iter()
                .map(|a| (a.entity_id(), a.read(cx).notes_rev))
                .collect::<Vec<_>>(),
            agent.entity_id(),
        );
        if self.pi.last.as_ref() == Some(&key) {
            return;
        }
        self.pi.last = Some(key);
        let rope = self.editor.read(cx).text().clone();
        let content = rope.to_string();
        let a = agent.read(cx);
        let current = a.current_stop.clone();
        let mut stops: Vec<Stop> = a
            .stops()
            .into_iter()
            .filter(|s| s.path == self.path)
            .collect();
        // Other open sessions' notes on this file: marks only, dimmer, re-found too.
        let others: Vec<usize> = all
            .iter()
            .filter(|o| **o != agent)
            .flat_map(|o| {
                let o = o.read(cx);
                o.stops()
                    .into_iter()
                    .filter(|s| s.path == self.path)
                    .map(|s| relocate(o.anchor(&s.at), s.line, &content).unwrap_or(s.line))
                    .collect::<Vec<_>>()
            })
            .collect();
        for s in &mut stops {
            if let Some(n) = relocate(a.anchor(&s.at), s.line, &content) {
                s.line = n;
            }
            // A hunk note's card opens below the whole change it explains, not
            // over its other lines (short changes; a long one keeps its line).
            s.end_line = s.line;
            if let pide_core::collab::StopRef::Hunk { file, .. } = s.at
                && let Some(before) = a.changes.get(file).and_then(|c| c.before.as_deref())
                && let Some(end) = pide_core::changes::hunk_end(before, &content, s.line, 15)
            {
                s.end_line = end;
            }
        }
        // The current note's range is stronger.
        let shown = current
            .as_ref()
            .and_then(|c| stops.iter().find(|s| &s.at == c))
            .cloned();
        let color = pide_core::colors::pi_note(cx);
        let gutter: Vec<GutterDecoration> = others
            .iter()
            .map(|l| GutterDecoration::new(line_range(&rope, *l)).with_color(color.opacity(0.4)))
            .chain(
                stops
                    .iter()
                    .map(|s| GutterDecoration::new(line_range(&rope, s.line)).with_color(color)),
            )
            .collect();
        let ruler: Vec<OverviewRulerDecoration> = stops
            .iter()
            .map(|s| {
                OverviewRulerDecoration::new(line_range(&rope, s.line))
                    .with_lane(OverviewRulerLane::Right)
                    .with_color(color)
            })
            .collect();
        // Every note's lines faintly (a pin's whole range), the shown one stronger.
        // A note on quoted words tints just them (a bit stronger, they're short) and
        // frames them when it's the current one.
        let tint: Vec<RangeDecoration> = stops
            .iter()
            .flat_map(|s| {
                let strong = shown.as_ref().is_some_and(|x| x.at == s.at);
                match span_range(&rope, s) {
                    Some(r) => {
                        let fill = RangeDecoration::new(r.clone())
                            .with_style(RangeDecorationStyle::Fill)
                            .with_color(color.opacity(if strong { 0.30 } else { 0.16 }));
                        let frame = strong.then(|| {
                            RangeDecoration::new(r)
                                .with_style(RangeDecorationStyle::Frame)
                                .with_color(color.opacity(0.8))
                        });
                        std::iter::once(fill).chain(frame).collect::<Vec<_>>()
                    }
                    None => {
                        let start = line_range(&rope, s.line).start;
                        let end = line_range(&rope, s.end_line.max(s.line)).end;
                        vec![RangeDecoration::new(start..end)
                            .with_style(RangeDecorationStyle::Fill)
                            .with_color(color.opacity(if strong { 0.20 } else { 0.08 }))]
                    }
                }
            })
            .collect();

        if let Some(g) = self.pi.gutter.take() {
            g.dispose(cx);
        }
        if !gutter.is_empty() {
            self.pi.gutter = Some(self.editor.update(cx, |e, cx| {
                e.create_gutter_decorations_collection(gutter, cx)
            }));
        }
        match &self.pi.ruler {
            Some(r) => r.set(ruler, cx),
            None if !ruler.is_empty() => {
                self.pi.ruler = Some(self.editor.update(cx, |e, cx| {
                    e.create_overview_ruler_decorations_collection(ruler, cx)
                }));
            }
            None => {}
        }
        match &self.pi.line {
            Some(l) => l.set(tint, cx),
            None if !tint.is_empty() => {
                self.pi.line = Some(
                    self.editor
                        .update(cx, |e, cx| e.create_range_decorations_collection(tint, cx)),
                );
            }
            None => {}
        }
        self.pi.stops = stops;
        cx.notify();
    }
}

/// Where a stop's anchor is in `content` now (1-based), starting from `line`:
/// by its line text, or by its quoted words when the line changed.
fn relocate(anchor: Option<Anchor>, line: usize, content: &str) -> Option<usize> {
    let mut anchor = anchor?;
    anchor.line = line;
    anchor.locate(content)
}

/// The byte range of a stop's quoted words in the buffer, if it has some and
/// they're still on its line (else the note covers its whole line).
pub(crate) fn span_range(rope: &Rope, stop: &Stop) -> Option<std::ops::Range<usize>> {
    let span = stop.span.as_ref()?;
    let line = line_range(rope, stop.line);
    let text = rope.slice(line.clone()).to_string();
    let r = span.find_in(&text)?;
    Some(line.start + r.start..line.start + r.end)
}

/// 1-based `line` as a byte range (the whole line; clamped to the text).
pub(crate) fn line_range(rope: &Rope, line: usize) -> std::ops::Range<usize> {
    let count = rope.lines_len().max(1);
    let l = line.clamp(1, count) - 1;
    let start = rope.position_to_offset(&Position::new(l as u32, 0));
    let end = rope.line_end_offset(l).max(start);
    start..end
}
