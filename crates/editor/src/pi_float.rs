//! pi's notes floating over the code (docs/COLLAB.md, "Notes in the code"),
//! taking no width from it. At rest each note is one dim line of text after the
//! code on its first line (a "lens": "◆ pi <the note's first words> · 2 replies"),
//! or a small "◆ pi" pill at the editor's right edge when the line is too long
//! for the text. The walkthrough's current note opens as a card just below its
//! lines (above them if there's no room), with the whole note, its thread and a
//! reply box; ✕ or Esc collapses it, clicking a lens opens it again.
//!
//! GPUI Kit can't make room between lines, so the open card covers the code
//! under it. Positions come from `EditorState::range_to_bounds`, which reports
//! where the editor drew a range at its last paint: we correct by how far it
//! scrolled since, and ask for one more frame whenever what we drew from turns
//! out stale (a line came into view, the card's height changed).

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pide_core::collab::{Stop, StopRef};
use pide_core::Agent;

use crate::EditorPanel;

/// The open card's width.
const CARD_W: f32 = 440.;
/// A lens needs at least this much room after its line, else it's a pill.
const MIN_LENS_W: f32 = 140.;
/// Space between a line's end and its lens.
const LENS_GAP: f32 = 24.;
/// Kept clear of the editor's right edge (its scrollbar).
const RIGHT_PAD: f32 = 18.;
/// The open card's height before it has been laid out once.
const GUESS_H: f32 = 220.;

/// What the overlay drew from, and what the last paint saw.
#[derive(Default)]
pub(crate) struct FloatGeom {
    /// At the last paint: the editor's vertical scroll offset and the overlay's bounds.
    painted: Option<(Pixels, Bounds<Pixels>)>,
    /// The open card's laid-out height.
    card_h: Option<Pixels>,
    /// What the last render used: the scroll offset, the overlay's bounds,
    /// which notes were on screen, and the card height.
    used_scroll: Pixels,
    used_bounds: Option<Bounds<Pixels>>,
    used_visible: Vec<StopRef>,
    used_card_h: Option<Pixels>,
    /// The render drew a card (its bounds come last among the overlay's children).
    has_card: bool,
}

pub(crate) type Geom = Rc<RefCell<FloatGeom>>;

impl EditorPanel {
    /// The reply box (made once, with the tab).
    pub(crate) fn make_reply_box(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reply = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .placeholder(format!(
                    "Reply to pi… ({} sends)",
                    pide_core::keymap::label("secondary-enter")
                ))
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

    /// The note whose card is open: the walkthrough's current stop in this
    /// file, unless you collapsed it.
    pub(crate) fn open_stop(&self, cx: &App) -> Option<Stop> {
        self.active_stop(cx)
            .filter(|s| self.pi.collapsed.as_ref() != Some(&s.at))
    }

    /// ✕ / Esc: fold the open card back into its lens.
    pub(crate) fn collapse_pi_card(&mut self, cx: &mut Context<Self>) -> bool {
        match self.open_stop(cx) {
            Some(s) => {
                self.pi.collapsed = Some(s.at);
                cx.notify();
                true
            }
            None => false,
        }
    }

    /// The notes over the code: lenses or pills, and the open card.
    pub(crate) fn render_pi_float(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let agent = self.pi.agent.clone()?;
        if self.pi.stops.is_empty() {
            return None;
        }
        let color = pide_core::colors::pi_note(cx);
        let open = self.open_stop(cx);
        let editor = self.editor.read(cx);
        let rope = editor.text().clone();
        let scroll_now = editor.scroll_offset().y;
        let view = editor.input_bounds();

        let mut geom = self.pi.geom.borrow_mut();
        let painted = geom.painted;
        let card_h = geom.card_h.unwrap_or(px(GUESS_H));
        geom.used_scroll = scroll_now;
        geom.used_bounds = painted.map(|(_, b)| b);
        geom.used_card_h = geom.card_h;
        drop(geom);
        let mut visible: Vec<StopRef> = Vec::new();
        let mut lenses: Vec<AnyElement> = Vec::new();
        let mut card: Option<AnyElement> = None;
        let mut card_at = None;

        if let Some((scroll_painted, ob)) = painted {
            let dy = scroll_now - scroll_painted;
            let a = agent.read(cx);
            let all = a.stops();
            let total = all.len();
            let busy = a.is_busy();
            let answering = a.answering.as_ref().map(|(at, _)| at.clone());
            let pins = a.pins.clone();
            let session = a.name.clone();
            for (i, s) in self.pi.stops.iter().enumerate() {
                let Some(start) = line_bounds(editor, &rope, s.line) else {
                    continue;
                };
                visible.push(s.at.clone());
                let is_open = open.as_ref().is_some_and(|o| o.at == s.at);
                let top = start.top() + dy - ob.top();
                // The lens after the line's text, or a pill at the right edge.
                let end = crate::pi_notes::line_range(&rope, s.line).end;
                let line_end = editor
                    .range_to_bounds(&(end..end))
                    .map(|b| b.left())
                    .unwrap_or(start.left());
                let x = line_end + px(LENS_GAP) - ob.left();
                let room = view.right() - px(RIGHT_PAD) - (line_end + px(LENS_GAP));
                let replies = s.thread.len();
                let stop = s.clone();
                let agent_c = agent.clone();
                let el = div()
                    .id(("pi-lens", i))
                    .absolute()
                    .top(top)
                    .h(start.size.height)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_xs()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle_pi_card(&agent_c, stop.clone(), window, cx)
                    }));
                let el = if room >= px(MIN_LENS_W) {
                    el.left(x)
                        .max_w(room)
                        .overflow_hidden()
                        .text_color(color.opacity(if is_open { 0.95 } else { 0.7 }))
                        .hover(|st| st.text_color(color))
                        .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child("◆ pi"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(lens_text(&note_body(&s.label()))),
                        )
                        .when(replies > 0, |d| {
                            d.child(div().flex_none().child(if replies == 1 {
                                "· 1 reply".to_string()
                            } else {
                                format!("· {replies} replies")
                            }))
                        })
                } else {
                    el.right(px(RIGHT_PAD))
                        .px_2()
                        .rounded_full()
                        .border_1()
                        .border_color(color.opacity(0.5))
                        .bg(if is_open { color } else { color.opacity(0.18) })
                        .text_color(if is_open { cx.theme().background } else { color })
                        .child(if replies > 0 {
                            format!("◆ pi · {replies}")
                        } else {
                            "◆ pi".to_string()
                        })
                };
                lenses.push(el.into_any_element());

                if is_open {
                    // Below the note's last line; above its first if that doesn't fit.
                    let last = line_bounds(editor, &rope, s.end_line).unwrap_or(start);
                    // On line boundaries, so no line shows half above the card.
                    let below = last.bottom() + dy - ob.top();
                    let above = top - card_h;
                    let y = if below + card_h <= ob.size.height || above < px(0.) {
                        below
                    } else {
                        above
                    };
                    let code_x = first_text_x(editor, &rope, s.line).unwrap_or(start.left());
                    let x = (code_x - ob.left())
                        .min(ob.size.width - px(CARD_W + RIGHT_PAD))
                        .max(px(8.));
                    let index = all.iter().position(|x| x.at == s.at);
                    let kind = kind_label(&s.at, &pins);
                    let answering = answering.as_ref() == Some(&s.at);
                    card_at = Some((s.clone(), x, y, kind, index, answering));
                }
            }
            if let Some((s, x, y, kind, index, answering)) = card_at {
                let theme = cx.theme().clone();
                let el = self.active_card(
                    &s, kind, index, total, busy, answering, &session, &agent, &theme, color, cx,
                );
                card = Some(
                    div()
                        .absolute()
                        .left(x)
                        .top(y)
                        .w(px(CARD_W))
                        .child(el)
                        .into_any_element(),
                );
            }
        }
        let has_card = card.is_some();
        {
            let mut g = self.pi.geom.borrow_mut();
            g.used_visible = visible;
            g.has_card = has_card;
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
                    .filter(|(_, line)| line_bounds(editor, rope, *line).is_some())
                    .map(|(at, _)| at.clone())
                    .collect();
                let mut g = geom.borrow_mut();
                g.painted = Some((scroll, bounds));
                let stale = g.used_scroll != scroll
                    || g.used_bounds != Some(bounds)
                    || g.used_visible != visible_now
                    || (g.has_card && g.used_card_h != g.card_h);
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

        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(recorder)
                .children(lenses)
                .children(card)
                .on_children_prepainted(move |bounds, _, _| {
                    let mut g = geom_children.borrow_mut();
                    if g.has_card
                        && let Some(b) = bounds.last()
                    {
                        g.card_h = Some(b.size.height);
                    }
                })
                .into_any_element(),
        )
    }

    /// A lens or pill was clicked: open its card (making it the walkthrough's
    /// current stop), or collapse it if it's the open one.
    fn toggle_pi_card(
        &mut self,
        agent: &Entity<Agent>,
        stop: Stop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open_stop(cx).is_some_and(|o| o.at == stop.at) {
            self.collapse_pi_card(cx);
            return;
        }
        self.pi.collapsed = None;
        let agent = agent.clone();
        // go_to opens the file (this tab) and moves the cursor: not inside our update.
        window.defer(cx, move |window, cx| {
            pide_core::collab::go_to(&agent, stop, window, cx);
        });
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn active_card(
        &mut self,
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
        let label = stop.label();
        let tour = pide_core::collab::parse_tour_note(&label);
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
                    .child(match tour {
                        // A tour stop: the tour's title and the stop's own count on top.
                        Some((title, i, n, _)) => format!("{title} · {i}/{n}"),
                        None => match index {
                            Some(i) => format!("{kind} · {}/{total} · {session}", i + 1),
                            None => format!("{kind} · {session}"),
                        },
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
            )
            .child(
                Button::new("pi-card-close")
                    .xsmall()
                    .ghost()
                    .label("✕")
                    .tooltip("Collapse (Esc)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.collapse_pi_card(cx);
                    })),
            );
        let mut md = |text: &str, cx: &mut Context<Self>| -> AnyElement {
            let view = self
                .pi
                .markdown
                .entry(text.to_string())
                .or_insert_with(|| cx.new(|cx| TextViewState::markdown(&plain_numbers(text), cx)))
                .clone();
            TextView::new(&view).selectable(true).into_any_element()
        };
        let note = md(&note_body(&label), cx);
        let replies: Vec<(bool, AnyElement)> = stop
            .thread
            .iter()
            .map(|r| {
                let body = if r.pi {
                    md(&r.text, cx)
                } else {
                    div().w_full().child(r.text.clone()).into_any_element()
                };
                (r.pi, body)
            })
            .collect();
        let theme = cx.theme().clone();
        let thread = replies.into_iter().map(|(pi, body)| {
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
                        .text_color(if pi { color } else { theme.foreground })
                        .child(if pi { "pi" } else { "You" }),
                )
                .child(body)
        });
        let body = v_flex()
            .id("pi-card-body")
            .w_full()
            .max_h(px(260.))
            .overflow_y_scrollbar()
            .child(note)
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
            .bg(theme.popover)
            .shadow_lg()
            .child(head)
            .child(body)
            .when_some(reply, |d, r| d.child(div().mt_1().child(Textarea::new(&r))))
            .child(actions)
            .into_any_element()
    }
}

/// A note without the "<title> · i/n: " a tour stop's note starts with.
fn note_body(label: &str) -> String {
    match pide_core::collab::parse_tour_note(label) {
        Some((_, _, _, note)) => note.to_string(),
        None => label.to_string(),
    }
}

/// The lens's text: the note's first line.
fn lens_text(label: &str) -> String {
    let first = label.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    first.trim().chars().take(200).collect()
}

/// Where the first non-blank character of 1-based `line` was drawn (its x).
fn first_text_x(
    editor: &gpui_kit::component::input::EditorState,
    rope: &gpui_kit::component::input::Rope,
    line: usize,
) -> Option<Pixels> {
    let r = crate::pi_notes::line_range(rope, line);
    let text = rope.slice(r.clone()).to_string();
    let indent = text.len() - text.trim_start().len();
    let at = r.start + indent;
    editor.range_to_bounds(&(at..at)).map(|b| b.left())
}

/// pi writes "1) …" as plain text; markdown would make it a numbered list.
fn plain_numbers(text: &str) -> String {
    text.lines()
        .map(|l| {
            let t = l.trim_start();
            let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 && t[digits..].starts_with(')') {
                let indent = &l[..l.len() - t.len()];
                format!("{indent}{}\\){}", &t[..digits], &t[digits + 1..])
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where 1-based `line` was drawn at the editor's last paint, if it was on
/// screen. `range_to_bounds` alone isn't enough: for a line above the laid-out
/// rows it reports the first laid-out line instead of `None`.
fn line_bounds(
    editor: &gpui_kit::component::input::EditorState,
    rope: &gpui_kit::component::input::Rope,
    line: usize,
) -> Option<Bounds<Pixels>> {
    let rows = editor.visible_row_range()?;
    if !rows.contains(&line.saturating_sub(1)) {
        return None;
    }
    let start = crate::pi_notes::line_range(rope, line).start;
    let b = editor.range_to_bounds(&(start..start))?;
    let view = editor.input_bounds();
    (b.bottom() > view.top() && b.top() < view.bottom()).then_some(b)
}

fn kind_label(at: &StopRef, pins: &[pide_core::collab::Pin]) -> &'static str {
    match at {
        StopRef::Pin(i) if pins.get(*i).is_some_and(pide_core::walk::is_tour) => "tour stop",
        StopRef::Pin(_) => "pin",
        StopRef::Hunk { .. } => "change note",
        StopRef::File(_) => "changed file",
    }
}

#[cfg(test)]
mod tests {
    use super::{lens_text, note_body, plain_numbers};

    #[::core::prelude::v1::test]
    fn numbered_parens_stay_plain() {
        assert_eq!(plain_numbers("1) a\n  12) b\nx 3) c"), "1\\) a\n  12\\) b\nx 3) c");
    }

    #[::core::prelude::v1::test]
    fn a_tour_note_loses_its_heading() {
        assert_eq!(note_body("Story · 2/5: The cache is filled here."), "The cache is filled here.");
        assert_eq!(note_body("Plain note: with a colon"), "Plain note: with a colon");
    }

    #[::core::prelude::v1::test]
    fn the_lens_shows_the_first_line() {
        assert_eq!(lens_text("\n  Core write path.\nMore."), "Core write path.");
    }
}
