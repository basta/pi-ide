//! Positions between GPUI Kit and a server.
//!
//! GPUI Kit's editor counts LSP columns in characters (Unicode scalar values);
//! servers count in UTF-16 code units unless they agree to something else at
//! `initialize`. Everything crossing the boundary goes through here.

use std::borrow::Cow;

use gpui_kit::component::input::{Rope, RopeExt as _};
use lsp_types::{Position, Range};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Encoding {
    Utf8,
    #[default]
    Utf16,
    /// Characters: what GPUI Kit uses, so no conversion.
    Utf32,
}

impl Encoding {
    pub fn from_capability(enc: Option<&lsp_types::PositionEncodingKind>) -> Self {
        match enc.map(|e| e.as_str()) {
            Some("utf-8") => Self::Utf8,
            Some("utf-32") => Self::Utf32,
            _ => Self::Utf16,
        }
    }
}

/// Line text by row (without the line break).
pub trait Lines {
    fn line(&self, row: u32) -> Cow<'_, str>;
}

impl Lines for Rope {
    fn line(&self, row: u32) -> Cow<'_, str> {
        Cow::Owned(self.slice_line(row as usize).to_string())
    }
}

/// Lines of a plain string, indexed once.
pub struct TextLines<'a>(Vec<&'a str>);

impl<'a> TextLines<'a> {
    pub fn new(text: &'a str) -> Self {
        Self(
            text.split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l))
                .collect(),
        )
    }
}

impl Lines for TextLines<'_> {
    fn line(&self, row: u32) -> Cow<'_, str> {
        Cow::Borrowed(self.0.get(row as usize).copied().unwrap_or(""))
    }
}

fn unit_len(c: char, enc: Encoding) -> u32 {
    match enc {
        Encoding::Utf8 => c.len_utf8() as u32,
        Encoding::Utf16 => c.len_utf16() as u32,
        Encoding::Utf32 => 1,
    }
}

/// Character column -> server column.
pub fn to_server(lines: &impl Lines, pos: Position, enc: Encoding) -> Position {
    if enc == Encoding::Utf32 {
        return pos;
    }
    let line = lines.line(pos.line);
    let character = line
        .chars()
        .take(pos.character as usize)
        .map(|c| unit_len(c, enc))
        .sum();
    Position {
        line: pos.line,
        character,
    }
}

/// Server column -> character column (clamped to the line; a column inside a
/// character lands after it).
pub fn from_server(lines: &impl Lines, pos: Position, enc: Encoding) -> Position {
    if enc == Encoding::Utf32 {
        return pos;
    }
    let line = lines.line(pos.line);
    let (mut units, mut chars) = (0u32, 0u32);
    for c in line.chars() {
        if units >= pos.character {
            break;
        }
        units += unit_len(c, enc);
        chars += 1;
    }
    Position {
        line: pos.line,
        character: chars,
    }
}

pub fn range_to_server(lines: &impl Lines, r: Range, enc: Encoding) -> Range {
    Range {
        start: to_server(lines, r.start, enc),
        end: to_server(lines, r.end, enc),
    }
}

pub fn range_from_server(lines: &impl Lines, r: Range, enc: Encoding) -> Range {
    Range {
        start: from_server(lines, r.start, enc),
        end: from_server(lines, r.end, enc),
    }
}

/// Byte offset in a rope -> server position.
pub fn offset_to_server(rope: &Rope, offset: usize, enc: Encoding) -> Position {
    to_server(rope, rope.offset_to_position(offset), enc)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "fn main() {\n    let s = \"čšž😀x\";\r\n}\n";

    #[test]
    fn utf16_round_trip_through_multibyte_and_astral_chars() {
        let lines = TextLines::new(TEXT);
        // `x` is character 22 on line 1: `    let s = "` (13) + `čšž` (3) + `😀` (1) + … = 17, then x.
        let x_chars = Position::new(1, 17);
        let x_utf16 = to_server(&lines, x_chars, Encoding::Utf16);
        assert_eq!(x_utf16, Position::new(1, 18), "😀 is two UTF-16 units");
        assert_eq!(from_server(&lines, x_utf16, Encoding::Utf16), x_chars);
        let x_utf8 = to_server(&lines, x_chars, Encoding::Utf8);
        assert_eq!(x_utf8, Position::new(1, 13 + 6 + 4));
        assert_eq!(from_server(&lines, x_utf8, Encoding::Utf8), x_chars);
        assert_eq!(to_server(&lines, x_chars, Encoding::Utf32), x_chars);
    }

    #[test]
    fn rope_and_text_lines_agree_and_columns_clamp() {
        let rope = Rope::from(TEXT);
        let p = Position::new(1, 17);
        assert_eq!(
            to_server(&rope, p, Encoding::Utf16),
            to_server(&TextLines::new(TEXT), p, Encoding::Utf16)
        );
        // Past the end of the line: clamps to the line length in characters.
        assert_eq!(
            from_server(&rope, Position::new(0, 99), Encoding::Utf16),
            Position::new(0, 11)
        );
    }
}
