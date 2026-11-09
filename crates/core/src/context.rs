//! "Send editor context" (spec: Pi agent integration). pi has no way to be told
//! what you're looking at, so the chat prepends a short block to each prompt:
//! active file, selection range, and the selected text if it's under 4 KB.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::*;

/// Largest selection included verbatim.
pub const MAX_SELECTION_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct EditorContext {
    /// Relative to the project root.
    pub path: PathBuf,
    /// 1-based (line, column), inclusive start / exclusive end. Equal = just a cursor.
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub selected: Option<String>,
    pub selected_bytes: usize,
    pub language: &'static str,
}

impl EditorContext {
    /// The chip shown for it: `buffer.rs 41–44`, `buffer.rs:12`.
    pub fn label(&self) -> String {
        let name = self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if self.start == self.end {
            format!("{name}:{}", self.start.0)
        } else if self.start.0 == self.end.0 {
            format!("{name}:{}", self.start.0)
        } else {
            format!("{name} {}–{}", self.start.0, self.end.0)
        }
    }

    /// The block prepended to the prompt.
    pub fn block(&self) -> String {
        let mut b = format!("<editor-context>\nfile: {}\n", self.path.display());
        if self.start == self.end {
            b.push_str(&format!("cursor: {}:{}\n", self.start.0, self.start.1));
        } else {
            b.push_str(&format!("selection: {}:{}-{}:{}\n", self.start.0, self.start.1, self.end.0, self.end.1));
            match &self.selected {
                Some(text) => b.push_str(&format!("```{}\n{}\n```\n", self.language, text.trim_end_matches('\n'))),
                None => b.push_str(&format!("(selected text not included: {} bytes, over 4 KB)\n", self.selected_bytes)),
            }
        }
        b.push_str("</editor-context>\n\n");
        b
    }
}

/// Split a sent prompt back into (context chip label, the user's own text) for display.
pub fn split_prompt(text: &str) -> (Option<String>, &str) {
    let Some(rest) = text.strip_prefix("<editor-context>\n") else { return (None, text) };
    let Some(end) = rest.find("</editor-context>") else { return (None, text) };
    let block = &rest[..end];
    let user = rest[end + "</editor-context>".len()..].trim_start_matches('\n');
    let file = block.lines().find_map(|l| l.strip_prefix("file: ")).unwrap_or("?");
    let name = std::path::Path::new(file).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let label = if let Some(sel) = block.lines().find_map(|l| l.strip_prefix("selection: ")) {
        let (a, b) = sel.split_once('-').unwrap_or((sel, sel));
        let line = |p: &str| p.split(':').next().unwrap_or("").to_string();
        if line(a) == line(b) { format!("{name}:{}", line(a)) } else { format!("{name} {}–{}", line(a), line(b)) }
    } else if let Some(cur) = block.lines().find_map(|l| l.strip_prefix("cursor: ")) {
        format!("{name}:{}", cur.split(':').next().unwrap_or(""))
    } else {
        name
    };
    (Some(label), user)
}

/// Installed by the editor crate: the file and selection of the active editor
/// in a window, if any.
pub struct EditorContextSource(pub Rc<dyn Fn(&Window, &App) -> Option<EditorContext>>);
impl Global for EditorContextSource {}

pub fn current(window: &Window, cx: &App) -> Option<EditorContext> {
    cx.try_global::<EditorContextSource>().and_then(|s| (s.0)(window, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(selected: Option<&str>) -> EditorContext {
        EditorContext {
            path: "crates/core/src/buffer.rs".into(),
            start: (41, 5),
            end: (44, 6),
            selected: selected.map(str::to_string),
            selected_bytes: selected.map_or(9000, str::len),
            language: "rust",
        }
    }

    #[::core::prelude::v1::test]
    fn block_round_trips_to_a_chip() {
        let sent = format!("{}Why strip the BOM first?", ctx(Some("pub fn decode(")).block());
        assert!(sent.contains("```rust\npub fn decode(\n```"));
        let (label, user) = split_prompt(&sent);
        assert_eq!(label.as_deref(), Some("buffer.rs 41–44"));
        assert_eq!(user, "Why strip the BOM first?");
        assert_eq!(split_prompt("plain"), (None, "plain"));
    }

    #[::core::prelude::v1::test]
    fn large_selections_are_described_not_included() {
        let b = ctx(None).block();
        assert!(b.contains("not included: 9000 bytes"));
        assert!(!b.contains("```"));
    }
}
