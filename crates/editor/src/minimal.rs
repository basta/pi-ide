//! Bring the editor's text to a new version with line-level edits instead of
//! replacing everything, so undo history survives (pi's edits can be undone
//! with Cmd+Z) and the view doesn't jump.

use std::ops::Range;

use gpui_kit::component::input::{EditorState, RopeExt as _};
use gpui_kit::*;

/// Byte ranges of `old` to replace (last first), with their new text.
pub(crate) fn line_edits(old: &str, new: &str) -> Vec<(Range<usize>, String)> {
    let starts = |s: &str| -> Vec<usize> {
        let mut v = vec![0];
        v.extend(s.match_indices('\n').map(|(i, _)| i + 1));
        if *v.last().unwrap() != s.len() {
            v.push(s.len());
        }
        v
    };
    let (os, ns) = (starts(old), starts(new));
    let diff = similar::TextDiff::from_lines(old, new);
    let mut out = Vec::new();
    for op in diff.ops() {
        if op.tag() == similar::DiffTag::Equal {
            continue;
        }
        let (o, n) = (op.old_range(), op.new_range());
        let at = |v: &Vec<usize>, i: usize| v[i.min(v.len() - 1)];
        out.push((
            at(&os, o.start)..at(&os, o.end),
            new[at(&ns, n.start)..at(&ns, n.end)].to_string(),
        ));
    }
    out.reverse();
    out
}

/// Make the editor's text `new`, keeping undo history and the caret's line.
pub(crate) fn set_text(
    e: &mut EditorState,
    new: &str,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) {
    let old = e.text().to_string();
    if old == new {
        return;
    }
    let rope = e.text().clone();
    let edits: Vec<lsp_types::TextEdit> = line_edits(&old, new)
        .into_iter()
        .map(|(r, text)| lsp_types::TextEdit {
            range: lsp_types::Range::new(
                rope.offset_to_position(r.start),
                rope.offset_to_position(r.end),
            ),
            new_text: text,
        })
        .collect();
    let cursor = e.cursor_position();
    e.apply_lsp_edits(&edits, window, cx);
    e.set_cursor_position(cursor, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &str, edits: &[(Range<usize>, String)]) -> String {
        let mut s = old.to_string();
        for (r, t) in edits {
            s.replace_range(r.clone(), t);
        }
        s
    }

    #[::core::prelude::v1::test]
    fn edits_rebuild_the_new_text() {
        for (old, new) in [
            ("a\nb\nc\n", "a\nB\nc\nd\n"),
            ("notes\nchecked\nsaved", "NOTES\nchecked\nsaved\nend"),
            ("x\ny\nz\n", "x\nz\n"),
            ("", "hello\n"),
            ("hello\n", ""),
            ("só\nü\n", "só\nüü\n"),
        ] {
            let edits = line_edits(old, new);
            assert_eq!(apply(old, &edits), new, "{old:?} -> {new:?}");
        }
        // Only the changed line is touched.
        assert_eq!(
            line_edits("a\nb\nc\n", "a\nB\nc\n"),
            vec![(2..4, "B\n".to_string())]
        );
    }
}
