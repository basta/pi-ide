//! An open file. The workspace owns buffers and is the source of truth for
//! their contents; editors display them and push edits back.
//!
//! Round-trip rule (M1): a saved file is byte-identical to what was loaded,
//! except for the lines the user edited. So we remember the line ending and BOM
//! from disk, hand the editor plain `\n` text, and restore both on save.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use gpui_kit::SharedString;
use gpui_kit::component::input::Rope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::Crlf => "CRLF",
        }
    }
}

const BOM: &[u8] = b"\xEF\xBB\xBF";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// Text with `\n` line endings, without BOM.
    pub text: String,
    pub line_ending: LineEnding,
    pub bom: bool,
    /// The file mixed LF and CRLF; saving will normalise to `line_ending`.
    pub mixed: bool,
}

/// Decode file bytes. `Err` = not valid UTF-8 (open read-only, never save).
pub fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    let (bom, body) = match bytes.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    let text = std::str::from_utf8(body).map_err(|e| {
        format!(
            "not valid UTF-8 (first bad byte at offset {})",
            e.valid_up_to() + if bom { 3 } else { 0 }
        )
    })?;
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count();
    let (line_ending, mixed) = match (crlf, lf - crlf) {
        (0, _) => (LineEnding::Lf, false),
        (_, 0) => (LineEnding::Crlf, false),
        (c, l) => (
            if c >= l {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            },
            true,
        ),
    };
    let text = if crlf > 0 {
        text.replace("\r\n", "\n")
    } else {
        text.to_string()
    };
    Ok(Decoded {
        text,
        line_ending,
        bom,
        mixed,
    })
}

/// Encode editor text (`\n` endings) back to file bytes.
pub fn encode(text: &str, line_ending: LineEnding, bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + if bom { 3 } else { 0 });
    if bom {
        out.extend_from_slice(BOM);
    }
    match line_ending {
        LineEnding::Lf => out.extend_from_slice(text.as_bytes()),
        LineEnding::Crlf => {
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    out.extend_from_slice(b"\r\n");
                }
                out.extend_from_slice(line.as_bytes());
            }
        }
    }
    out
}

pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

pub struct Buffer {
    pub path: PathBuf,
    pub(crate) text: Rope,
    /// The text as last loaded or saved: editing back to it makes the buffer clean.
    pub(crate) saved_text: Rope,
    pub line_ending: LineEnding,
    pub bom: bool,
    pub(crate) dirty: bool,
    /// Why the buffer can't be edited (e.g. not UTF-8). `None` = editable.
    pub read_only: Option<SharedString>,
    /// A one-line notice to show over the editor (mixed endings, …).
    pub notice: Option<SharedString>,
    /// Changed on disk while it had unsaved edits.
    pub(crate) conflict: bool,
    /// Hash of the bytes last read from or written to disk.
    pub(crate) disk_hash: u64,
    /// Bumped whenever the text is replaced from outside the editor (reload).
    pub(crate) revision: u64,
}

impl Buffer {
    pub(crate) fn from_bytes(path: PathBuf, bytes: &[u8]) -> Self {
        let disk_hash = hash_bytes(bytes);
        match decode(bytes) {
            Ok(d) => Self {
                path,
                text: Rope::from(d.text.as_str()),
                saved_text: Rope::from(d.text.as_str()),
                line_ending: d.line_ending,
                bom: d.bom,
                dirty: false,
                read_only: None,
                notice: d.mixed.then(|| {
                    format!(
                        "Mixed line endings; saving will use {} throughout.",
                        d.line_ending.label()
                    )
                    .into()
                }),
                conflict: false,
                disk_hash,
                revision: 0,
            },
            Err(reason) => Self {
                path,
                text: Rope::from(String::from_utf8_lossy(bytes).as_ref()),
                saved_text: Rope::from(String::from_utf8_lossy(bytes).as_ref()),
                line_ending: LineEnding::Lf,
                bom: false,
                dirty: false,
                read_only: Some(format!("Read-only: {reason}.").into()),
                notice: None,
                conflict: false,
                disk_hash,
                revision: 0,
            },
        }
    }

    pub fn text(&self) -> &Rope {
        &self.text
    }
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
    pub fn has_conflict(&self) -> bool {
        self.conflict
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn file_name(&self) -> SharedString {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
            .into()
    }
    pub(crate) fn encoded(&self) -> Vec<u8> {
        encode(&self.text.to_string(), self.line_ending, self.bom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(bytes: &[u8]) {
        let d = decode(bytes).unwrap();
        assert!(!d.text.contains("\r\n"));
        assert_eq!(encode(&d.text, d.line_ending, d.bom), bytes);
    }

    #[test]
    fn lf_roundtrips() {
        roundtrip(b"fn main() {}\nlet x = 1;\n");
        roundtrip(b"no trailing newline");
        roundtrip(b"");
    }

    #[test]
    fn crlf_roundtrips() {
        roundtrip(b"a\r\nb\r\n");
        roundtrip(b"a\r\nb");
        let d = decode(b"a\r\nb\r\n").unwrap();
        assert_eq!(d.line_ending, LineEnding::Crlf);
        assert_eq!(d.text, "a\nb\n");
    }

    #[test]
    fn non_ascii_and_bom_roundtrip() {
        roundtrip("příliš žluťoučký kůň 🐎\n".as_bytes());
        roundtrip(b"\xEF\xBB\xBFwith bom\r\n");
    }

    #[test]
    fn crlf_edit_only_changes_edited_line() {
        let original = b"one\r\ntwo\r\nthree\r\n";
        let d = decode(original).unwrap();
        let edited = d.text.replace("two", "TWO");
        assert_eq!(
            encode(&edited, d.line_ending, d.bom),
            b"one\r\nTWO\r\nthree\r\n"
        );
    }

    #[test]
    fn lone_cr_is_preserved() {
        roundtrip(b"a\rb\n");
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        assert!(decode(b"ok \xFF\xFE bad").is_err());
    }

    #[test]
    fn mixed_endings_are_flagged() {
        let d = decode(b"a\r\nb\nc\r\n").unwrap();
        assert!(d.mixed);
        assert_eq!(d.line_ending, LineEnding::Crlf);
    }
}
