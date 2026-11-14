//! Collaborator mode in the editor (docs/COLLAB.md): the active pi session's pins
//! and hunk notes on this file as gutter marks (drawn over the git marks),
//! scrollbar marks, a tint on the shown note's lines, and the note bar above the
//! editor ("pi · <session>: <note>" with Reply, Dismiss, Back, Next).
//! All drawing is GPUI Kit's decorations; we only compute where.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    GutterDecoration, GutterDecorationCollection, OverviewRulerDecoration,
    OverviewRulerDecorationCollection, OverviewRulerLane, Position, RangeDecoration,
    RangeDecorationCollection, RangeDecorationStyle, Rope, RopeExt as _,
};
use gpui_kit::component::{ActiveTheme, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
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
    /// The margin's reply box and what the margin last drew from (`pi_margin`).
    pub(crate) reply: Option<Entity<gpui_kit::component::input::TextareaState>>,
    pub(crate) _reply_sub: Option<Subscription>,
    pub(crate) geom: crate::pi_margin::Geom,
    /// The last render showed the margin (its width counts toward the tab's).
    pub(crate) margin_was_on: bool,
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
        if self.pi.agent.is_none() {
            return;
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
                    .map(|s| {
                        o.anchor_text(&s.at)
                            .and_then(|t| Anchor::new(s.line, t).locate(&content))
                            .unwrap_or(s.line)
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        for s in &mut stops {
            if let Some(text) = a.anchor_text(&s.at)
                && let Some(n) = Anchor::new(s.line, text).locate(&content)
            {
                s.line = n;
            }
        }
        // In the margin only the active card's range is stronger; the bar shows
        // (and so stresses) the first unseen note too.
        let shown = if self.pi.margin_was_on {
            current
                .as_ref()
                .and_then(|c| stops.iter().find(|s| &s.at == c))
                .cloned()
        } else {
            shown_stop(&stops, current.as_ref()).cloned()
        };
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
        let tint: Vec<RangeDecoration> = stops
            .iter()
            .map(|s| {
                let strong = shown.as_ref().is_some_and(|x| x.at == s.at);
                let start = line_range(&rope, s.line).start;
                let end = line_range(&rope, s.end_line.max(s.line)).end;
                RangeDecoration::new(start..end)
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(color.opacity(if strong { 0.20 } else { 0.08 }))
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

    /// The note bar, when the active session has a note on this file.
    pub(crate) fn render_pi_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let agent = self.pi.agent.clone()?;
        let a = agent.read(cx);
        let stop = shown_stop(&self.pi.stops, a.current_stop.as_ref())?.clone();
        let all = a.stops();
        let index = all.iter().position(|s| s.at == stop.at);
        let total = all.len();
        let session = a.name.clone();
        let root = self.workspace.read(cx).root().to_path_buf();
        let reply = stop.reply_prefix(&root);
        let theme = cx.theme();
        let color = pide_core::colors::pi_note(cx);
        let (a_reply, a_dismiss, a_back, a_next) =
            (agent.clone(), agent.clone(), agent.clone(), agent.clone());
        let at = stop.at.clone();
        let at_reply = stop.at.clone();
        // Who and where on top (with the buttons), the note itself below at full width.
        let head = h_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .flex_none()
                    .max_w(px(220.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(format!("pi · {session}")),
            )
            .child(div().flex_1())
            .when_some(index, |this, i| {
                this.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{}/{total}", i + 1)),
                )
            })
            .child(
                Button::new("pi-note-reply")
                    .xsmall()
                    .label("Reply")
                    .tooltip("Answer pi in this session, about this spot")
                    .on_click(move |_, window, cx| {
                        a_reply.update(cx, |a, cx| a.visit_stop(&at_reply, cx));
                        pide_core::opener::ask_agent(reply.clone(), window, cx);
                    }),
            )
            .child(
                Button::new("pi-note-dismiss")
                    .xsmall()
                    .ghost()
                    .label("Dismiss")
                    .on_click(move |_, _, cx| {
                        a_dismiss.update(cx, |a, cx| a.dismiss_stop(&at, cx));
                    }),
            )
            .child(
                Button::new("pi-note-back")
                    .xsmall()
                    .ghost()
                    .label("Back")
                    .on_click(move |_, window, cx| {
                        pide_core::collab::walk(&a_back, -1, window, cx);
                    }),
            )
            .child(
                Button::new("pi-note-next")
                    .xsmall()
                    .ghost()
                    .label("Next")
                    .on_click(move |_, window, cx| {
                        pide_core::collab::walk(&a_next, 1, window, cx);
                    }),
            );
        Some(
            v_flex()
                .w_full()
                .gap_0p5()
                .px_3()
                .py_1()
                .text_sm()
                .bg(color.opacity(0.12))
                .border_b_1()
                .border_color(theme.border)
                .child(head)
                .child(div().w_full().child(stop.label()).when(stop.outdated, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("outdated: the line it was about changed"),
                    )
                }))
                .into_any_element(),
        )
    }
}

/// The stop the bar shows: the walkthrough's current stop if it's in this file,
/// else this file's first unseen one.
fn shown_stop<'a>(
    stops: &'a [Stop],
    current: Option<&pide_core::collab::StopRef>,
) -> Option<&'a Stop> {
    current
        .and_then(|c| stops.iter().find(|s| &s.at == c))
        .or_else(|| stops.iter().find(|s| !s.seen))
}

/// 1-based `line` as a byte range (the whole line; clamped to the text).
pub(crate) fn line_range(rope: &Rope, line: usize) -> std::ops::Range<usize> {
    let count = rope.lines_len().max(1);
    let l = line.clamp(1, count) - 1;
    let start = rope.position_to_offset(&Position::new(l as u32, 0));
    let end = rope.line_end_offset(l).max(start);
    start..end
}
