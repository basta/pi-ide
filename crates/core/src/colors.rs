//! Semantic colors. The UI is neutral by default; a hue always means
//! something, and it means the same thing in every panel (AGENTS.md, Colors).
//!
//! GPUI Kit's theme names are a trap here: in the dark theme `primary` and
//! `link` are white and `accent` is the same gray as `border`. So "the
//! selected / active thing" is [`accent`] (blue), never `theme.primary`.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Selectable as _;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::{App, Hsla};

/// How a file differs from its baseline (HEAD, or before pi touched it).
/// JetBrains colors: modified blue, added green, deleted gray,
/// unversioned red, conflict red.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Modified,
    Added,
    Deleted,
    Untracked,
    Conflict,
}

impl Change {
    /// From a one-letter status (`M`, `A`, `D`, `R`, `?`, `U`…).
    pub fn from_letter(c: char) -> Self {
        match c {
            'A' => Self::Added,
            'D' => Self::Deleted,
            '?' => Self::Untracked,
            'U' => Self::Conflict,
            _ => Self::Modified,
        }
    }
}

/// The color of a file name or status letter for `kind`.
pub fn change(kind: Change, cx: &App) -> Hsla {
    let t = cx.theme();
    match kind {
        Change::Modified => t.blue,
        Change::Added => t.green,
        Change::Deleted => t.muted_foreground,
        Change::Untracked | Change::Conflict => t.red,
    }
}

/// The one accent: checked checkboxes, the active tab's marker, "set here"
/// markers, links, work in progress. Selected rows use `theme.list_active`
/// (a tint of the same blue), hovered rows `theme.list_hover`.
pub fn accent(cx: &App) -> Hsla {
    cx.theme().blue
}

/// pi's notes in the code (collaborator mode): pins and hunk notes in the
/// gutter, the note bar. Magenta, so it never reads as a git status.
pub fn pi_note(cx: &App) -> Hsla {
    cx.theme().magenta
}

/// Background of an inline notice (read-only file, conflict, pending
/// restart): a tint, with body-colored text, instead of a solid fill.
pub fn notice_bg(error: bool, cx: &App) -> Hsla {
    let t = cx.theme();
    if error {
        t.danger.opacity(0.16)
    } else {
        t.warning.opacity(0.14)
    }
}

/// A toggle (segmented choice, on/off chip): on = the selection tint with
/// body text, off = a quiet ghost button. Never `primary()`: a white fill is
/// reserved for the one action a view is about (Commit, a dialog's OK).
pub fn toggle(button: Button, on: bool, cx: &App) -> Button {
    let t = cx.theme();
    let button = button.selected(on);
    if on {
        let tint = t.list_active;
        button.custom(
            ButtonCustomVariant::new(cx)
                .color(tint)
                .foreground(t.foreground)
                .hover(tint)
                .active(tint),
        )
    } else {
        button.ghost()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_letters() {
        assert_eq!(Change::from_letter('A'), Change::Added);
        assert_eq!(Change::from_letter('D'), Change::Deleted);
        assert_eq!(Change::from_letter('R'), Change::Modified);
        assert_eq!(Change::from_letter('M'), Change::Modified);
        assert_eq!(Change::from_letter('?'), Change::Untracked);
    }
}
