//! SPIKE (branch spike-ghost-rows, throwaway): can GPUI Kit's inline
//! completion open empty rows under a line, for a pi suggestion drawn there
//! (design S1)? See SPIKE-GHOST-ROWS.md.
//!
//! Commands (run with `dev/pidectl run <id>`):
//! - `spike.ghost_show`: cursor to the end of the configured line, show the
//!   suggestion as ghost rows, our overlay paints the replacement over them
//! - `spike.ghost_show_raw`: same, but the overlay only adds the button row
//!   (GPUI Kit's own ghost text stays visible)
//! - `spike.ghost_reshow`: ask GPUI Kit again for the current suggestion
//! - `spike.ghost_accept` / `spike.ghost_reject`
//! - `spike.ghost_info`: log geometry to pide.log
//!
//! Config: `/tmp/pide-spike-ghost.txt`, first line the 1-based buffer line,
//! the rest the replacement lines (default: line 3, two made-up lines).

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{EditorState, RopeExt as _};
use gpui_kit::component::{ActiveTheme, Sizable as _, h_flex};
use gpui_kit::*;
use pide_core::commands::{CommandRegistry, CommandSpec};

use crate::EditorPanel;

pub(crate) struct Ghost {
    path: PathBuf,
    /// 1-based buffer line the suggestion replaces.
    line: usize,
    /// The cursor offset GPUI Kit must see (end of `line`).
    offset: usize,
    new_lines: Vec<String>,
    /// `new_lines` soft-wrapped by us to the editor's width: one ghost row each
    /// (GPUI Kit never wraps ghost lines).
    rows: Vec<String>,
    /// The text length when shown: an edit moves our anchor.
    text_len: usize,
    /// GPUI Kit got our item and (as far as we can tell) still shows it.
    kit_has: bool,
    /// Paint the replacement ourselves over GPUI Kit's ghost rows.
    cover: bool,
    /// What the last render used / the last paint saw (overlay origin, scroll).
    used: Option<(Point<Pixels>, Pixels)>,
    painted: Option<(Point<Pixels>, Pixels)>,
}

thread_local! {
    static GHOST: RefCell<Option<Ghost>> = const { RefCell::new(None) };
}

fn insert_text(g: &Ghost) -> String {
    // Empty first line (nothing after the cursor), one ghost row per new
    // line, one more (blank) for the buttons.
    format!("\n{}\n", g.rows.join("\n"))
}

/// The completion GPUI Kit should show at `offset` in `path`.
pub(crate) fn item_for(path: &Path, offset: usize) -> Option<lsp_types::InlineCompletionItem> {
    GHOST.with_borrow_mut(|g| {
        let g = g.as_mut()?;
        let hit = g.path == path && g.offset == offset;
        g.kit_has |= hit;
        hit.then(|| lsp_types::InlineCompletionItem {
            insert_text: insert_text(g),
            filter_text: None,
            range: None,
            command: None,
            insert_text_format: None,
        })
    })
}

pub(crate) fn active() -> bool {
    GHOST.with_borrow(|g| g.is_some())
}

fn config() -> (usize, Vec<String>) {
    let text = std::fs::read_to_string("/tmp/pide-spike-ghost.txt").unwrap_or_default();
    let mut lines = text.lines();
    let line = lines.next().and_then(|l| l.trim().parse().ok()).unwrap_or(3);
    let rest: Vec<String> = lines.map(str::to_string).collect();
    let rest = if rest.is_empty() {
        vec![
            "The replacement, first line of what pi suggests here.".to_string(),
            "And its second line.".to_string(),
        ]
    } else {
        rest
    };
    (line, rest)
}

pub(crate) fn init(cx: &mut App) {
    CommandRegistry::register(
        cx,
        CommandSpec::new("spike.ghost_show", "Spike: Show Ghost Rows"),
        |window, cx| show(true, window, cx),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("spike.ghost_show_raw", "Spike: Show Ghost Rows (raw)"),
        |window, cx| show(false, window, cx),
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("spike.ghost_reshow", "Spike: Re-show Ghost Rows"),
        |window, cx| {
            if let Some(panel) = crate::active_panel(window, cx) {
                let editor = panel.read(cx).editor.clone();
                trigger(&editor, window, cx);
            }
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("spike.ghost_accept", "Spike: Accept Ghost"),
        |window, cx| {
            if let Some(panel) = crate::active_panel(window, cx) {
                panel.update(cx, |p, cx| accept(p, window, cx));
            }
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("spike.ghost_reject", "Spike: Reject Ghost"),
        |window, cx| {
            if let Some(panel) = crate::active_panel(window, cx) {
                panel.update(cx, |p, cx| reject(p, window, cx));
            }
        },
    );
    CommandRegistry::register(
        cx,
        CommandSpec::new("spike.ghost_info", "Spike: Log Ghost Geometry"),
        |window, cx| {
            let Some(panel) = crate::active_panel(window, cx) else { return };
            let p = panel.read(cx);
            let e = p.editor.read(cx);
            let rope = e.text().clone();
            let (line, _) = GHOST.with_borrow(|g| g.as_ref().map(|g| (g.line, 0))).unwrap_or((3, 0));
            let r = crate::pi_notes::line_range(&rope, line);
            let below = crate::pi_notes::line_range(&rope, line + 1);
            log::warn!(
                "spike-ghost: active={} focused={} cursor={:?} rows={:?} lh={:?} scroll={:?} view={:?} start={:?} end={:?} next_line_start={:?} dirty_text_len={}",
                active(),
                e.focus_handle(cx).is_focused(window),
                e.cursor_position(),
                e.visible_row_range(),
                e.line_height(),
                e.scroll_offset(),
                e.input_bounds(),
                e.range_to_bounds(&(r.start..r.start)),
                e.range_to_bounds(&(r.end..r.end)),
                e.range_to_bounds(&(below.start..below.start)),
                rope.len(),
            );
        },
    );
}

fn show(cover: bool, window: &mut Window, cx: &mut App) {
    let Some(panel) = crate::active_panel(window, cx) else { return };
    let (editor, path) = {
        let p = panel.read(cx);
        (p.editor.clone(), p.path.clone())
    };
    let (line, new_lines) = config();
    let rope = editor.read(cx).text().clone();
    let r = crate::pi_notes::line_range(&rope, line);
    // Wrap the replacement to the editor's width ourselves (monospace).
    let cols = {
        let e = editor.read(cx);
        let start = e.range_to_bounds(&(r.start..r.start));
        let one = e.range_to_bounds(&(r.start..(r.start + 1).min(r.end)));
        match (start, one) {
            (Some(s), Some(o)) if o.size.width > px(0.) => {
                ((e.input_bounds().right() - s.left() - px(14.)) / o.size.width).floor() as usize
            }
            _ => 80,
        }
    };
    let rows = new_lines.iter().flat_map(|l| wrap(l, cols.max(10))).collect();
    GHOST.with_borrow_mut(|g| {
        *g = Some(Ghost {
            path,
            line,
            offset: r.end,
            new_lines,
            rows,
            text_len: rope.len(),
            kit_has: false,
            cover,
            used: None,
            painted: None,
        })
    });
    let pos = rope.offset_to_position(r.end);
    editor.update(cx, |e, cx| e.set_cursor_position(pos, window, cx));
    trigger(&editor, window, cx);
}

/// GPUI Kit asks its provider only after typed input: type nothing.
fn trigger(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) {
    editor.update(cx, |e, cx| {
        e.focus(window, cx);
        e.replace_text_in_range(None, "", window, cx);
    });
}

fn wrap(line: &str, cols: usize) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    for word in line.split(' ') {
        let n = cur.chars().count();
        if n > 0 && n + 1 + word.chars().count() > cols {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    out.push(cur);
    out
}

fn clear(p: &mut EditorPanel, window: &mut Window, cx: &mut Context<EditorPanel>) {
    GHOST.with_borrow_mut(|g| *g = None);
    // Moving the cursor is the public way to drop GPUI Kit's completion.
    p.editor.update(cx, |e, cx| {
        let pos = e.cursor_position();
        e.set_cursor_position(pos, window, cx);
    });
    cx.notify();
}

fn accept(p: &mut EditorPanel, window: &mut Window, cx: &mut Context<EditorPanel>) {
    let Some((line, text)) =
        GHOST.with_borrow(|g| g.as_ref().map(|g| (g.line, g.new_lines.join("\n"))))
    else {
        return;
    };
    clear(p, window, cx);
    p.editor.update(cx, |e, cx| {
        let rope = e.text().clone();
        let r = crate::pi_notes::line_range(&rope, line);
        let edit = lsp_types::TextEdit {
            range: lsp_types::Range::new(rope.offset_to_position(r.start), rope.offset_to_position(r.end)),
            new_text: text,
        };
        e.apply_lsp_edits(&vec![edit], window, cx);
    });
}

fn reject(p: &mut EditorPanel, window: &mut Window, cx: &mut Context<EditorPanel>) {
    clear(p, window, cx);
}

/// Tab = accept, Esc = reject, while a suggestion is up. Returns true if handled.
pub(crate) fn on_key(
    p: &mut EditorPanel,
    key: &str,
    window: &mut Window,
    cx: &mut Context<EditorPanel>,
) -> bool {
    if !active() {
        return false;
    }
    match key {
        "tab" => accept(p, window, cx),
        "escape" => reject(p, window, cx),
        _ => return false,
    }
    true
}

/// The overlay: a red tint over the old line, the replacement painted over
/// GPUI Kit's ghost rows (when `cover`), and Accept/Reject in the last row.
pub(crate) fn render(p: &mut EditorPanel, window: &mut Window, cx: &mut Context<EditorPanel>) -> Option<AnyElement> {
    let (line, new_lines, cover, painted) = GHOST.with_borrow(|g| {
        let g = g.as_ref().filter(|g| g.path == p.path)?;
        Some((g.line, g.rows.clone(), g.cover, g.painted))
    })?;
    let editor = p.editor.read(cx);
    let rope = editor.text().clone();
    // GPUI Kit drops its completion on blur, any cursor move, any edit, a
    // click and Esc, and tells nobody. Mirror those rules; when they hold
    // again (focus back, cursor back at the line's end), ask it again.
    let focused = editor.focus_handle(cx).is_focused(window);
    let cursor = editor.cursor();
    let retrigger = GHOST.with_borrow_mut(|g| {
        let g = g.as_mut()?;
        if g.text_len != rope.len() {
            // Text changed: re-anchor at the line's (new) end.
            g.text_len = rope.len();
            g.offset = crate::pi_notes::line_range(&rope, g.line).end;
            g.kit_has = false;
        }
        if !focused || cursor != g.offset {
            g.kit_has = false;
            return Some(false);
        }
        // Asking re-renders us before the answer comes: count it as shown
        // already, or we ask again every frame and the debounce never ends.
        let ask = !g.kit_has;
        g.kit_has = true;
        Some(ask)
    })?;
    if retrigger {
        let editor = p.editor.clone();
        window.defer(cx, move |window, cx| trigger(&editor, window, cx));
    }
    let editor = p.editor.read(cx);
    if !focused || cursor != GHOST.with_borrow(|g| g.as_ref().map_or(0, |g| g.offset)) {
        return None;
    }
    let rows = editor.visible_row_range()?;
    if !rows.contains(&(line - 1)) {
        return None;
    }
    let r = crate::pi_notes::line_range(&rope, line);
    let start_b = editor.range_to_bounds(&(r.start..r.start))?;
    let end_b = editor.range_to_bounds(&(r.end..r.end))?;
    let lh = editor.line_height()?;
    let scroll_now = editor.scroll_offset().y;
    let view = editor.input_bounds();
    GHOST.with_borrow_mut(|g| {
        if let Some(g) = g.as_mut() {
            g.used = g.painted.map(|(o, _)| (o, scroll_now));
        }
    });
    let (origin, dy) = match painted {
        Some((o, s)) => (o, scroll_now - s),
        None => (view.origin, px(0.)),
    };
    let _ = dy; // range_to_bounds is from the last paint; scroll since then is ignored in the spike
    let left = start_b.left() - origin.x;
    let width = view.right() - start_b.left() - px(14.);
    let old_top = start_b.top() - origin.y;
    let old_h = end_b.bottom() - start_b.top();
    let ghost_top = end_b.bottom() - origin.y;

    let theme = cx.theme();
    let bg = theme.background;
    let red = hsla(0., 0.7, 0.5, 0.18);
    let green = hsla(0.36, 0.6, 0.45, 0.20);
    let mono = theme.mono_font_family.clone();
    let mono_size = theme.mono_font_size;
    let fg = theme.foreground;

    let mut kids: Vec<AnyElement> = vec![
        div()
            .absolute()
            .top(old_top)
            .left(left)
            .w(width)
            .h(old_h)
            .bg(red)
            .into_any_element(),
    ];
    if cover {
        for (i, l) in new_lines.iter().enumerate() {
            kids.push(
                div()
                    .absolute()
                    .top(ghost_top + lh * i as f32)
                    .left(left)
                    .w(width)
                    .h(lh)
                    .bg(bg)
                    .child(
                        div()
                            .size_full()
                            .bg(green)
                            .font_family(mono.clone())
                            .text_size(mono_size)
                            .text_color(fg)
                            .line_height(lh)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(l.clone()),
                    )
                    .into_any_element(),
            );
        }
    }
    let n = new_lines.len() as f32;
    let weak = cx.weak_entity();
    let weak2 = cx.weak_entity();
    kids.push(
        h_flex()
            .absolute()
            .top(ghost_top + lh * n)
            .left(left)
            .h(lh)
            .gap_1()
            .child(
                Button::new("spike-accept")
                    .xsmall()
                    .primary()
                    .label("Accept ⇥")
                    .on_click(move |_, window, cx| {
                        let _ = weak.update(cx, |p, cx| accept(p, window, cx));
                    }),
            )
            .child(
                Button::new("spike-reject")
                    .xsmall()
                    .ghost()
                    .label("Reject ⎋")
                    .on_click(move |_, window, cx| {
                        let _ = weak2.update(cx, |p, cx| reject(p, window, cx));
                    }),
            )
            .into_any_element(),
    );

    let editor_entity = p.editor.clone();
    let weak = cx.weak_entity();
    let recorder = canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| {
            let scroll = editor_entity.read(cx).scroll_offset().y;
            let stale = GHOST.with_borrow_mut(|g| {
                let Some(g) = g.as_mut() else { return false };
                g.painted = Some((bounds.origin, scroll));
                g.used != g.painted
            });
            if stale {
                let weak = weak.clone();
                window.on_next_frame(move |_, cx| {
                    let _ = weak.update(cx, |_, cx| cx.notify());
                });
            }
        },
    )
    .absolute()
    .size_full();

    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .overflow_hidden()
            .child(recorder)
            .children(kids)
            .into_any_element(),
    )
}
