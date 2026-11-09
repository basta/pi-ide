# Gaps and deviations

Things GPUI Kit or pi can't do the way the spec assumed, and places the implementation knowingly differs.
Each needs the user's call before anyone works around it.

| Area | Gap / deviation | Current handling | Since |
| --- | --- | --- | --- |
| Line endings | A file mixing LF and CRLF can't be saved byte-identical: the editor works on `\n` text, and we restore one ending per file. | The majority ending wins on save; a notice bar says so when the file is opened. Pure LF and pure CRLF files round-trip exactly. | M1 |
| Panel trait | Spec's `Panel::title` would clash with GPUI Kit's dock `Panel::title`. | Named `title_text`. | M1 |
| Fuzzy matching | Spec says `nucleo`. | Uses `nucleo-matcher` (the matching core of the same project); the full `nucleo` crate adds a threaded matcher we don't need for a few dozen commands. Revisit for quick-open (M4). | M1 |
| Close dirty tab | Not specified. | Closing a tab with unsaved edits keeps the buffer in the workspace; reopening the file brings the edits back. Quitting or closing the window asks Save / Don't Save / Cancel. | M1 |
| Before-snapshots | pi doesn't report file edits. Reading the file at `tool_execution_start` loses the race: with parallel calls pi writes 3–7 ms later (seen in the app on 2026-11-09: empty diffs). | Snapshot is taken as soon as the call's `path` has streamed in `toolcall_delta` (pi runs a message's tools only after the whole message has streamed, ~1 s later). Falls back to a read at the start, with a warning in the log. `edit` results also carry an exact `details.patch` we could cross-check against. | M2 |
| Files changed by `bash` | Only `edit` and `write` are attributed. A `bash` call that edits files (sed, mv, cargo fmt) isn't in the Changes list. | The file watcher still reloads them in the editor. Candidate: attribute changes seen during a running `bash` call to its session. | M2 |
| Session history | pi sessions are trees (branches). | A resumed session shows its messages in file order, branches flattened. | M2 |
| Diff tabs after restart | Diffs compare against in-memory before-snapshots. | A diff tab restored from the saved layout says it's no longer available. | M2 |
| Context chip in the composer | The editor has no selection-changed event. | The chat re-checks the editor context every 500 ms and redraws only when it changed; the chip shows `main.rs:12` or `main.rs 5–7`. | M2 |
| Inlay hints | GPUI Kit's editor can't draw virtual text inside a line (its inline tokens are real characters in the document), and we don't write editor rendering code. | No inlay hints (user's call, 2026-11-10). Revisit if GPUI Kit adds virtual text. | M3 |
| rust-analyzer type errors | rust-analyzer reports type mismatches through `cargo check`, which it runs on save. | Errors from `cargo check` appear after saving, not while typing; syntax errors appear at once. | M3 |
| Hover code blocks | GPUI Kit's markdown view draws a language label on each fenced code block in a hover, and it overlaps the code's right edge. | Cosmetic; left as is. | M3 |
| Menu shortcuts for editor-only commands | GPUI looks up a menu item's keystroke in the focused context; when the editor isn't focused the editor-only keys (Cmd+B, Shift+F6…) don't show in the Code menu. | The keys still work in the editor. | M3 |
| Signature help placement | GPUI Kit's caret-anchored popup is crate-private; ours uses the caret's rendered bounds from the previous frame. | Fine while typing; it hides when the caret leaves the line, on Escape and on focus loss. | M3 |
| PDF rendering untested | Claude couldn't run the downloaded pdfium library during the night (the session's safety check blocks executing a native binary fetched from the internet). | The PDF panel compiles; its "install pdfium" state and file routing were tested. Rendering, zoom, search highlights and the 256 MB cache need a first run by the user: open a PDF, click Install pdfium. Alternative if pdfium is unwanted: Apple's PDFKit on macOS (no download), pdfium on Linux. | M5 |
