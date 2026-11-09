//! Git change marks in the editor gutter (spec M4): lines added, changed or
//! deleted compared with the file at HEAD, kept current as you type. Drawn by
//! GPUI Kit's gutter decorations; we only compute where.

use std::time::Duration;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::{GutterDecoration, Position, Rope, RopeExt as _};
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
        let task = cx.background_executor().spawn(async move { git::head_text(&path) });
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
            let changes = cx.background_executor().spawn(async move { git::line_changes(&base, &current) }).await;
            let _ = this.update(cx, |this, cx| this.apply_git_marks(&rope, changes, cx));
        }));
    }

    fn apply_git_marks(&mut self, rope: &Rope, changes: Vec<LineChange>, cx: &mut Context<Self>) {
        // The text moved on since this was computed: the next refresh handles it.
        if self.editor.read(cx).text() != rope {
            return;
        }
        let status = &cx.theme().highlight_theme.style.status;
        let (added, modified, deleted) = (status.created(cx), status.modified(cx), status.deleted(cx));
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
                let g = self.editor.update(cx, |e, cx| e.create_gutter_decorations_collection(marks, cx));
                self.gutter = Some(g);
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
