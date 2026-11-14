//! Extension dialog cards (select, confirm, input, editor).

use super::*;

impl AgentChat {
    /// A text field for an `input` (Enter submits) or `editor` (Cmd+Enter submits) dialog.
    pub(crate) fn make_dialog_input(
        &mut self,
        agent: Entity<Agent>,
        req: DialogRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = req.kind == DialogKind::Editor;
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state = if editor {
                state.auto_grow(4, 14)
            } else {
                state.auto_grow(1, 3).submit_on_enter(true)
            };
            if let Some(p) = &req.placeholder {
                state = state.placeholder(p.clone());
            }
            if let Some(text) = &req.prefill {
                state = state.default_value(text.clone());
            }
            state
        });
        let id = req.id.clone();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, input, ev: &InputEvent, _, cx| {
                let submit = match ev {
                    InputEvent::PressEnter {
                        secondary,
                        shift: false,
                    } => !editor || *secondary,
                    _ => false,
                };
                if submit {
                    let value = input.read(cx).value().to_string();
                    this.answer(&agent, &id, DialogAnswer::Value(value), cx);
                }
            },
        );
        let focus = input.focus_handle(cx);
        self.dialog_inputs.insert(req.id, (input, sub));
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }

    pub(crate) fn answer(
        &mut self,
        agent: &Entity<Agent>,
        id: &str,
        answer: DialogAnswer,
        cx: &mut Context<Self>,
    ) {
        agent.update(cx, |a, cx| a.answer_dialog(id, answer, cx));
        self.dialog_inputs.remove(id);
        cx.notify();
    }

    pub(crate) fn render_dialog(
        &self,
        agent: &Entity<Agent>,
        d: &DialogRequest,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let btn = |label: &'static str,
                   id: &str,
                   answer: DialogAnswer,
                   primary: bool,
                   cx: &mut Context<Self>| {
            let (agent, id) = (agent.clone(), id.to_string());
            let b = Button::new(SharedString::from(format!("dlg-{id}-{label}")))
                .small()
                .label(label);
            let b = if primary { b.primary() } else { b };
            b.on_click(
                cx.listener(move |this, _, _, cx| this.answer(&agent, &id, answer.clone(), cx)),
            )
        };
        let mut card = v_flex()
            .gap_2()
            .p_3()
            .m_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.warning)
            .bg(theme.secondary)
            .text_sm()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.warning)
                    .child("PI IS WAITING FOR YOU"),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(d.title.clone()),
            )
            .when_some(d.message.clone(), |this, m| {
                this.child(div().text_color(theme.muted_foreground).child(m))
            });
        match d.kind {
            DialogKind::Confirm => {
                card = card.child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(btn("Cancel", &d.id, DialogAnswer::Cancelled, false, cx))
                        .child(btn(
                            "Deny",
                            &d.id,
                            DialogAnswer::Confirmed(false),
                            false,
                            cx,
                        ))
                        .child(btn("Allow", &d.id, DialogAnswer::Confirmed(true), true, cx)),
                );
            }
            DialogKind::Select => {
                let options: Vec<AnyElement> = d
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| {
                        let (agent, id, value) = (agent.clone(), d.id.clone(), o.clone());
                        Button::new(SharedString::from(format!("dlg-{}-opt-{i}", d.id)))
                            .small()
                            .w_full()
                            .label(o.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer(&agent, &id, DialogAnswer::Value(value.clone()), cx)
                            }))
                            .into_any_element()
                    })
                    .collect();
                card = card.child(v_flex().gap_1().children(options)).child(
                    h_flex().justify_end().child(btn(
                        "Cancel",
                        &d.id,
                        DialogAnswer::Cancelled,
                        false,
                        cx,
                    )),
                );
            }
            DialogKind::Input | DialogKind::Editor => {
                let hint = if d.kind == DialogKind::Editor {
                    "⌘Enter to submit"
                } else {
                    "Enter to submit"
                };
                if let Some((input, _)) = self.dialog_inputs.get(&d.id) {
                    card = card.child(Textarea::new(input));
                }
                let (agent2, id2) = (agent.clone(), d.id.clone());
                card = card.child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(hint),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(btn("Cancel", &d.id, DialogAnswer::Cancelled, false, cx))
                                .child(
                                    Button::new(SharedString::from(format!("dlg-{}-ok", d.id)))
                                        .small()
                                        .primary()
                                        .label("Submit")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let value = this
                                                .dialog_inputs
                                                .get(&id2)
                                                .map(|(i, _)| i.read(cx).value().to_string())
                                                .unwrap_or_default();
                                            this.answer(
                                                &agent2,
                                                &id2,
                                                DialogAnswer::Value(value),
                                                cx,
                                            )
                                        })),
                                ),
                        ),
                );
            }
        }
        card.into_any_element()
    }
}
