//! One session's conversation: chat items, composer, queue.

use super::*;

impl AgentChat {
    pub(crate) fn render_item(
        &self,
        view: &SessionView,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        match &view.items[ix] {
            Item::User(chip, text) => {
                // A reply to a note: your words, under a small "↪ where · note".
                let (reply_to, text) = match pide_core::collab::split_reply(text) {
                    Some((loc, words)) => (Some(loc.to_string()), words.to_string()),
                    None => (None, text.clone()),
                };
                v_flex()
                .gap_1()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .bg(theme.secondary)
                .when_some(reply_to, |this, loc| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("↪ {loc} · reply to a note")),
                    )
                })
                .when_some(chip.clone(), |this, c| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .font_family(theme.mono_font_family.clone())
                            .child(format!("⌁ {c}")),
                    )
                })
                .child(text)
                .into_any_element()
            }
            Item::Assistant {
                markdown,
                thinking,
                thinking_open,
                error,
                ..
            } => v_flex()
                .gap_1()
                .when(!thinking.is_empty(), |this| {
                    let open = *thinking_open;
                    let agent_id = self.active(cx).entity_id();
                    this.child(
                        div()
                            .id(("thinking", ix))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(Item::Assistant { thinking_open, .. }) = this
                                    .views
                                    .get_mut(&agent_id)
                                    .and_then(|v| v.items.get_mut(ix))
                                {
                                    *thinking_open = !*thinking_open;
                                    cx.notify();
                                }
                            }))
                            .child(if open {
                                format!("▾ Thinking\n{}", thinking.trim())
                            } else {
                                format!(
                                    "▸ Thinking ({} words)",
                                    thinking.split_whitespace().count()
                                )
                            }),
                    )
                })
                .child(
                    TextView::new(markdown)
                        .style(chat_text_style())
                        .markdown_extensions(pide_core::math::markdown_extensions())
                        .selectable(true),
                )
                .when_some(error.clone(), |this, e| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(theme.danger)
                            .child(format!("Ended: {e}")),
                    )
                })
                .into_any_element(),
            Item::Tool {
                name,
                summary,
                state,
                output,
                duration_ms,
                ..
            } => {
                let (mark, color) = match state {
                    ToolState::Running => ("…", theme.muted_foreground),
                    ToolState::Done => ("✓", theme.success),
                    ToolState::Failed => ("✗", theme.danger),
                };
                v_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_color(color).child(mark))
                            .child(div().font_weight(FontWeight::BOLD).child(name.clone()))
                            .child({
                                // A file tool's path opens the file.
                                let file =
                                    matches!(name.as_str(), "read" | "write" | "edit").then(|| {
                                        let p = std::path::Path::new(summary.as_str());
                                        if p.is_absolute() {
                                            p.to_path_buf()
                                        } else {
                                            self.workspace.read(cx).root().join(p)
                                        }
                                    });
                                div()
                                    .id(("tool-path", ix))
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .when_some(file, |d, file| {
                                        d.cursor_pointer().hover(|s| s.underline()).on_click(
                                            move |_, window, cx| {
                                                if file.is_file() {
                                                    pide_core::opener::open_file(
                                                        file.clone(),
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            },
                                        )
                                    })
                                    .child(summary.clone())
                            })
                            .when_some(*duration_ms, |this, d| {
                                this.child(
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{d} ms")),
                                )
                            }),
                    )
                    .when(!output.is_empty(), |this| {
                        this.child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(output.clone()),
                        )
                    })
                    .into_any_element()
            }
            Item::Pin(pin_id) => self.render_pin(pin_id, ix, cx),
            Item::Tour(call_id) => self.render_tour(call_id, ix, cx),
            Item::Explained(path) => {
                let agent = self.active(cx);
                let a = agent.read(cx);
                let Some(change) = a.changes.iter().find(|c| &c.path == path) else {
                    return div().into_any_element();
                };
                let root = self.workspace.read(cx).root().to_path_buf();
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                let summary = change.summary.clone().unwrap_or_default();
                let notes = change.notes.len();
                let first = a.stops().into_iter().find(|st| &st.path == path);
                h_flex()
                    .id(("explained", ix))
                    .gap_2()
                    .text_xs()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .rounded(theme.radius)
                    .px_1()
                    .child(
                        div()
                            .flex_none()
                            .text_color(pide_core::colors::accent(cx))
                            .child("✎"),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme.mono_font_family.clone())
                            .child(rel),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(theme.muted_foreground)
                            .child(summary),
                    )
                    .when(notes > 0, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{notes} note{}",
                                    if notes == 1 { "" } else { "s" }
                                )),
                        )
                    })
                    .on_click(move |_, window, cx| {
                        if let Some(st) = first.clone() {
                            pide_core::collab::go_to(&agent, st, window, cx);
                        }
                    })
                    .into_any_element()
            }
            Item::Notice { text, error } => div()
                .text_sm()
                .text_color(if *error {
                    theme.danger
                } else {
                    theme.muted_foreground
                })
                .child(text.clone())
                .into_any_element(),
        }
    }

    pub(crate) fn render_conversation(
        &mut self,
        wide: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let agent = self.active(cx);
        let a = agent.read(cx);
        let status = a.status().clone();
        let busy = a.is_busy();
        let name = a.name.clone();
        let status_text = Self::status_text(a);
        let (steering, follow_ups) = a.queue.clone();
        let dialog = a.dialogs.first().cloned();
        let n_changes = a.changes.len();
        let unreviewed = a.to_review();
        let show_changes = self.show_changes;
        let slash_rows = self.suggestions(cx);
        // `/` typed but the commands haven't arrived yet.
        let slash_rows_empty_hint = slash_rows.is_empty()
            && !a.commands_loaded
            && slash::typed(&self.input.read(cx).value()).is_some();
        let model_label = match &a.model {
            Some(m) => model_picker::short_label(m),
            None => "Model".to_string(),
        };
        // Thinking level, for models that reason.
        let thinking_label = match (&a.model, &a.thinking) {
            (Some(m), Some(t)) if m.reasoning => Some(format!("thinking: {t}")),
            _ => None,
        };
        let agent_thinking = agent.clone();
        let stats = a.stats.clone().filter(|s| s.tokens_total > 0);
        let model_tip = a.model.as_ref().map(|m| format!("{}/{}", m.provider, m.id));
        let agent_model = agent.clone();
        let id = agent.entity_id();
        let (items, scroll, empty) = match self.views.get(&id) {
            Some(view) => {
                let els: Vec<AnyElement> = tool_runs::segments(&view.items)
                    .into_iter()
                    .map(|seg| match seg {
                        tool_runs::Segment::Item(ix) => self.render_item(view, ix, cx),
                        tool_runs::Segment::Run(ixs) => self.render_run(view, &ixs, cx),
                    })
                    .collect();
                (els, view.scroll.clone(), view.items.is_empty())
            }
            None => (Vec::new(), ScrollHandle::new(), true),
        };
        let theme = cx.theme().clone();
        let agent_close = agent.clone();
        let narrow = self.width.get() < 520.;

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full() // the wide layout's row centers children vertically otherwise
            .min_h_0()
            .overflow_hidden()
            .when(wide, |this| {
                this.child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            v_flex()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(name.clone()))
                                .child(div().text_xs().text_color(theme.muted_foreground).child(status_text)),
                        )
                        .child(Button::new("close-session").small().label("Close").on_click(cx.listener(
                            move |this, _, _, cx| this.sessions.update(cx, |s, cx| s.close(&agent_close, cx)),
                        ))),
                )
            })
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Button::new("view-chat").xsmall().label("Chat").map(|b| pide_core::colors::toggle(b, !show_changes, cx)).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.show_changes = false;
                            cx.notify();
                        }),
                    ))
                    .child(
                        Button::new("view-changes")
                            .xsmall()
                            .label(match (n_changes, unreviewed) {
                                (0, _) => "Changes".to_string(),
                                (_, 0) => format!("Changes · {n_changes}"),
                                (_, u) => format!("Changes · {u} to review"),
                            })
                            .map(|b| pide_core::colors::toggle(b, show_changes, cx))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_changes = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("catch-me-up")
                            .xsmall()
                            .ghost()
                            .label("Catch me up")
                            .tooltip("A short briefing on this session: what was asked, what pi did, what needs you")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let shown = this.catch_up_open;
                                let has = this.active(cx).read(cx).catch_up.is_some();
                                if has && !shown {
                                    this.catch_up_open = true; // show the saved one
                                    cx.notify();
                                } else {
                                    this.catch_up(cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("understand-help")
                            .xsmall()
                            .ghost()
                            .label("?")
                            .map(|b| pide_core::colors::toggle(b, self.show_help, cx))
                            .tooltip("Ways to follow pi's code, and what the marks mean")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_help = !this.show_help;
                                this.show_changes = false;
                                // The help sits above the history: scroll up to it,
                                // and back to where you were when it closes.
                                let id = this.active(cx).entity_id();
                                if let Some(view) = this.views.get(&id) {
                                    if this.show_help {
                                        this.help_return = Some(view.scroll.offset());
                                        view.scroll.set_offset(point(px(0.), px(0.)));
                                    } else if let Some(o) = this.help_return.take() {
                                        view.scroll.set_offset(o);
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .when_some(stats, |this, st| {
                        // No "$0.000" (a free or local model); in a narrow panel the
                        // numbers give way so the model name (the picker) stays readable.
                        let cost = if st.cost < 0.0005 {
                            None
                        } else if st.cost < 0.01 {
                            Some(format!("${:.3}", st.cost))
                        } else {
                            Some(format!("${:.2}", st.cost))
                        };
                        let ctx = st.context_percent.map(|p| format!("{}% ctx", p.round() as i64));
                        let label = match (cost, ctx) {
                            (Some(c), Some(x)) if !narrow => format!("{c} · {x}"),
                            (Some(c), _) => c,
                            (None, Some(x)) => x,
                            (None, None) => String::new(),
                        };
                        if label.is_empty() {
                            return this;
                        }
                        let k = |n: u64| if n >= 1000 { format!("{:.1}k", n as f64 / 1000.0) } else { n.to_string() };
                        let tip = format!(
                            "Tokens: {} in, {} out, {} cache read ({} total){}",
                            k(st.tokens_input),
                            k(st.tokens_output),
                            k(st.tokens_cache_read),
                            k(st.tokens_total),
                            match (st.context_tokens, st.context_window) {
                                (Some(t), Some(w)) => format!(" · context {} of {}", k(t), k(w)),
                                _ => String::new(),
                            }
                        );
                        this.child(Button::new("session-stats").xsmall().ghost().label(label).tooltip(tip))
                    })
                    .when_some(thinking_label, |this, label| {
                        this.child(
                            Button::new("pick-thinking")
                                .xsmall()
                                .ghost()
                                .label(label)
                                .tooltip("Thinking level for this session")
                                .on_click(move |_, window, cx| {
                                    let agent = agent_thinking.clone();
                                    agent.update(cx, |a, cx| a.request_thinking_levels(cx));
                                    model_picker::open_thinking(agent, window, cx);
                                }),
                        )
                    })
                    .child(
                        Button::new("pick-model")
                            .xsmall()
                            .ghost()
                            .flex_none()
                            .label(model_label)
                            .when_some(model_tip, |b, t| b.tooltip(t))
                            .on_click(move |_, window, cx| {
                                let agent = agent_model.clone();
                                agent.update(cx, |a, cx| a.request_models(cx));
                                model_picker::open(agent, window, cx);
                            }),
                    ),
            )
            .when_some(self.render_progress(cx), |this, p| this.child(p))
            .when(show_changes, |this| this.child(div().flex_1().min_h_0().child(self.render_changes(&agent, cx))))
            .when(!show_changes, |this| {
                // The catch-up card lies over the top of the chat: inserting it
                // above pushed what you were reading down, twice (placeholder,
                // then the briefing).
                let catch_up = self.render_catch_up(cx).map(|c| {
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bg(theme.background)
                        .shadow_md()
                        .child(c)
                });
                this.child(
                    div().relative().flex_1().min_h_0().child(
                        div().id(("chat-scroll", id.as_u64())).size_full().overflow_y_scroll().track_scroll(&scroll).child(
                            v_flex()
                                .p_3()
                                .gap_3()
                                .text_sm()
                                .when(empty || self.show_help, |this| this.child(self.render_understand(cx)))
                                .children(items),
                        ),
                    )
                    .children(catch_up),
                )
            })
            .when(show_changes, |this| this.when_some(self.render_catch_up(cx), |this, c| this.child(c)))
            .when_some(dialog.map(|d| self.render_dialog(&agent, &d, cx)), |this, card| this.child(card))
            .when_some(
                match &status {
                    AgentStatus::Crashed { message, stderr_tail } => Some((message.clone(), stderr_tail.clone())),
                    _ => None,
                },
                |this, (message, tail)| {
                    this.child(
                        v_flex()
                            .gap_1()
                            .p_2()
                            .m_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.danger)
                            .text_xs()
                            .child(div().text_color(theme.danger).child(message))
                            .when(!tail.is_empty(), |this| {
                                this.child(
                                    div()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(theme.muted_foreground)
                                        .child(tail.join("\n")),
                                )
                            })
                            .child(h_flex().child(Button::new("restart").small().primary().label("Restart pi").on_click(
                                cx.listener(|this, _, _, cx| this.active(cx).update(cx, |a, cx| a.restart(cx))),
                            ))),
                    )
                },
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .children(steering.into_iter().map(|t| queued_row("steer", t, &theme)))
                    .children(follow_ups.into_iter().map(|t| queued_row("after this run", t, &theme)))
                    // The working line keeps its height when pi is idle, so finishing
                    // a run doesn't move the chat.
                    .child(
                        div()
                            .h(px(16.))
                            .text_xs()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(theme.muted_foreground)
                            .when(busy, |d| {
                                d.child(format!(
                                    "{status_text}  Enter steers · {} queues a follow-up",
                                    pide_core::keymap::label("alt-enter")
                                ))
                            }),
                    )
                    .when(!slash_rows.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .border_1()
                                .border_color(theme.border)
                                .rounded(theme.radius)
                                .py_1()
                                .text_sm()
                                .children(slash_rows.into_iter().enumerate().map(|(i, c)| {
                                    let pick = c.clone();
                                    h_flex()
                                        .id(("slash", i))
                                        .gap_2()
                                        .px_2()
                                        .py_0p5()
                                        .cursor_pointer()
                                        .when(i == 0, |r| r.bg(theme.list_active))
                                        .hover(|r| r.bg(theme.list_hover))
                                        .on_click(cx.listener(move |this, _, window, cx| this.complete(&pick, window, cx)))
                                        .child(div().flex_none().font_family(theme.mono_font_family.clone()).child(c.label.clone()))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child(c.detail.clone()),
                                        )
                                        .child(div().flex_1())
                                        .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(c.tag.clone()))
                                })),
                        )
                    })
                    .when(slash_rows_empty_hint, |this| {
                        this.child(div().text_xs().text_color(theme.muted_foreground).child("Loading pi's commands…"))
                    })
                    .when(!self.attachments.is_empty(), |this| {
                        this.child(h_flex().gap_2().flex_wrap().children(self.attachments.iter().enumerate().map(|(i, (_, img_data))| {
                            h_flex()
                                .gap_1()
                                .p_1()
                                .border_1()
                                .border_color(theme.border)
                                .rounded(theme.radius)
                                .child(img(img_data.clone()).h(px(40.)).max_w(px(120.)).object_fit(ObjectFit::Contain))
                                .child(Button::new(("remove-image", i)).xsmall().ghost().label("×").tooltip("Remove").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        if i < this.attachments.len() {
                                            this.attachments.remove(i);
                                        }
                                        cx.notify();
                                    }),
                                ))
                        })))
                    })
                    .child(
                        div()
                            .key_context("AgentInput")
                            // An image on the clipboard becomes an attachment; text pastes as usual.
                            .capture_action(cx.listener(|this, _: &Paste, _, cx| {
                                if this.paste_image(cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .on_action(|_: &BackToEditor, window, cx| {
                                CommandRegistry::run("editor.focus", window, cx);
                            })
                            .on_action(cx.listener(|this, _: &CompleteSlash, window, cx| {
                                match this.suggestions(cx).into_iter().next() {
                                    Some(first) => this.complete(&first, window, cx),
                                    None => window.focus_next(cx),
                                }
                            }))
                            .on_action(cx.listener(|this, _: &QueueFollowUp, window, cx| this.send(true, window, cx)))
                            .child(Textarea::new(&self.input)),
                    )
                    .child(h_flex().justify_between().gap_2().child({
                        let on = self.send_context(cx);
                        let label = match (on, self.context_now.clone()) {
                            (false, _) => "No editor context".to_string(),
                            // `main.rs:12` for a cursor, `main.rs 5–7` for a selection.
                            (true, Some(c)) => format!("⌁ Context: {}", c.label()),
                            (true, None) => "⌁ Context: no file open".to_string(),
                        };
                        Button::new("context-toggle")
                            .xsmall()
                            .label(label)
                            .map(|b| pide_core::colors::toggle(b, on, cx))
                            .tooltip("Send the active file and selection with new prompts")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_context(cx)))
                    }).when(busy, |this| {
                        this.child(Button::new("stop").xsmall().danger().label("Stop").on_click(
                            cx.listener(|this, _, _, cx| this.active(cx).update(cx, |a, cx| a.abort(cx))),
                        ))
                    })),
            )
    }
}

impl AgentChat {
    /// An `ide_pin` card: where, pi's note, and Open / Reply.
    fn render_pin(&self, pin_id: &str, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let agent = self.active(cx);
        let Some(stop) = agent.read(cx).pin_stop_by_id(pin_id) else {
            return div().into_any_element();
        };
        let snippet = agent
            .read(cx)
            .pins
            .iter()
            .find(|p| p.id == pin_id)
            .map(|p| p.snippet.clone())
            .unwrap_or_default();
        let first_line = stop.line;
        let root = self.workspace.read(cx).root().to_path_buf();
        let location = stop.location(&root);
        let (agent_open, stop_open) = (agent.clone(), stop.clone());
        let reply_to = stop.reply_prefix(&root);
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
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("PINNED"),
                    )
                    .child(
                        div()
                            .id(("pin-loc", ix))
                            .font_family(theme.mono_font_family.clone())
                            .text_color(pide_core::colors::accent(cx))
                            .cursor_pointer()
                            .hover(|s| s.underline())
                            .on_click(move |_, window, cx| {
                                pide_core::collab::go_to(&agent_open, stop_open.clone(), window, cx)
                            })
                            .child(location),
                    )
                    .when(stop.outdated, |this| {
                        this.child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child("outdated: the line changed"),
                        )
                    })
                    .when(stop.seen, |this| {
                        this.child(div().text_color(theme.muted_foreground).child("· seen"))
                    }),
            )
            // The lines pi pinned, as they were then (numbered like the editor).
            .when(!snippet.is_empty(), |this| {
                let width = (first_line + snippet.len()).to_string().len();
                this.child(
                    v_flex()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .bg(theme.secondary)
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .overflow_hidden()
                        .children(snippet.into_iter().enumerate().map(|(i, l)| {
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{:>width$}", first_line + i)),
                                )
                                .child(div().whitespace_nowrap().child(l.replace('\t', "    ")))
                        })),
                )
            })
            .child(div().text_sm().child(stop.note.clone()))
            .child(
                h_flex().gap_1().child(
                    Button::new(("pin-reply", ix))
                        .xsmall()
                        .ghost()
                        .label("Reply")
                        .tooltip("Answer pi about this spot")
                        .on_click(move |_, window, cx| {
                            pide_core::opener::ask_agent(reply_to.clone(), window, cx)
                        }),
                ),
            )
            .into_any_element()
    }

    /// pi's checklist (`ide_progress`), above the conversation; one line once all done.
    pub(crate) fn render_progress(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let agent = self.active(cx);
        let steps = agent.read(cx).progress.clone();
        if steps.is_empty() {
            return None;
        }
        let done = steps.iter().filter(|s| s.done).count();
        let all = done == steps.len();
        let open = self.progress_open || !all;
        let id = agent.entity_id();
        Some(
            v_flex()
                .flex_shrink_0()
                .gap_0p5()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(theme.border)
                .text_xs()
                .child(
                    h_flex()
                        .id(("progress-head", id.as_u64()))
                        .gap_2()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.progress_open = !this.progress_open;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.muted_foreground)
                                .child("PLAN"),
                        )
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(format!("{done}/{} done", steps.len())),
                        )
                        .child(div().flex_1())
                        .child(div().text_color(theme.muted_foreground).child(if open {
                            "▾"
                        } else {
                            "▸"
                        })),
                )
                .when(open, |this| {
                    this.children(steps.into_iter().map(|st| {
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(14.))
                                    .text_color(if st.done {
                                        pide_core::colors::accent(cx)
                                    } else {
                                        theme.muted_foreground
                                    })
                                    .child(if st.done { "✓" } else { "○" }),
                            )
                            .child(
                                div()
                                    .when(st.done, |d| d.text_color(theme.muted_foreground))
                                    .child(st.text),
                            )
                    }))
                })
                .into_any_element(),
        )
    }
}

pub(crate) fn queued_row(
    kind: &'static str,
    text: String,
    theme: &gpui_kit::component::theme::Theme,
) -> AnyElement {
    h_flex()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .border_1()
        .border_dashed()
        .border_color(theme.border)
        .text_xs()
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(kind),
        )
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(text),
        )
        .into_any_element()
}

pub(crate) fn tool_mut<'a>(items: &'a mut [Item], id: &str) -> Option<&'a mut Item> {
    items
        .iter_mut()
        .rev()
        .find(|i| matches!(i, Item::Tool { id: tid, .. } if tid == id))
}

/// pi's replies sit between 14 px chat text and 12 px tool cards, so markdown
/// headings stay close to the body (the default scale is for documents: an
/// `#` would be ~25 px here). Weight carries the hierarchy, as everywhere else.
pub(crate) fn chat_text_style() -> TextViewStyle {
    TextViewStyle::default().heading_font_size(|level, base| match level {
        1 => base * 1.25,
        2 => base * 1.125,
        _ => base,
    })
}
