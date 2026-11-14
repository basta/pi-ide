//! The "Understand" card (collaborator mode): what you can ask pi to help you
//! follow the code, one line each, and what the marks in the editor mean. Shown
//! in an empty session and behind the ? in the pi toolbar. No LLM call.

use super::*;

/// (title, what it's for, command id)
const VERBS: &[(&str, &str, &str)] = &[
    (
        "Take a Tour…",
        "Ask how something works; pi walks you through the code across files, stop by stop.",
        "agent.tour",
    ),
    (
        "Explain This Code",
        "Select code (or put the cursor in it); pi explains it with notes in reading order.",
        "agent.explain_this",
    ),
    (
        "Walk Through Changes",
        "Step through what pi changed and why, note by note, in the editor.",
        "agent.walk_through",
    ),
    (
        "Tell It as a Story",
        "pi retells this session's change as a tour, in the order that explains it.",
        "agent.change_story",
    ),
    (
        "Catch Me Up",
        "A short briefing on this session: what you asked, what pi did, what needs you.",
        "agent.catch_up",
    ),
];

impl AgentChat {
    pub(crate) fn render_understand(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let pi = pide_core::colors::pi_note(cx);
        let swatch = |c: Hsla| div().flex_none().mt(px(1.)).w(px(3.)).h(px(14.)).rounded(px(1.)).bg(c);
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child("Ask pi anything below; it starts on your first message. To follow the code:"),
            )
            .children(VERBS.iter().enumerate().map(|(i, (title, what, cmd))| {
                let cmd = *cmd;
                v_flex()
                    .id(("understand", i))
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .on_click(move |_, window, cx| {
                        CommandRegistry::run(cmd, window, cx);
                    })
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(*title))
                    .child(div().min_w_0().text_color(theme.muted_foreground).child(*what))
            }))
            .child(
                v_flex()
                    .gap_1()
                    .pt_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("IN THE EDITOR"))
                    .child(h_flex().gap_2().items_start().child(swatch(pi)).child(div().min_w_0().child("A note from this session: the bar above the editor shows it, with Reply, Back and Next.")))
                    .child(h_flex().gap_2().items_start().child(swatch(pi.opacity(0.4))).child(div().min_w_0().child("A note from another open session (pi → pi's Notes in This File lists them all).")))
                    .child(h_flex().gap_2().items_start().child(swatch(pide_core::colors::change(pide_core::colors::Change::Modified, cx))).child(div().min_w_0().child("Changed since the last commit (git).")))
                    .child(div().child("“outdated” means the line a note was about has changed since.")),
            )
            .into_any_element()
    }
}
