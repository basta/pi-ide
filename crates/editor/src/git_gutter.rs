//! Git change marks in the editor gutter (spec M4): lines added, changed or
//! deleted compared with the file at HEAD, kept current as you type. Drawn by
//! GPUI Kit's gutter decorations; we only compute where.

use std::time::Duration;

use gpui_kit::component::input::{GutterDecoration, Position, Rope, RopeExt as _};
use gpui_kit::component::{ActiveTheme, WindowExt as _};
use gpui_kit::*;
use pide_core::git::{self, ChangeKind, LineChange};

use crate::EditorPanel;

/// The committed text: not loaded yet, or loaded (`None` = untracked / no repo).
#[derive(Default)]
pub(crate) enum Base {
    #[default]
    Unknown,
    Loaded(Option<String>),
}

impl EditorPanel {
    /// (Re)read the file's text at HEAD, then refresh the marks.
    pub(crate) fn load_git_base(&mut self, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let task = cx
            .background_executor()
            .spawn(async move { git::head_text(&path) });
        self._git_task = Some(cx.spawn(async move |this, cx| {
            let base = task.await;
            let _ = this.update(cx, |this, cx| {
                this.git_base = Base::Loaded(base);
                this.refresh_git_marks(Duration::ZERO, cx);
            });
        }));
    }

    /// Recompute the marks (after `delay`, so typing doesn't diff every keystroke).
    pub(crate) fn refresh_git_marks(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Base::Loaded(Some(base)) = &self.git_base else {
            if let Some(g) = self.gutter.take() {
                g.dispose(cx);
            }
            return;
        };
        let base = base.clone();
        let rope = self.editor.read(cx).text().clone();
        self._git_marks = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let current = rope.to_string();
            let changes = cx
                .background_executor()
                .spawn(async move { git::line_changes(&base, &current) })
                .await;
            let _ = this.update(cx, |this, cx| this.apply_git_marks(&rope, changes, cx));
        }));
    }

    fn apply_git_marks(&mut self, rope: &Rope, changes: Vec<LineChange>, cx: &mut Context<Self>) {
        // The text moved on since this was computed: the next refresh handles it.
        if self.editor.read(cx).text() != rope {
            return;
        }
        let status = &cx.theme().highlight_theme.style.status;
        // Modified is blue, as in every git list (GPUI Kit's own is yellow).
        let modified = pide_core::colors::change(pide_core::colors::Change::Modified, cx);
        let (added, deleted) = (status.created(cx), status.deleted(cx));
        let marks: Vec<GutterDecoration> = changes
            .iter()
            .map(|c| {
                let range = byte_range(rope, &c.lines);
                let color = match c.kind {
                    ChangeKind::Added => added,
                    ChangeKind::Modified => modified,
                    ChangeKind::Deleted => deleted,
                };
                GutterDecoration::new(range).with_color(color)
            })
            .collect();
        match &self.gutter {
            Some(g) => g.set(marks, cx),
            None => {
                let g = self.editor.update(cx, |e, cx| {
                    e.create_gutter_decorations_collection(marks, cx)
                });
                self.gutter = Some(g);
                // pi's marks go on top: make their collection again, after this one.
                self.pi_marks_stale();
                self.refresh_pi_marks(cx);
            }
        }
    }
}

/// Lines `a..b` as a byte range (whole lines); an empty range is the boundary
/// before line `a` (after the last line if `a` is past the end).
fn byte_range(rope: &Rope, lines: &std::ops::Range<u32>) -> std::ops::Range<usize> {
    let line_count = rope.lines_len() as u32;
    let start_of = |line: u32| {
        if line >= line_count {
            rope.len()
        } else {
            rope.position_to_offset(&Position::new(line, 0))
        }
    };
    if lines.is_empty() {
        let at = start_of(lines.start);
        return at..at;
    }
    let start = start_of(lines.start);
    let last = (lines.end - 1).min(line_count.saturating_sub(1));
    let end = rope.line_end_offset(last as usize).max(start);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn line_ranges_to_bytes() {
        let rope = Rope::from("ab\ncd\nef");
        assert_eq!(byte_range(&rope, &(1..2)), 3..5);
        assert_eq!(byte_range(&rope, &(0..3)), 0..8);
        assert_eq!(byte_range(&rope, &(1..1)), 3..3);
        assert_eq!(byte_range(&rope, &(9..9)), 8..8);
    }
}

/// The change (vs HEAD) at `line`: its hunk, if the line is in one (a deletion
/// counts on the line where text was removed, or the one before).
fn hunk_at(base: &str, current: &str, line: usize) -> Option<pide_core::changes::Hunk> {
    pide_core::changes::hunks(base, current)
        .into_iter()
        .find(|h| {
            if h.after.is_empty() {
                line == h.after.start || line + 1 == h.after.start
            } else {
                h.after.contains(&line)
            }
        })
}

/// Git: Rollback Lines — put the change under the cursor back the way it is at HEAD (undoable).
pub(crate) fn rollback(window: &mut Window, cx: &mut App) {
    let Some(panel) = crate::active_panel(window, cx) else {
        return;
    };
    let (editor, base) = {
        let p = panel.read(cx);
        let Base::Loaded(Some(base)) = &p.git_base else {
            window.push_notification(
                gpui_kit::component::notification::Notification::warning(
                    "This file isn't tracked by git.",
                ),
                cx,
            );
            return;
        };
        (p.editor.clone(), base.clone())
    };
    let (current, line) = {
        let e = editor.read(cx);
        (e.text().to_string(), e.cursor_position().line as usize)
    };
    let Some(hunk) = hunk_at(&base, &current, line) else {
        window.push_notification(
            gpui_kit::component::notification::Notification::info(
                "No change from git at the cursor.",
            ),
            cx,
        );
        return;
    };
    let text = pide_core::changes::revert_hunk(&base, &current, &hunk);
    crate::minimal::set_text(&editor, text, false, window, cx);
}

/// Git: Next / Previous Change — move the cursor to the next (or previous) changed block.
pub(crate) fn jump_change(forward: bool, window: &mut Window, cx: &mut App) {
    let Some(panel) = crate::active_panel(window, cx) else {
        return;
    };
    let (editor, base) = {
        let p = panel.read(cx);
        let Base::Loaded(Some(base)) = &p.git_base else {
            return;
        };
        (p.editor.clone(), base.clone())
    };
    let (current, line) = {
        let e = editor.read(cx);
        (e.text().to_string(), e.cursor_position().line as usize)
    };
    let starts: Vec<usize> = pide_core::changes::hunks(&base, &current)
        .iter()
        .map(|h| h.after.start)
        .collect();
    let target = if forward {
        starts
            .iter()
            .copied()
            .find(|&s| s > line)
            .or(starts.first().copied())
    } else {
        starts
            .iter()
            .rev()
            .copied()
            .find(|&s| s < line)
            .or(starts.last().copied())
    };
    if let Some(t) = target {
        editor.update(cx, |e, cx| {
            e.set_cursor_position(Position::new(t as u32, 0), window, cx)
        });
    }
}

#[cfg(test)]
mod rollback_tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn finds_the_change_at_a_line() {
        let base = "a\nb\nc\nd\n";
        let current = "a\nB\nc\n";
        let h = hunk_at(base, current, 1).unwrap();
        assert_eq!(
            pide_core::changes::revert_hunk(base, current, &h),
            "a\nb\nc\n"
        );
        assert!(hunk_at(base, current, 0).is_none());
        // The deleted "d" counts on the last line.
        let h = hunk_at(base, current, 2).unwrap();
        assert_eq!(
            pide_core::changes::revert_hunk(base, current, &h),
            "a\nB\nc\nd\n"
        );
    }
}
