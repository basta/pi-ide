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
use gpui_kit::component::{ActiveTheme, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::collab::{Anchor, Stop};
use pide_core::{Agent, Services, SessionsEvent};

use crate::EditorPanel;

/// What this tab shows of the active session.
#[derive(Default)]
pub(crate) struct PiNotes {
    agent: Option<Entity<Agent>>,
    /// This file's stops, at their lines in the current text.
    stops: Vec<Stop>,
    gutter: Option<GutterDecorationCollection>,
    ruler: Option<OverviewRulerDecorationCollection>,
    line: Option<RangeDecorationCollection>,
    /// What the marks were last made from; the agent notifies on every streamed
    /// token, so unchanged notes skip the work.
    last: Option<(
        Vec<(pide_core::collab::StopRef, usize, bool, bool)>,
        Option<pide_core::collab::StopRef>,
        u64,
    )>,
    _agent_sub: Option<Subscription>,
    _sessions_sub: Option<Subscription>,
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
        let active = sessions.read(cx).active();
        self.follow_agent(active, cx);
    }

    fn follow_agent(&mut self, agent: Entity<Agent>, cx: &mut Context<Self>) {
        if self.pi.agent.as_ref() == Some(&agent) {
            return;
        }
        self.pi._agent_sub = Some(cx.observe(&agent, |this, _, cx| this.refresh_pi_marks(cx)));
        self.pi.agent = Some(agent);
        self.pi_marks_stale();
        self.refresh_pi_marks(cx);
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
        let a = agent.read(cx);
        let current = a.current_stop.clone();
        let mut stops: Vec<Stop> = a
            .stops()
            .into_iter()
            .filter(|s| s.path == self.path)
            .collect();
        let key = (
            stops
                .iter()
                .map(|s| (s.at.clone(), s.line, s.seen, s.outdated))
                .collect::<Vec<_>>(),
            current.clone(),
            self.buffer
                .as_ref()
                .map(|b| b.read(cx).revision())
                .unwrap_or(0),
        );
        if self.pi.last.as_ref() == Some(&key) {
            return;
        }
        self.pi.last = Some(key);
        let rope = self.editor.read(cx).text().clone();
        let content = rope.to_string();
        for s in &mut stops {
            if let Some(text) = a.anchor_text(&s.at)
                && let Some(n) = Anchor::new(s.line, text).locate(&content)
            {
                s.line = n;
            }
        }
        let shown = shown_stop(&stops, current.as_ref()).cloned();
        let color = pide_core::colors::pi_note(cx);
        let gutter: Vec<GutterDecoration> = stops
            .iter()
            .map(|s| GutterDecoration::new(line_range(&rope, s.line)).with_color(color))
            .collect();
        let ruler: Vec<OverviewRulerDecoration> = stops
            .iter()
            .map(|s| {
                OverviewRulerDecoration::new(line_range(&rope, s.line))
                    .with_lane(OverviewRulerLane::Right)
                    .with_color(color)
            })
            .collect();
        let tint: Vec<RangeDecoration> = shown
            .iter()
            .map(|s| {
                RangeDecoration::new(line_range(&rope, s.line))
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(color.opacity(0.10))
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
        let location = stop.location(&root);
        let theme = cx.theme();
        let color = pide_core::colors::pi_note(cx);
        let (a_reply, a_dismiss, a_back, a_next) =
            (agent.clone(), agent.clone(), agent.clone(), agent.clone());
        let at = stop.at.clone();
        let at_reply = stop.at.clone();
        Some(
            h_flex()
                .w_full()
                .gap_2()
                .px_3()
                .py_1()
                .text_sm()
                .bg(color.opacity(0.12))
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(color)
                        .child(format!("pi · {session}")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(stop.label())
                        .when(stop.outdated, |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("outdated: the line it was about changed"),
                            )
                        }),
                )
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
                            pide_core::opener::ask_agent(format!("{location}: "), window, cx);
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
                )
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
fn line_range(rope: &Rope, line: usize) -> std::ops::Range<usize> {
    let count = rope.lines_len().max(1);
    let l = line.clamp(1, count) - 1;
    let start = rope.position_to_offset(&Position::new(l as u32, 0));
    let end = rope.line_end_offset(l).max(start);
    start..end
}
