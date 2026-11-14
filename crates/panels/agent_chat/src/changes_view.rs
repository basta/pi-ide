//! The Changes view: what pi changed in a session, revert, review.

use super::*;

impl AgentChat {
    /// Revert one file after asking: it overwrites the file, so it also drops later edits
    /// (yours, or another session's — named in the question).
    pub(crate) fn confirm_revert(
        &mut self,
        agent: Entity<Agent>,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.workspace.read(cx).root().to_path_buf();
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let others: Vec<String> = self
            .sessions
            .read(cx)
            .all()
            .iter()
            .filter(|a| **a != agent)
            .filter(|a| {
                a.read(cx)
                    .changes
                    .iter()
                    .any(|c| c.path == path && !c.reverted)
            })
            .map(|a| a.read(cx).name.to_string())
            .collect();
        let created = agent
            .read(cx)
            .changes
            .iter()
            .any(|c| c.path == path && c.before.is_none());
        let mut text = if created {
            format!("pi created {rel} in this session. Reverting deletes it.")
        } else {
            format!(
                "This puts {rel} back to how it was before pi first touched it in this session. Any later edits to it are lost, including yours."
            )
        };
        if !others.is_empty() {
            text.push_str(&format!(
                "\n\nIt was also changed by: {}. Their changes to this file are undone too.",
                others.join(", ")
            ));
        }
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (agent, path) = (agent.clone(), path.clone());
            alert
                .title(format!(
                    "Revert {}?",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                ))
                .description(text.clone())
                .confirm()
                .ok_text("Revert")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let task = agent.update(cx, |a, cx| a.revert(&path, cx));
                    window
                        .spawn(cx, async move |cx| {
                            if let Err(e) = task.await {
                                let _ = cx.update(|window, cx| {
                                    window.push_notification(
                                        Notification::error(format!("Revert failed: {e:#}")),
                                        cx,
                                    )
                                });
                            }
                        })
                        .detach();
                    true
                })
        });
    }

    /// Revert every file this session changed (after asking).
    pub(crate) fn confirm_revert_all(
        &mut self,
        agent: Entity<Agent>,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let n = paths.len();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (agent, paths) = (agent.clone(), paths.clone());
            alert
                .title(format!("Revert all {n} file{}?", if n == 1 { "" } else { "s" }))
                .description("Every file goes back to how it was before pi first touched it in this session. Later edits to them are lost, including yours.")
                .confirm()
                .ok_text("Revert all")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let tasks: Vec<_> = paths.iter().map(|p| agent.update(cx, |a, cx| a.revert(p, cx))).collect();
                    window
                        .spawn(cx, async move |cx| {
                            let mut failed = Vec::new();
                            for t in tasks {
                                if let Err(e) = t.await {
                                    failed.push(format!("{e:#}"));
                                }
                            }
                            if !failed.is_empty() {
                                let _ = cx.update(|window, cx| {
                                    window.push_notification(Notification::error(format!("Some files weren't reverted: {}", failed.join("; "))), cx)
                                });
                            }
                        })
                        .detach();
                    true
                })
        });
    }

    pub(crate) fn render_changes(
        &self,
        agent: &Entity<Agent>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let root = self.workspace.read(cx).root().to_path_buf();
        let label: SharedString = agent.read(cx).name.clone();
        let changes = agent.read(cx).changes.clone();
        if changes.is_empty() {
            return div()
                .p_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("pi hasn't changed any files in this session yet. Files it edits or writes show up here.")
                .into_any_element();
        }
        // Files other open sessions also changed.
        let shared: Vec<PathBuf> = self
            .sessions
            .read(cx)
            .all()
            .iter()
            .filter(|a| *a != agent)
            .flat_map(|a| {
                a.read(cx)
                    .changes
                    .iter()
                    .filter(|c| !c.reverted)
                    .map(|c| c.path.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        // Files pi changed without saying why (it skipped ide_explain_change).
        let unexplained: Vec<String> = changes
            .iter()
            .filter(|c| !c.reverted && c.summary.is_none() && c.notes.is_empty())
            .map(|c| c.path.strip_prefix(&root).unwrap_or(&c.path).display().to_string())
            .collect();
        let agent_explain = agent.clone();
        let rows: Vec<AnyElement> = changes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let rel = c
                    .path
                    .strip_prefix(&root)
                    .unwrap_or(&c.path)
                    .display()
                    .to_string();
                let badge = c.badge();
                let badge_color = pide_core::colors::change(
                    pide_core::colors::Change::from_letter(badge.chars().next().unwrap_or('M')),
                    cx,
                );
                let flagged = shared.contains(&c.path);
                let (diff_path, diff_before, diff_label) =
                    (c.path.clone(), c.before.clone(), label.clone());
                let (agent_r, path_r) = (agent.clone(), c.path.clone());
                let (agent_v, path_v, reviewed) = (agent.clone(), c.path.clone(), c.reviewed);
                v_flex()
                    .id(("change", i))
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.list_hover))
                    .on_click(cx.listener(move |_, _, window, cx| {
                        pide_core::opener::open_diff(
                            pide_core::opener::DiffRequest {
                                path: diff_path.clone(),
                                before: diff_before.clone(),
                                after: None,
                                label: diff_label.clone(),
                                before_is: "when pi first touched it".into(),
                            },
                            window,
                            cx,
                        );
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .w(px(12.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(badge_color)
                                    .child(badge),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .when(c.reverted, |this| {
                                        this.line_through().text_color(theme.muted_foreground)
                                    })
                                    .when(!c.reviewed && !c.reverted, |this| {
                                        this.font_weight(FontWeight::SEMIBOLD)
                                    })
                                    .child(rel),
                            )
                            .when_some(c.stats, |this, (a, d)| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.success)
                                        .child(format!("+{a}")),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.danger)
                                        .child(format!("−{d}")),
                                )
                            }),
                    )
                    // pi's reason for the change (`ide_explain_change`).
                    .when_some(c.summary.clone(), |this, summary| {
                        this.child(
                            div()
                                .pl(px(20.))
                                .when(c.reverted, |d| d.text_color(theme.muted_foreground))
                                .child(summary),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .pl(px(20.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            // No reason from pi: one click asks it about every such file.
                            .when(!c.reverted && c.summary.is_none() && c.notes.is_empty(), |this| {
                                let (agent, files) = (agent_explain.clone(), unexplained.clone());
                                this.child(
                                    div()
                                        .id(("ask-why", i))
                                        .text_color(pide_core::colors::pi_note(cx))
                                        .cursor_pointer()
                                        .hover(|s| s.underline())
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            send_or_queue(&agent, ask_why_prompt(&files), cx);
                                        })
                                        .child("no reason given · ask why"),
                                )
                            })
                            .child(div().flex_1().child(if c.reverted {
                                "reverted".into()
                            } else if c.notes.is_empty() {
                                c.tools_label()
                            } else {
                                let n = c.notes.len();
                                format!(
                                    "{} · {n} note{}",
                                    c.tools_label(),
                                    if n == 1 { "" } else { "s" }
                                )
                                .into()
                            }))
                            .when(flagged, |this| {
                                this.child(
                                    div()
                                        .text_color(theme.warning)
                                        .child("also changed by another session"),
                                )
                            })
                            .when(!c.reverted, |this| {
                                this.child(
                                    Button::new(("reviewed", i))
                                        .xsmall()
                                        .ghost()
                                        .label(if reviewed {
                                            "Reviewed ✓"
                                        } else {
                                            "Mark reviewed"
                                        })
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.stop_propagation();
                                            agent_v.update(cx, |a, cx| {
                                                a.set_reviewed(&path_v, !reviewed, cx)
                                            });
                                        })),
                                )
                                .child(
                                    Button::new(("revert", i))
                                        .xsmall()
                                        .label("Revert")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.confirm_revert(
                                                agent_r.clone(),
                                                path_r.clone(),
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        let open: Vec<PathBuf> = changes
            .iter()
            .filter(|c| !c.reverted)
            .map(|c| c.path.clone())
            .collect();
        let unreviewed = changes
            .iter()
            .filter(|c| !c.reviewed && !c.reverted)
            .count();
        let (agent_all, agent_rev) = (agent.clone(), agent.clone());
        let (stops, unseen) = {
            let a = agent.read(cx);
            (a.stops().len(), a.unseen_stops())
        };
        let agent_walk = agent.clone();

        let agent_story = agent.clone();
        let story_files = changes.iter().filter(|c| !c.reverted).count();
        let header = h_flex()
            .flex_wrap() // the buttons go to a second row in a narrow panel
            .gap_2()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().child(format!(
                "{} file{} · {} to review",
                changes.len(),
                if changes.len() == 1 { "" } else { "s" },
                unreviewed
            )))
            .when(story_files > 0, |this| {
                this.child(
                    Button::new("change-story")
                        .xsmall()
                        .label("Tell it as a story")
                        .tooltip("Ask pi to walk you through its change in the order that explains it, as a tour")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let prompt = story_prompt();
                            agent_story.update(cx, |a, cx| {
                                if a.is_busy() {
                                    a.queue(prompt, true, cx);
                                } else {
                                    a.prompt(prompt, cx);
                                }
                            });
                        })),
                )
            })
            .when(stops > 0, |this| {
                this.child(
                    Button::new("walk-through")
                        .xsmall()
                        .label(if unseen > 0 {
                            format!("Walk through · {unseen}")
                        } else {
                            "Walk through".to_string()
                        })
                        .tooltip("Step through pi's changes and notes in the editor")
                        .on_click(move |_, window, cx| {
                            pide_core::collab::start_walk(&agent_walk, window, cx);
                        }),
                )
            })
            .when(unreviewed > 0, |this| {
                this.child(
                    Button::new("review-all")
                        .xsmall()
                        .ghost()
                        .label("Mark all reviewed")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let paths: Vec<PathBuf> = agent_all
                                .read(cx)
                                .changes
                                .iter()
                                .map(|c| c.path.clone())
                                .collect();
                            agent_all.update(cx, |a, cx| {
                                for p in &paths {
                                    a.set_reviewed(p, true, cx);
                                }
                            });
                        })),
                )
            })
            .when(!open.is_empty(), |this| {
                this.child(
                    Button::new("revert-all")
                        .xsmall()
                        .ghost()
                        .label("Revert all…")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_revert_all(agent_rev.clone(), open.clone(), window, cx)
                        })),
                )
            });
        v_flex()
            .id("changes-list")
            .size_full()
            .overflow_y_scroll()
            .text_sm()
            .child(header)
            .children(rows)
            .into_any_element()
    }
}

/// What "ask why" asks pi about files it changed without a reason.
pub(crate) fn ask_why_prompt(files: &[String]) -> String {
    format!(
        "Don't change any code. For each of these files you changed, call \
         ide_explain_change with a one-line summary of why, and hunk notes \
         where the reason isn't obvious: {}",
        files.join(", ")
    )
}

/// What "Tell it as a story" asks pi.
pub(crate) fn story_prompt() -> String {
    format!(
        "Don't change any code. Walk me through the change you made in this session as a story: \
         call ide_tour once, titled \"{}\", with stops at the places you changed (and any \
         unchanged code needed to follow it), in the order that explains the change from its entry \
         point onward, not file by file. Each note is one to three plain sentences: what changed \
         there, why, and how it leads to the next stop. Don't number the notes or start them with \
         \"Step N\" (pide shows the position).",
        pide_core::collab::STORY_TITLE
    )
}

/// Send `prompt` to `agent` now, or after its current run.
pub(crate) fn send_or_queue(agent: &Entity<Agent>, prompt: String, cx: &mut App) {
    agent.update(cx, |a, cx| {
        if a.is_busy() {
            a.queue(prompt, true, cx);
        } else {
            a.prompt(prompt, cx);
        }
    });
}
