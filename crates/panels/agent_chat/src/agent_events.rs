//! Applying a session's agent events to its conversation.

use super::*;

impl AgentChat {
    pub(crate) fn on_agent_event(
        &mut self,
        agent: Entity<Agent>,
        ev: &AgentEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_active = agent == self.active(cx);
        let name = agent.read(cx).name.clone();
        if let AgentEvent::Dialog(req) = ev {
            if !is_active {
                window.push_notification(
                    Notification::warning(format!(
                        "pi in \u{201c}{name}\u{201d} is waiting for your answer."
                    )),
                    cx,
                );
            }
            Self::desktop_notify(window, cx, "pi is asking", format!("{name}: {}", req.title));
            if matches!(req.kind, DialogKind::Input | DialogKind::Editor) {
                self.make_dialog_input(agent, req.clone(), window, cx);
            }
            cx.notify();
            return;
        }
        let image_note = matches!(ev, AgentEvent::UserPrompt { .. })
            .then(|| self.image_notes.remove(&agent.entity_id()))
            .flatten();
        let Some(view) = self.views.get_mut(&agent.entity_id()) else {
            return;
        };
        let items = &mut view.items;
        match ev {
            AgentEvent::UserPrompt { text } => {
                let (chip, text) = pide_core::context::split_prompt(text);
                items.push(Item::User(chip, text.to_string()));
                if let Some(n) = image_note {
                    items.push(Item::Notice {
                        text: format!("With {n} image{}.", if n == 1 { "" } else { "s" }),
                        error: false,
                    });
                }
            }
            AgentEvent::AssistantStart => {
                let markdown = cx.new(|cx| TextViewState::markdown("", cx));
                items.push(Item::Assistant {
                    markdown,
                    text: String::new(),
                    last_index: None,
                    thinking: String::new(),
                    thinking_open: false,
                    error: None,
                });
            }
            AgentEvent::TextDelta { index, delta } => {
                if let Some(Item::Assistant {
                    markdown,
                    text,
                    last_index,
                    ..
                }) = items.last_mut()
                {
                    let mut chunk = String::new();
                    if last_index.is_some_and(|i| i != *index) && !text.is_empty() {
                        chunk.push_str("\n\n");
                    }
                    *last_index = Some(*index);
                    chunk.push_str(delta);
                    text.push_str(&chunk);
                    markdown.update(cx, |m, cx| m.push_str(&chunk, cx));
                }
            }
            AgentEvent::TextEnd { content, .. } => {
                if let Some(Item::Assistant {
                    markdown,
                    text,
                    last_index,
                    ..
                }) = items.last_mut()
                    && last_index.is_some()
                    && !text.ends_with(content.as_str())
                    && !content.is_empty()
                {
                    *text = content.clone();
                    markdown.update(cx, |m, cx| m.set_text(content, cx));
                }
            }
            AgentEvent::ThinkingDelta { delta, .. } => {
                if let Some(Item::Assistant { thinking, .. }) = items.last_mut() {
                    thinking.push_str(delta);
                }
            }
            AgentEvent::AssistantEnd { error } => {
                if let Some(Item::Assistant { error: e, .. }) = items.last_mut() {
                    *e = error.clone();
                }
            }
            // pide's own tools: their results show as pin cards / the checklist / Changes.
            // (ide_diagnostics stays a tool card: what pi saw is worth seeing.)
            AgentEvent::ToolStart { name, .. }
                if name.starts_with("ide_") && name != "ide_diagnostics" => {}
            AgentEvent::Pin(pin) => match pin.id.strip_prefix(pide_core::collab::TOUR_PREFIX) {
                // A tour's stops: one card for the whole tour.
                Some(rest) => {
                    let call = rest.rsplit_once(':').map_or(rest, |(c, _)| c).to_string();
                    if !items.iter().any(|i| matches!(i, Item::Tour(c) if *c == call)) {
                        items.push(Item::Tour(call.clone()));
                        // An "Update tour" answer: hide the tour it replaces.
                        if let Some(old) = self.replacing_tour.get(&agent.entity_id()).cloned()
                            && old != call
                        {
                            self.replacing_tour.remove(&agent.entity_id());
                            let agent = agent.clone();
                            cx.defer(move |cx| agent.update(cx, |a, cx| a.dismiss_tour(&old, cx)));
                        }
                    }
                }
                None => items.push(Item::Pin(pin.id.clone())),
            },
            AgentEvent::ExplainChange { path, .. } => {
                if !items
                    .iter()
                    .any(|i| matches!(i, Item::Explained(p) if p == path))
                {
                    items.push(Item::Explained(path.clone()));
                }
            }
            AgentEvent::ToolStart { id, name, args } => items.push(Item::Tool {
                id: id.clone(),
                name: name.clone(),
                summary: summarize_args(name, args),
                state: ToolState::Running,
                output: String::new(),
                duration_ms: None,
            }),
            AgentEvent::ToolUpdate { id, partial } => {
                if let Some(Item::Tool { output, .. }) = tool_mut(items, id) {
                    *output = result_text(partial);
                }
            }
            AgentEvent::ToolEnd {
                id,
                result,
                is_error,
                duration_ms,
                ..
            } => {
                if let Some(Item::Tool {
                    state,
                    output,
                    duration_ms: d,
                    ..
                }) = tool_mut(items, id)
                {
                    *state = if *is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Done
                    };
                    *output = result_text(result);
                    *d = *duration_ms;
                }
            }
            AgentEvent::Settled { aborted } => {
                if *aborted {
                    items.push(Item::Notice {
                        text: "Stopped.".into(),
                        error: false,
                    });
                }
                if !is_active {
                    window.push_notification(
                        Notification::info(format!("pi finished in \u{201c}{name}\u{201d}.")),
                        cx,
                    );
                }
                if !*aborted {
                    Self::desktop_notify(window, cx, "pi finished", name.to_string());
                }
            }
            AgentEvent::Error(e) => items.push(Item::Notice {
                text: e.clone(),
                error: true,
            }),
            AgentEvent::Notify { message, error } => {
                let n = if *error {
                    Notification::error(message.clone())
                } else {
                    Notification::info(message.clone())
                };
                window.push_notification(n, cx);
            }
            AgentEvent::Unknown(v) => log::debug!("unknown agent record: {v}"),
            _ => {}
        }
        view.scroll.scroll_to_bottom();
        cx.notify();
    }
}
