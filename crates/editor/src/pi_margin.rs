//! pi's notes as comment cards in a margin beside the code, like Google Docs or
//! Overleaf comments (docs/COLLAB.md, "Notes in the margin"). Each card sits
//! level with its note's first line; cards that would overlap are pushed apart
//! (the active one stays level, the others move up or down). The active card
//! shows the whole note, its thread, and a reply box that asks pi in the session.
//!
//! Positions come from GPUI Kit's `EditorState::range_to_bounds`, which reports
//! the bounds the editor laid out when it last painted. We correct them by how far
//! the editor has scrolled since, and ask for one more frame whenever what we
//! drew from turns out to be stale (a line came into view, a card's height changed).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::collab::{Stop, StopRef};
use pide_core::Agent;

use crate::EditorPanel;

/// The margin's width.
pub(crate) const MARGIN_W: f32 = 300.;
/// The tab must be at least this wide for the margin; narrower tabs keep the note bar.
const MIN_TAB_W: f32 = 900.;
/// Space between stacked cards.
const GAP: f32 = 6.;
/// A card's height before it has been laid out once.
const GUESS_H: f32 = 64.;

/// What the margin drew from, and what the last paint saw.
#[derive(Default)]
pub(crate) struct MarginGeom {
    /// At the last paint: the editor's vertical scroll offset and the margin's top (window y).
    painted: Option<(Pixels, Pixels)>,
    /// Laid-out card heights, by note.
    heights: HashMap<StopRef, Pixels>,
    /// What the last render used: the scroll offset, the margin's top, which
    /// cards were on screen, in order, and their heights.
    used_scroll: Pixels,
    used_top: Option<Pixels>,
    used_visible: Vec<StopRef>,
    used_heights: Vec<Pixels>,
}

impl EditorPanel {
    /// The reply box (made once, with the tab).
    pub(crate) fn make_reply_box(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reply = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .placeholder("Reply to pi… (⌘↩ sends)")
        });
        self.pi._reply_sub = Some(cx.subscribe_in(
            &reply,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter {
                    secondary: true, ..
                } => this.send_reply(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        ));
        self.pi.reply = Some(reply);
    }

    /// Whether the notes show as cards in the margin (else the note bar).
    pub(crate) fn margin_on(&self, cx: &App) -> bool {
        if self.pi.stops.is_empty() || self.pi.agent.is_none() {
            return false;
        }
        // The editor's width from its last paint, plus the margin if it was shown.
        let w = self.editor.read(cx).input_bounds().size.width;
        if w <= px(0.) {
            return true; // not laid out yet: assume wide (avoids a bar flash)
        }
        let tab = w + if self.pi.margin_was_on { px(MARGIN_W) } else { px(0.) };
        tab >= px(MIN_TAB_W)
    }

    /// The active card's note: the walkthrough's current stop, if it's in this file.
    pub(crate) fn active_stop(&self, cx: &App) -> Option<Stop> {
        let agent = self.pi.agent.as_ref()?;
        let current = agent.read(cx).current_stop.clone()?;
        self.pi.stops.iter().find(|s| s.at == current).cloned()
    }

    fn send_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(agent), Some(reply), Some(stop)) =
            (self.pi.agent.clone(), self.pi.reply.clone(), self.active_stop(cx))
        else {
            return;
        };
        let text = reply.read(cx).value().to_string();
        let root = self.workspace.read(cx).root().to_path_buf();
        let sent = agent.update(cx, |a, cx| a.reply_to_stop(&stop, &root, text, cx));
        if sent {
            reply.update(cx, |r, cx| r.set_value("", window, cx));
        }
    }

    /// The margin with the cards, beside the editor.
    pub(crate) fn render_pi_margin(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let color = pide_core::colors::pi_note(cx);
        let agent = self.pi.agent.clone().expect("margin_on checked");
        let active = self.active_stop(cx);
        let editor = self.editor.read(cx);
        let rope = editor.text().clone();
        let scroll_now = editor.scroll_offset().y;

        // Where each note's first line is now (relative to the margin's top).
        let mut geom = self.pi.geom.borrow_mut();
        let (scroll_painted, top) = match geom.painted {
            Some((s, t)) => (s, Some(t)),
            None => (scroll_now, None),
        };
        let mut visible: Vec<(Stop, Pixels)> = Vec::new();
        if let Some(top) = top {
            for s in &self.pi.stops {
                let start = crate::pi_notes::line_range(&rope, s.line).start;
                if let Some(b) = editor.range_to_bounds(&(start..start)) {
                    visible.push((s.clone(), b.top() + (scroll_now - scroll_painted) - top));
                }
            }
        }
        let heights: Vec<Pixels> = visible
            .iter()
            .map(|(s, _)| *geom.heights.get(&s.at).unwrap_or(&px(GUESS_H)))
            .collect();
        let active_i = active
            .as_ref()
            .and_then(|a| visible.iter().position(|(s, _)| s.at == a.at));
        let ys = stack(
            &visible
                .iter()
                .zip(&heights)
                .map(|((_, y), h)| (f32::from(*y), f32::from(*h)))
                .collect::<Vec<_>>(),
            active_i,
            GAP,
        );
        geom.used_scroll = scroll_now;
        geom.used_top = top;
        geom.used_visible = visible.iter().map(|(s, _)| s.at.clone()).collect();
        geom.used_heights = heights;
        drop(geom);

        let a = agent.read(cx);
        let all = a.stops();
        let total = all.len();
        let busy = a.is_busy();
        let answering = a.answering.as_ref().map(|(at, _)| at.clone());
        let pins = a.pins.clone();
        let session = a.name.clone();

        let mut cards: Vec<AnyElement> = Vec::new();
        for (i, ((stop, _), y)) in visible.iter().zip(&ys).enumerate() {
            let is_active = Some(i) == active_i;
            let index = all.iter().position(|s| s.at == stop.at);
            let kind = kind_label(&stop.at, &pins);
            let card = if is_active {
                self.active_card(stop, kind, index, total, busy, answering.as_ref() == Some(&stop.at), &session, &agent, &theme, color, cx)
            } else {
                resting_card(stop, kind, &agent, &theme, color, i)
            };
            cards.push(
                div()
                    .absolute()
                    .top(px(*y))
                    .left(px(if is_active { 6. } else { 14. }))
                    .w(px(MARGIN_W - if is_active { 12. } else { 28. }))
                    .child(card)
                    .into_any_element(),
            );
        }

        // Records what the paint saw; asks for another frame if the render was stale.
        let editor_entity = self.editor.clone();
        let geom = self.pi.geom.clone();
        let geom_children = self.pi.geom.clone();
        let stops_now: Vec<(StopRef, usize)> =
            self.pi.stops.iter().map(|s| (s.at.clone(), s.line)).collect();
        let weak = cx.weak_entity();
        let recorder = canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                let editor = editor_entity.read(cx);
                let scroll = editor.scroll_offset().y;
                let rope = editor.text();
                let visible_now: Vec<StopRef> = stops_now
                    .iter()
                    .filter(|(_, line)| {
                        let start = crate::pi_notes::line_range(rope, *line).start;
                        editor.range_to_bounds(&(start..start)).is_some()
                    })
                    .map(|(at, _)| at.clone())
                    .collect();
                let mut g = geom.borrow_mut();
                g.painted = Some((scroll, bounds.top()));
                let heights_now: Vec<Pixels> = g
                    .used_visible
                    .iter()
                    .map(|at| *g.heights.get(at).unwrap_or(&px(GUESS_H)))
                    .collect();
                let stale = g.used_scroll != scroll
                    || g.used_top != Some(bounds.top())
                    || g.used_visible != visible_now
                    || g.used_heights != heights_now;
                drop(g);
                if stale {
                    let weak = weak.clone();
                    window.on_next_frame(move |_, cx| {
                        let _ = weak.update(cx, |_, cx| cx.notify());
                    });
                }
            },
        )
        .absolute()
        .size_full();

        let order: Vec<StopRef> = visible.iter().map(|(s, _)| s.at.clone()).collect();
        let _ = window;
        div()
            .relative()
            .flex_none()
            .w(px(MARGIN_W))
            .h_full()
            .overflow_hidden()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .child(recorder)
            .children(cards)
            .on_children_prepainted(move |bounds, _, _| {
                // bounds[0] is the recorder; then one per card, in `order`
                // (connectors aren't children, so the indices line up).
                let mut g = geom_children.borrow_mut();
                for (at, b) in order.iter().zip(bounds.iter().skip(1)) {
                    g.heights.insert(at.clone(), b.size.height);
                }
            })
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn active_card(
        &self,
        stop: &Stop,
        kind: &'static str,
        index: Option<usize>,
        total: usize,
        busy: bool,
        answering: bool,
        session: &SharedString,
        agent: &Entity<Agent>,
        theme: &gpui_kit::component::theme::Theme,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (a_back, a_next, a_resolve) = (agent.clone(), agent.clone(), agent.clone());
        let at = stop.at.clone();
        let reply_empty = self
            .pi
            .reply
            .as_ref()
            .is_none_or(|r| r.read(cx).value().trim().is_empty());
        let head = h_flex()
            .w_full()
            .gap_1p5()
            .text_xs()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child("pi"),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(theme.muted_foreground)
                    .child(match index {
                        Some(i) => format!("{kind} · {}/{total} · {session}", i + 1),
                        None => format!("{kind} · {session}"),
                    }),
            )
            .child(
                Button::new("pi-card-back")
                    .xsmall()
                    .ghost()
                    .label("Back")
                    .on_click(move |_, window, cx| {
                        pide_core::collab::walk(&a_back, -1, window, cx);
                    }),
            )
            .child(
                Button::new("pi-card-next")
                    .xsmall()
                    .ghost()
                    .label("Next")
                    .on_click(move |_, window, cx| {
                        pide_core::collab::walk(&a_next, 1, window, cx);
                    }),
            );
        let thread = stop.thread.iter().map(|r| {
            v_flex()
                .w_full()
                .pt_1p5()
                .mt_1p5()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if r.pi { color } else { theme.foreground })
                        .child(if r.pi { "pi" } else { "You" }),
                )
                .child(div().w_full().child(r.text.clone()))
        });
        let body = v_flex()
            .id("pi-card-body")
            .w_full()
            .max_h(px(420.))
            .overflow_y_scroll()
            .child(div().w_full().child(stop.label()))
            .when(stop.outdated, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("outdated: the line it was about changed"),
                )
            })
            .children(thread)
            .when(answering && busy, |d| {
                d.child(
                    div()
                        .pt_1p5()
                        .mt_1p5()
                        .border_t_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("pi is answering…"),
                )
            });
        let send_disabled = busy || reply_empty;
        let reply = self.pi.reply.clone();
        let actions = h_flex()
            .w_full()
            .gap_1()
            .mt_1p5()
            .child(div().flex_1().when(busy, |d| {
                d.text_xs()
                    .text_color(theme.muted_foreground)
                    .child("pi is busy")
            }))
            .child(
                Button::new("pi-card-resolve")
                    .xsmall()
                    .ghost()
                    .label("Resolve")
                    .tooltip("Hide this note")
                    .on_click(move |_, _, cx| {
                        a_resolve.update(cx, |a, cx| a.dismiss_stop(&at, cx));
                    }),
            )
            .child(
                Button::new("pi-card-send")
                    .xsmall()
                    .primary()
                    .label("Send")
                    .disabled(send_disabled)
                    .on_click(cx.listener(|this, _, window, cx| this.send_reply(window, cx))),
            );
        v_flex()
            .w_full()
            .p_2()
            .gap_1()
            .text_sm()
            .rounded_md()
            .border_1()
            .border_color(color)
            .bg(theme.background)
            .shadow_md()
            .child(head)
            .child(body)
            .when_some(reply, |d, r| d.child(div().mt_1().child(Textarea::new(&r))))
            .child(actions)
            .into_any_element()
    }
}

/// A card at rest: who, what kind, two lines of the note, the reply count.
fn resting_card(
    stop: &Stop,
    kind: &'static str,
    agent: &Entity<Agent>,
    theme: &gpui_kit::component::theme::Theme,
    color: Hsla,
    i: usize,
) -> AnyElement {
    let agent = agent.clone();
    let open = stop.clone();
    let replies = stop.thread.len();
    v_flex()
        .id(("pi-card", i))
        .w_full()
        .p_2()
        .gap_0p5()
        .text_sm()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .cursor_pointer()
        .hover(|s| s.border_color(color.opacity(0.6)))
        .on_click(move |_, window, cx| {
            pide_core::collab::go_to(&agent, open.clone(), window, cx);
        })
        .child(
            h_flex()
                .w_full()
                .gap_1p5()
                .text_xs()
                .when(!stop.seen, |d| {
                    d.child(div().size(px(6.)).rounded_full().bg(color))
                })
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(color)
                        .child("pi"),
                )
                .child(div().flex_1().text_color(theme.muted_foreground).child(kind))
                .when(replies > 0, |d| {
                    d.child(div().text_color(theme.muted_foreground).child(if replies == 1 {
                        "1 reply".to_string()
                    } else {
                        format!("{replies} replies")
                    }))
                }),
        )
        .child(
            div()
                .w_full()
                .line_clamp(2)
                .text_color(if stop.outdated {
                    theme.muted_foreground
                } else {
                    theme.foreground
                })
                .child(stop.label()),
        )
        .into_any_element()
}

fn kind_label(at: &StopRef, pins: &[pide_core::collab::Pin]) -> &'static str {
    match at {
        StopRef::Pin(i) if pins.get(*i).is_some_and(pide_core::walk::is_tour) => "tour stop",
        StopRef::Pin(_) => "pin",
        StopRef::Hunk { .. } => "change note",
        StopRef::File(_) => "changed file",
    }
}

/// Card tops for cards wanting to sit at `want[i] = (y, height)` (sorted by y),
/// without overlapping: the active card stays at its y, cards below it are
/// pushed down, cards above it pushed up; with no active card, top to bottom.
pub(crate) fn stack(want: &[(f32, f32)], active: Option<usize>, gap: f32) -> Vec<f32> {
    let mut ys: Vec<f32> = want.iter().map(|(y, _)| *y).collect();
    let start = active.unwrap_or(0);
    if want.is_empty() {
        return ys;
    }
    for i in start + 1..want.len() {
        let below = ys[i - 1] + want[i - 1].1 + gap;
        ys[i] = ys[i].max(below);
    }
    for i in (0..start).rev() {
        let above = ys[i + 1] - gap - want[i].1;
        ys[i] = ys[i].min(above);
    }
    ys
}

#[cfg(test)]
mod tests {
    use super::stack;

    #[::core::prelude::v1::test]
    fn cards_push_apart_around_the_active_one() {
        // Three cards 50 high wanting y 0, 10, 20; the middle one is active.
        let ys = stack(&[(0., 50.), (10., 50.), (20., 50.)], Some(1), 6.);
        assert_eq!(ys, vec![-46., 10., 66.]);
        // No active card: top to bottom.
        let ys = stack(&[(0., 50.), (10., 50.), (200., 50.)], None, 6.);
        assert_eq!(ys, vec![0., 56., 200.]);
        assert!(stack(&[], Some(0), 6.).is_empty());
    }
}

// Rc/RefCell are shared between the render and the paint callbacks.
pub(crate) type Geom = Rc<RefCell<MarginGeom>>;
