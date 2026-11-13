//! "Catch me up" (collaborator mode): a short briefing on a session for someone
//! who didn't watch it. The panel writes down what it knows (prompts, replies,
//! tools, changes with pi's reasons, pins, plan); the model turns that into a
//! briefing on the side (`Agent::ask_aside`), so the session's own context and
//! history stay untouched.

use super::*;
use crate::conversation::chat_text_style;

/// Most characters of record sent (the newest part is kept).
const MAX_RECORD: usize = 24_000;

impl AgentChat {
    /// Write the session down as plain text for the briefing.
    pub(crate) fn session_record(&self, agent: &Entity<Agent>, cx: &App) -> String {
        let root = self.workspace.read(cx).root().to_path_buf();
        let rel = |p: &std::path::Path| p.strip_prefix(&root).unwrap_or(p).display().to_string();
        let clip = |t: &str, n: usize| {
            if t.chars().count() > n {
                format!("{}…", t.chars().take(n).collect::<String>())
            } else {
                t.to_string()
            }
        };
        let a = agent.read(cx);
        let mut out = String::new();
        if let Some(view) = self.views.get(&agent.entity_id()) {
            out.push_str("## Conversation\n");
            for item in &view.items {
                match item {
                    Item::User(_, text) => out.push_str(&format!("\nUSER: {}\n", clip(text, 1500))),
                    Item::Assistant { text, error, .. } => {
                        if !text.trim().is_empty() {
                            out.push_str(&format!("PI: {}\n", clip(text.trim(), 1200)));
                        }
                        if let Some(e) = error {
                            out.push_str(&format!("PI'S REPLY ENDED: {e}\n"));
                        }
                    }
                    Item::Tool {
                        name,
                        summary,
                        state,
                        ..
                    } => out.push_str(&format!(
                        "tool {name}{} {}\n",
                        if *state == ToolState::Failed { " (failed)" } else { "" },
                        clip(summary, 120)
                    )),
                    Item::Notice { text, error: true } => {
                        out.push_str(&format!("ERROR: {}\n", clip(text, 300)))
                    }
                    _ => {}
                }
            }
        }
        let changes: Vec<_> = a.changes.iter().filter(|c| !c.reverted).collect();
        if !changes.is_empty() {
            out.push_str("\n## Files pi changed\n");
            for c in changes {
                let stats = c.stats.map(|(p, m)| format!(" +{p} −{m}")).unwrap_or_default();
                out.push_str(&format!(
                    "- {} [{}{stats}]{}{}\n",
                    rel(&c.path),
                    c.badge(),
                    c.summary.as_deref().map(|s| format!(": {s}")).unwrap_or_default(),
                    if c.reviewed { " (user reviewed it)" } else { "" }
                ));
                for n in &c.notes {
                    out.push_str(&format!("  - line {}: {}\n", n.anchor.line, n.note));
                }
            }
        }
        let pins: Vec<_> = a.pins.iter().filter(|p| !p.dismissed).collect();
        if !pins.is_empty() {
            out.push_str("\n## Pins (pi asked the user to look)\n");
            for p in pins {
                out.push_str(&format!(
                    "- {}:{}: {}{}\n",
                    rel(&p.path),
                    p.anchor.line,
                    p.note,
                    if p.seen { " (seen)" } else { "" }
                ));
            }
        }
        if !a.progress.is_empty() {
            out.push_str("\n## Plan\n");
            for s in &a.progress {
                out.push_str(&format!("- [{}] {}\n", if s.done { "x" } else { " " }, s.text));
            }
        }
        if out.len() > MAX_RECORD {
            let mut cut = out.len() - MAX_RECORD;
            while !out.is_char_boundary(cut) {
                cut += 1;
            }
            out = format!("(earlier part left out)\n{}", &out[cut..]);
        }
        out
    }

    /// Ask for a fresh briefing on the active session.
    pub(crate) fn catch_up(&mut self, cx: &mut Context<Self>) {
        let agent = self.active(cx);
        if self.catching_up.contains(&agent.entity_id()) {
            return;
        }
        let record = self.session_record(&agent, cx);
        if record.trim().len() < 20 {
            return;
        }
        let prompt = pide_core::collab::catch_up_prompt(&record);
        let task = agent.update(cx, |a, cx| a.ask_aside(prompt, cx));
        let id = agent.entity_id();
        self.catching_up.insert(id);
        self.catch_up_open = true;
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.catching_up.remove(&id);
                match result {
                    Ok(text) => {
                        let at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        agent.update(cx, |a, cx| {
                            a.set_catch_up(Some(pide_core::collab::CatchUp { text, at }), cx)
                        });
                    }
                    Err(e) => {
                        if let Some(view) = this.views.get_mut(&id) {
                            view.items.push(Item::Notice {
                                text: format!("Couldn't catch you up: {e:#}"),
                                error: true,
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The briefing card above the conversation.
    pub(crate) fn render_catch_up(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let agent = self.active(cx);
        let id = agent.entity_id();
        let busy = self.catching_up.contains(&id);
        let brief = agent.read(cx).catch_up.clone();
        if !busy && (brief.is_none() || !self.catch_up_open) {
            return None;
        }
        let theme = cx.theme().clone();
        let ago = brief.as_ref().map(|b| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            fmt_ago(std::time::Duration::from_secs(now.saturating_sub(b.at)))
        });
        let markdown = brief.as_ref().map(|b| {
            let text = b.text.clone();
            let entry = self.catch_up_md.entry(id).or_insert_with(|| {
                (String::new(), cx.new(|cx| TextViewState::markdown("", cx)))
            });
            if entry.0 != text {
                entry.0 = text.clone();
                entry.1.update(cx, |m, cx| m.set_text(&text, cx));
            }
            entry.1.clone()
        });
        Some(
            v_flex()
                .id(("catch-up", id.as_u64()))
                .flex_shrink_0()
                .max_h(px(220.))
                .overflow_y_scroll()
                .gap_1()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .bg(pide_core::colors::pi_note(cx).opacity(0.06))
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.muted_foreground)
                                .child("CATCH UP"),
                        )
                        .child(div().text_color(theme.muted_foreground).child(if busy {
                            "pi is writing a briefing…".to_string()
                        } else {
                            ago.unwrap_or_default()
                        }))
                        .child(div().flex_1())
                        .when(!busy, |this| {
                            this.child(
                                Button::new("catch-up-refresh")
                                    .xsmall()
                                    .ghost()
                                    .label("Refresh")
                                    .on_click(cx.listener(|this, _, _, cx| this.catch_up(cx))),
                            )
                        })
                        .child(
                            Button::new("catch-up-close")
                                .xsmall()
                                .ghost()
                                .label("×")
                                .tooltip("Hide (it stays saved)")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.catch_up_open = false;
                                    cx.notify();
                                })),
                        ),
                )
                .when_some(markdown, |this, md| {
                    this.child(
                        div()
                            .text_sm()
                            .child(TextView::new(&md).style(chat_text_style()).selectable(true)),
                    )
                })
                .into_any_element(),
        )
    }
}
