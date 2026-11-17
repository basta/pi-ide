//! Session tabs (narrow) and the session sidebar (wide).

use super::*;

/// Where a tour in the list lives.
#[derive(Clone)]
pub(crate) enum TourSource {
    Open(Entity<Agent>),
    /// A saved session: its file and name.
    Saved(PathBuf, SharedString),
}

impl AgentChat {
    /// Show a tour's session (resuming a saved one; pi itself starts only on the
    /// next message) and walk the tour.
    pub(crate) fn open_tour(
        &mut self,
        source: TourSource,
        call_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_list = false;
        let agent = match source {
            TourSource::Open(agent) => agent,
            TourSource::Saved(file, name) => self
                .sessions
                .update(cx, |s, cx| s.add(Some(file), Some(name.to_string()), cx)),
        };
        self.sessions.update(cx, |s, cx| s.activate(&agent, cx));
        cx.notify();
        // A resumed session's pins load in the background: try until they're there.
        cx.spawn_in(window, async move |_, cx| {
            for _ in 0..30 {
                let started = cx
                    .update(|window, cx| pide_core::collab::start_tour(&agent, &call_id, window, cx))
                    .unwrap_or(true);
                if started {
                    return;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
            }
            let _ = cx.update(|window, cx| {
                window.push_notification(
                    Notification::info("That tour's stops aren't there any more."),
                    cx,
                )
            });
        })
        .detach();
    }

    pub(crate) fn dot(agent: &Agent, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let base = div().size(px(8.)).rounded_full().flex_none();
        match agent.status() {
            AgentStatus::Crashed { .. } => base.bg(theme.danger).into_any_element(),
            _ if agent.waiting => base.bg(theme.warning).into_any_element(),
            AgentStatus::Busy | AgentStatus::Starting | AgentStatus::Aborting => {
                base.bg(pide_core::colors::accent(cx)).into_any_element()
            }
            _ if agent.unread => div()
                .flex_none()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.success)
                .child("✓")
                .into_any_element(),
            _ => base
                .border_1()
                .border_color(theme.muted_foreground)
                .into_any_element(),
        }
    }

    pub(crate) fn status_text(agent: &Agent) -> &'static str {
        match agent.status() {
            AgentStatus::Idle if agent.waiting => "Waiting for your answer",
            AgentStatus::Idle => "Idle",
            AgentStatus::Starting => "Starting pi…",
            AgentStatus::Busy => "pi is working…",
            AgentStatus::Aborting => "Stopping…",
            AgentStatus::Crashed { .. } => "pi crashed",
        }
    }

    pub(crate) fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sessions = self.sessions.read(cx);
        let active = sessions.active_index();
        let theme = cx.theme().clone();
        // The active session's tab is always in view (a new one was off the end).
        if self.tabs_shown.replace(Some(active)) != Some(active) {
            self.tabs_scroll.scroll_to_item(active);
        }
        let tabs: Vec<AnyElement> = sessions
            .all()
            .iter()
            .enumerate()
            .map(|(i, agent)| {
                let a = agent.read(cx);
                let selected = i == active;
                let agent_close = agent.clone();
                h_flex()
                    .id(("session-tab", i))
                    .flex_none()
                    .gap_1p5()
                    .items_center()
                    .px_2p5()
                    .py_1p5()
                    .max_w(px(180.))
                    .border_r_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.bg(theme.background)
                            .border_t_2()
                            .border_color(pide_core::colors::accent(cx))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sessions.update(cx, |s, cx| s.set_active(i, cx));
                    }))
                    .child(Self::dot(a, cx))
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .when(a.unread, |this| this.font_weight(FontWeight::SEMIBOLD))
                            .child(a.name.clone()),
                    )
                    .child(
                        div()
                            .id(("session-close", i))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .px_0p5()
                            .child("✕")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.sessions.update(cx, |s, cx| s.close(&agent_close, cx));
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        h_flex()
            .w_full()
            .text_sm()
            .bg(theme.secondary)
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .id("session-tabs")
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .track_scroll(&self.tabs_scroll)
                    .children(tabs),
            )
            .child(
                Button::new("new-session")
                    .ghost()
                    .small()
                    .label("+")
                    .tooltip("New session")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sessions.update(cx, |s, cx| {
                            s.add(None, None, cx);
                        })
                    })),
            )
            .child(
                Button::new("session-list")
                    .ghost()
                    .small()
                    .label(if self.show_list { "▴" } else { "▾" })
                    .tooltip("All sessions")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_list = !this.show_list;
                        if this.show_list {
                            this.refresh_saved(cx);
                        }
                        cx.notify();
                    })),
            )
    }

    pub(crate) fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let sessions = self.sessions.read(cx);
        let active = sessions.active_index();
        let open_files: Vec<PathBuf> = sessions.open_files(cx);
        // Collaborator mode: sessions with changes you haven't stepped through go first.
        let review: Vec<AnyElement> = sessions
            .all()
            .iter()
            .enumerate()
            .filter(|(_, a)| a.read(cx).needs_review())
            .map(|(i, agent)| {
                let a = agent.read(cx);
                let unseen = pide_core::walk::unseen_change_notes(&a.changes, &a.pins).len();
                let files = a.changes.iter().filter(|c| !c.reverted).count();
                let agent = agent.clone();
                h_flex()
                    .id(("review-session", i))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.list_hover))
                    .when(i == active, |this| this.bg(theme.list_active))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.sessions.update(cx, |s, cx| s.set_active(i, cx));
                        pide_core::collab::start_walk(&agent, window, cx);
                    }))
                    .child(div().text_color(pide_core::colors::pi_note(cx)).child("◆"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(a.name.clone()),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                        "{unseen} to step through{}",
                        match files {
                            0 => String::new(),
                            1 => " · 1 file".to_string(),
                            n => format!(" · {n} files"),
                        }
                    )))
                    .into_any_element()
            })
            .collect();
        let open: Vec<AnyElement> = sessions
            .all()
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.read(cx).needs_review())
            .map(|(i, agent)| {
                let a = agent.read(cx);
                let label = match a.status() {
                    AgentStatus::Busy | AgentStatus::Starting | AgentStatus::Aborting => "working",
                    AgentStatus::Crashed { .. } => "crashed",
                    _ if a.waiting => "needs you",
                    _ if a.unread => "done",
                    _ => "",
                };
                h_flex()
                    .id(("open-session", i))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.list_hover))
                    .when(i == active, |this| this.bg(theme.list_active))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sessions.update(cx, |s, cx| s.set_active(i, cx));
                    }))
                    .child(Self::dot(a, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .when(a.unread, |this| this.font_weight(FontWeight::SEMIBOLD))
                            .child(a.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                    .into_any_element()
            })
            .collect();
        let saved: Vec<AnyElement> = self
            .saved
            .iter()
            .filter(|s| !open_files.contains(&s.path))
            .take(50)
            .enumerate()
            .map(|(i, s)| {
                let path = s.path.clone();
                let name = s.name.clone();
                let title = s
                    .name
                    .clone()
                    .unwrap_or_else(|| s.preview.chars().take(60).collect());
                let ago = s.modified.elapsed().map(fmt_ago).unwrap_or_default();
                v_flex()
                    .id(("saved-session", i))
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.list_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (path, name) = (path.clone(), name.clone());
                        this.sessions.update(cx, |s, cx| {
                            s.add(Some(path), name, cx);
                        });
                    }))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(ago),
                            ),
                    )
                    .when(s.name.is_some(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(s.preview.clone()),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        // Tours (ide_tour) in open sessions, then in saved ones: click to walk one.
        let mut tour_rows: Vec<(SharedString, pide_core::notes::TourInfo, TourSource)> = Vec::new();
        for agent in sessions.all() {
            let a = agent.read(cx);
            for t in pide_core::notes::tours(&a.pins) {
                tour_rows.push((a.name.clone(), t, TourSource::Open(agent.clone())));
            }
        }
        for (file, tours) in &self.saved_tours {
            if open_files.contains(file) {
                continue;
            }
            let name: SharedString = self
                .saved
                .iter()
                .find(|s| &s.path == file)
                .map(|s| s.name.clone().unwrap_or_else(|| s.preview.chars().take(40).collect()))
                .unwrap_or_default()
                .into();
            for t in tours {
                tour_rows.push((name.clone(), t.clone(), TourSource::Saved(file.clone(), name.clone())));
            }
        }
        let tours: Vec<AnyElement> = tour_rows
            .into_iter()
            .enumerate()
            .map(|(i, (session, t, source))| {
                let call = t.call_id.clone();
                h_flex()
                    .id(("tour-row", i))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.list_hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_tour(source.clone(), call.clone(), window, cx)
                    }))
                    .child(div().flex_none().text_color(pide_core::colors::pi_note(cx)).child("◇"))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(t.title.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(session),
                            ),
                    )
                    .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(format!(
                        "{}/{} seen",
                        t.seen, t.stops
                    )))
                    .into_any_element()
            })
            .collect();
        let heading = |t: &'static str| {
            div()
                .px_3()
                .pt_3()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(t)
        };
        v_flex()
            .id("session-list")
            .size_full()
            .overflow_y_scroll()
            .text_sm()
            .when(!review.is_empty(), |this| {
                this.child(heading("TO REVIEW")).children(review)
            })
            .when(!open.is_empty(), |this| this.child(heading("OPEN")))
            .children(open)
            .when(!tours.is_empty(), |this| this.child(heading("TOURS")).children(tours))
            .child(heading("SAVED IN THIS PROJECT"))
            .when(saved.is_empty(), |this| {
                this.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("None yet."),
                )
            })
            .children(saved)
    }
}
