# Spike: ghost rows for pi suggestions (design S1)

Throwaway branch `spike-ghost-rows`. Question: can GPUI Kit 0.9's inline-completion ghost text open empty rows between lines, so pide can draw a pi suggestion's replacement there with Accept/Reject?

**Verdict: viable with caveats.** It works for **one suggestion at a time, while the cursor is parked at the end of the suggested line**. It doesn't work for suggestions that stay on screen while you move around. The ghost rows open in the right place, wrapped lines and the gutter included. Tab/Esc can be taken over cleanly, and an overlay can sit in the gap. But GPUI Kit drops the completion on any cursor move, edit, click or blur, and tells nobody. So S1 built on this is a "review this suggestion now" mode. For persistent or multiple suggestions we need a real block-decoration feature in GPUI Kit (end of this file).

## What the hack does

- `crates/editor/src/spike_ghost.rs`: commands `spike.ghost_show`, `spike.ghost_show_raw`, `spike.ghost_reshow`, `spike.ghost_accept`, `spike.ghost_reject`, `spike.ghost_info` (run them with `dev/pidectl run …`). The line and replacement come from `/tmp/pide-spike-ghost.txt`: the first line is the 1-based buffer line, the rest are the replacement lines.
- `lsp.rs`: `Providers::inline_completion` returns our item when the cursor offset is the end of that line. The debounce drops to 20 ms while a suggestion is active.
- **Triggering without typing:** GPUI Kit asks its provider only from `on_text_typed`, which is skipped for silent edits. There is no public setter for the item: `extras.inline_completion` is `pub(crate)`. The route that works is `EntityInputHandler::replace_text_in_range(None, "", …)`, an empty "typed" edit at the cursor. Verified that it adds no undo step and doesn't mark the tab dirty. It still relies on an implementation detail.
- `insert_text = "\n" + rows.join("\n") + "\n"`. The empty first line means nothing is painted after the cursor. Each row becomes one ghost row, plus one blank row for the buttons.
- Overlay (like `pi_float.rs`): a red tint over the old line, the replacement in our own colours over the ghost rows ("cover" mode), and Accept ⇥ / Reject ⎋ buttons in the last row.

## Answers

### 1. Soft-wrapped lines
- **Yes.** The ghost rows go after the **last visual row** of the wrapped buffer line. `paint` adds `ghost_lines_height` after the whole `LineLayout` of `current_row`, which is a buffer line. Line numbers move down with them, and gutter decorations are shifted too (`layout_gutter_decorations(ghost_rows)`). Shots 0, 1, 5.
- **Ghost rows themselves never wrap** (`shape_line`, no wrap width). A long replacement line runs off to the right. Fix: pide wraps the replacement itself (monospace: columns = width / char width from `range_to_bounds`) and asks for one ghost row per wrapped row. Done in the spike, shot 1. If the editor width changes, the rows have to be recomputed.
- The ghost x ignores horizontal scroll. That doesn't matter with soft wrap on.

### 2. Does it stay put? What clears it?
GPUI Kit clears it (`clear_inline_completion`) on:
- **any cursor move** (`move_to` / `select_to`: arrows, Home/End, Go to Line, `set_cursor_position`)
- **any edit**, typed or not (`replace_text_in_ranges`): typing, LSP edits, `apply_lsp_edits`, an outside reload through `minimal::set_text`
- **mouse down**
- **blur**: focus to the PDF pane, the pi panel, a dialog
- **Escape**, and `add_selection` (multi-cursor)

It is also **not drawn** (but kept) while the editor isn't focused or the cursor row is scrolled out of view.

What **survives**: scrolling (it hides while the line is out of view and comes back), save (Cmd+S), and partial scroll with the line's first rows above the view (shot 5).

**pide can't observe any of these clears.** There is no event, and `has_inline_completion` is `pub(crate)`. Shot 7 shows the overlay left stale before I added tracking. The spike mirrors the rules instead: the overlay shows only while the editor is focused, the cursor is at the anchor, and the text length is unchanged since the last ask. When those hold again it re-asks (one empty edit plus 20 ms):
- Focus return (Esc from the pi composer) re-shows it. Shot 3, column 5.
- An outside change on disk: the overlay re-anchors to the line's new end and re-shows. Shot 3, column 6.
- Cursor back by ←/→: re-shows.
- Cursor back by **End**: the re-ask **lost a race**. The overlay came back without the gap (shot 3, column 3). An explicit `spike.ghost_reshow` afterwards works. I didn't find the cause. The guard against asking every frame was needed: each ask re-renders, and re-asking from render would restart the debounce forever.

So yes, it can be re-shown cheaply: one empty edit and about 20 ms. But it is guesswork against GPUI Kit's private rules.

### 3. Tab / Esc, styling, overlay placement
- **Tab = our Accept:** yes, with `.capture_action::<IndentInline>` on the editor panel's root plus `cx.stop_propagation()`. We apply the real edit (`apply_lsp_edits`, replacing the whole line), and the ghost text is never inserted. One Cmd+Z undoes it. **Esc = Reject** the same way, with `capture_action::<Escape>` (shot 4).
- `capture_key_down` does **not** work for this. GPUI dispatches key bindings (Tab → `IndentInline`) before key-down listeners, so GPUI Kit had already inserted the ghost text into the buffer.
- **Ghost colour doesn't matter:** we paint our own rows over the ghost rows with an opaque background (cover mode, shot 1). GPUI Kit's own ghost text (shot 2, `muted_foreground` at 0.5) can't be styled. Its only use would be a fallback with no overlay.
- **Overlay placement is reliable** on the same basis as `pi_float`. The gap starts at `range_to_bounds(line_end).bottom()`, which is the line's last wrapped row, and is `rows × line_height` tall. `visible_row_range()` is in buffer lines and gates it: `range_to_bounds` returns the first laid-out line for lines above the view.
- The geometry is from the last paint, so the overlay needs one more frame after a scroll (the same recorder/`on_next_frame` trick as `pi_float`).
- Clicking our buttons works: they sit above the editor and get the click.

### 4. Other blockers
- **One at a time:** there is a single completion item and a single cursor. Two suggestions can't be shown at once.
- **Cursor-bound:** the gap exists only while the cursor sits at the end of that exact line. Reading the rest of the file with the arrow keys or clicking makes the suggestion disappear (it comes back when the cursor returns). This is the main UX limit for S1.
- **Nothing below the gap knows about it:**
  - `range_to_bounds` for later lines ignores the ghost height (`spike.ghost_info`: line 5 is reported at y=223 while painted at y=283).
  - Mouse hit-testing ignores it too: a click on a ghost row put the cursor on line 5 col 8, the line that would be there without the gap. That click also clears the gap, so the text jumps up under the mouse.
  - `pi_float` lenses, cards and anything else positioned below the suggestion would be off by the gap's height.
- **Near the bottom of the view the rows open below the fold.** The editor doesn't scroll to show them (shot 6), so pide would have to scroll itself.
- **Performance:** no concern. It shapes a few lines per frame. Nothing new showed up in the slow-frame log beyond the usual debug-build noise.
- **Scroll jumps:** none from the ghost rows. `set_cursor_position` to the line end does scroll the cursor into view, as any jump does.

## What a GPUI Kit block-decoration feature would need

Something like Zed's block map, owned by the editor and independent of the cursor and focus:

1. `set_block_decorations(Vec<Block>)` with `Block { id, anchor: buffer offset (tracks edits), placement: below the anchor line's last wrapped row (or above), height: rows or px, render: Fn(&BlockContext) -> AnyElement }`. Several at once.
2. Space reserved in layout: scroll height, `visible_range`, line numbers and gutter (already done for ghost lines), wrap width passed to the block.
3. Every geometry API accounts for blocks: `range_to_bounds`, hit-testing (`index_for_position`), scroll-to-cursor clearance, and a `block_bounds(id)` getter or the element rendered in place, so nothing has to be overlaid.
4. Persistent until removed: cleared only by pide, never by cursor moves, edits, blur or Escape. The anchor moves with edits.
5. Clicks inside a block go to the block's element, never to the text under it.

Until then, inline completion is enough for a single-suggestion, cursor-parked prototype of S1.

## Shots

On the `wip-shots` branch: `shots/spike-ghost-0-window.png` … `spike-ghost-7-overlay-stale-before-tracking.png` (captions in `shots/index.json`).
