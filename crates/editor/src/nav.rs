//! Navigate Back / Forward (Cmd+[ / Cmd+], JetBrains): where you were before
//! opening or jumping to another place (Go to Declaration in another file,
//! Find Usages, Go to File, Recent Files…).

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::*;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Loc {
    pub path: PathBuf,
    pub line: u32,
    pub character: u32,
}

#[derive(Default)]
pub(crate) struct History {
    back: Vec<Loc>,
    forward: Vec<Loc>,
}

const KEEP: usize = 100;

impl History {
    /// About to leave `here` for somewhere new.
    pub(crate) fn leave(&mut self, here: Loc) {
        if self.back.last() != Some(&here) {
            self.back.push(here);
            if self.back.len() > KEEP {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    /// Step back from `here`: where to go.
    pub(crate) fn back(&mut self, here: Option<Loc>) -> Option<Loc> {
        let to = self.back.pop()?;
        if let Some(h) = here {
            self.forward.push(h);
        }
        Some(to)
    }

    pub(crate) fn forward(&mut self, here: Option<Loc>) -> Option<Loc> {
        let to = self.forward.pop()?;
        if let Some(h) = here {
            self.back.push(h);
        }
        Some(to)
    }
}

#[derive(Default)]
struct Histories(HashMap<WindowId, History>);
impl Global for Histories {}

/// Where the cursor is now (the active editor tab).
fn here(window: &Window, cx: &App) -> Option<Loc> {
    let panel = crate::active_panel(window, cx)?;
    let p = panel.read(cx);
    let pos = p.editor.read(cx).cursor_position();
    Some(Loc {
        path: p.path.clone(),
        line: pos.line,
        character: pos.character,
    })
}

/// Record the current place before a jump (called by `open_path` / `open_path_at`).
pub(crate) fn record(window: &Window, cx: &mut App) {
    if let Some(h) = here(window, cx) {
        let id = window.window_handle().window_id();
        cx.default_global::<Histories>()
            .0
            .entry(id)
            .or_default()
            .leave(h);
    }
}

pub(crate) fn go(forward: bool, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let now = here(window, cx);
    let to = {
        let h = cx.default_global::<Histories>().0.entry(id).or_default();
        if forward { h.forward(now) } else { h.back(now) }
    };
    if let Some(l) = to {
        crate::open_path_at_unrecorded(l.path, l.line, l.character, window, cx);
    }
}

pub(crate) fn forget(id: WindowId, cx: &mut App) {
    if cx.has_global::<Histories>() {
        cx.global_mut::<Histories>().0.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(p: &str, line: u32) -> Loc {
        Loc {
            path: p.into(),
            line,
            character: 0,
        }
    }

    #[::core::prelude::v1::test]
    fn back_and_forward() {
        let mut h = History::default();
        h.leave(l("a", 1)); // a:1 -> b
        h.leave(l("b", 5)); // b:5 -> c
        assert_eq!(h.back(Some(l("c", 0))), Some(l("b", 5)));
        assert_eq!(h.back(Some(l("b", 5))), Some(l("a", 1)));
        assert_eq!(h.back(Some(l("a", 1))), None);
        assert_eq!(h.forward(Some(l("a", 1))), Some(l("b", 5)));
        assert_eq!(h.forward(Some(l("b", 5))), Some(l("c", 0)));
        // A new jump drops the forward list.
        h.back(Some(l("c", 0)));
        h.leave(l("b", 5));
        assert_eq!(h.forward(None), None);
    }
}
