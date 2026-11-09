//! Keep the server's diagnostics on their lines while the user adds or removes
//! lines, until the server publishes fresh ones (rust-analyzer's cargo check
//! results only come on save).

/// A line-count change: rows of an edit's start, old end and new end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LineEdit {
    pub start: u32,
    pub old_end: u32,
    pub new_end: u32,
    /// A pure insert at column 0: the start line's text moves down too.
    pub at_line_start: bool,
}

/// Where `line` is after `edits` (applied in order). Lines after an edit's
/// start line move by its row delta; lines inside a deleted range land on the
/// edit's start line. The edit's own start line stays.
pub(crate) fn shift_line(mut line: u32, edits: &[LineEdit]) -> u32 {
    for e in edits {
        if line < e.start || (line == e.start && !e.at_line_start) {
            continue;
        }
        if e.at_line_start {
            line += e.new_end - e.start;
            continue;
        }
        if line <= e.old_end {
            // Inside the replaced rows: keep it in the new text's range.
            line = line.min(e.new_end);
        } else {
            line = (line as i64 + e.new_end as i64 - e.old_end as i64).max(0) as u32;
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn lines_follow_inserts_and_deletes() {
        // Enter at the end of line 3: lines after 3 move down one.
        let enter = [LineEdit {
            start: 3,
            old_end: 3,
            new_end: 4,
            at_line_start: false,
        }];
        assert_eq!(shift_line(3, &enter), 3);
        assert_eq!(shift_line(9, &enter), 10);
        assert_eq!(shift_line(1, &enter), 1);
        // Two lines inserted at the top (an auto-import), then line 5 deleted.
        let edits = [
            LineEdit {
                start: 0,
                old_end: 0,
                new_end: 2,
                at_line_start: true,
            },
            LineEdit {
                start: 5,
                old_end: 6,
                new_end: 5,
                at_line_start: false,
            },
        ];
        assert_eq!(shift_line(0, &edits), 2);
        assert_eq!(shift_line(9, &edits), 10);
        assert_eq!(shift_line(4, &edits), 5);
        // Inside a deleted range: onto the edit's line.
        let del = [LineEdit {
            start: 2,
            old_end: 6,
            new_end: 2,
            at_line_start: false,
        }];
        assert_eq!(shift_line(4, &del), 2);
        assert_eq!(shift_line(8, &del), 4);
    }
}
