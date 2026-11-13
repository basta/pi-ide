//! Tours (collaborator mode): "how does X work?" answered as an ordered walk
//! through the code across files (`ide_tour`), in a session of its own. The
//! tour's stops are pins with ids `tour:<call id>:<n>`, so they walk in the note
//! bar like any other; the chat shows one card listing them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};

use super::*;

/// What pi is asked, around the user's question.
pub(crate) fn tour_prompt(question: &str) -> String {
    format!(
        "Give me a guided tour: {question}\n\nI don't write this code by hand, but I want to \
         understand it. Don't change anything. Read what you need, then call ide_tour once with \
         4 to 12 stops in the order I should read them (following the flow across files), each \
         with a short note on what happens there and how it leads to the next. Then reply with a \
         short overview of the whole thing."
    )
}

/// Agent: Take a Tour… — ask the question, start a session for it.
pub(crate) fn open(sessions: Entity<Sessions>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("e.g. how does a pi session resume after a restart?")
    });
    let focus_once = Rc::new(Cell::new(true));
    let run = Rc::new({
        let input = input.clone();
        move |window: &mut Window, cx: &mut App| {
            let question = input.read(cx).value().trim().to_string();
            if question.is_empty() {
                return;
            }
            let sessions = sessions.clone();
            window.defer(cx, move |window, cx| {
                let name = pide_core::agent::name_from_prompt(&format!("Tour: {question}"));
                let agent = sessions.update(cx, |s, cx| s.add(None, Some(name), cx));
                agent.update(cx, |a, cx| a.prompt(tour_prompt(&question), cx));
                CommandRegistry::run("agent.ask", window, cx);
            });
        }
    });
    let enter = run.clone();
    let sub = window.subscribe(&input, cx, move |_, ev: &InputEvent, window, cx| {
        if let InputEvent::PressEnter { .. } = ev {
            window.close_dialog(cx);
            enter(window, cx);
        }
    });
    let sub = Rc::new(RefCell::new(Some(sub)));
    window.open_dialog(cx, move |dialog, window, cx| {
        let (input, run, sub) = (input.clone(), run.clone(), sub.clone());
        if focus_once.replace(false) {
            let input = input.clone();
            window.defer(cx, move |window, cx| input.update(cx, |i, cx| i.focus(window, cx)));
        }
        dialog
            .text_sm()
            .title("Take a tour")
            .w(px(520.))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .pb_2()
                    .child("Ask how something works. pi answers with a walk through the code, stop by stop, without changing it."),
            )
            .child(Input::new(&input).large())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("tour-cancel").label("Cancel").outline()))
                    .child(DialogAction::new().child(Button::new("tour-ok").label("Start tour").primary())),
            )
            .on_ok(move |_, window, cx| {
                run(window, cx);
                true
            })
            .on_close(move |_, _, _| {
                sub.borrow_mut().take();
            })
    });
}

impl AgentChat {
    /// A tour's card: its stops in order (click one to go there) and Start.
    pub(crate) fn render_tour(&self, call_id: &str, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let agent = self.active(cx);
        let prefix = format!("{}{call_id}:", pide_core::collab::TOUR_PREFIX);
        let stops: Vec<pide_core::collab::Stop> = {
            let a = agent.read(cx);
            a.pins
                .iter()
                .filter(|p| p.id.starts_with(&prefix))
                .filter_map(|p| a.pin_stop_by_id(&p.id))
                .collect()
        };
        if stops.is_empty() {
            return div().into_any_element();
        }
        let root = self.workspace.read(cx).root().to_path_buf();
        let seen = stops.iter().filter(|s| s.seen).count();
        let total = stops.len();
        // "Title · 1/8: note" → title, and each stop's own note.
        let title = match pide_core::collab::split_tour_note(&stops[0].note).0 {
            "" => "Tour".to_string(),
            t => t.to_string(),
        };
        let first = stops.iter().find(|s| !s.seen).unwrap_or(&stops[0]).clone();
        let agent_start = agent.clone();
        v_flex()
            .gap_1()
            .px_2()
            .py_1p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("TOUR"))
                    .child(div().text_color(theme.muted_foreground).child(format!("{seen}/{total} seen")))
                    .child(div().flex_1())
                    .child(
                        Button::new(("tour-start", ix))
                            .xsmall()
                            .label(if seen == 0 { "Start tour" } else if seen < total { "Continue" } else { "Again" })
                            .on_click(move |_, window, cx| {
                                pide_core::collab::go_to(&agent_start, first.clone(), window, cx)
                            }),
                    ),
            )
            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
            .children(stops.into_iter().enumerate().map(|(i, st)| {
                let note = pide_core::collab::split_tour_note(&st.note).1.to_string();
                let location = st.location(&root);
                let agent = agent.clone();
                let seen = st.seen;
                h_flex()
                    .id(("tour-stop", ix * 100 + i))
                    .gap_2()
                    .items_start()
                    .text_sm()
                    .cursor_pointer()
                    .rounded(theme.radius)
                    .hover(|s| s.bg(theme.list_hover))
                    .on_click(move |_, window, cx| pide_core::collab::go_to(&agent, st.clone(), window, cx))
                    .child(
                        div()
                            .flex_none()
                            .w(px(18.))
                            .text_xs()
                            .text_color(if seen { theme.muted_foreground } else { pide_core::colors::pi_note(cx) })
                            .child(format!("{}", i + 1)),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(pide_core::colors::accent(cx))
                                    .child(location),
                            )
                            .child(div().child(note)),
                    )
            }))
            .into_any_element()
    }
}
