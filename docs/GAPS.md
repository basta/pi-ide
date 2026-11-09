# Gaps and deviations

Things GPUI Kit or pi can't do the way the spec assumed, and places the implementation knowingly differs.
Each needs the user's call before anyone works around it.

| Area | Gap / deviation | Current handling | Since |
| --- | --- | --- | --- |
| Line endings | A file mixing LF and CRLF can't be saved byte-identical: the editor works on `\n` text, and we restore one ending per file. | The majority ending wins on save; a notice bar says so when the file is opened. Pure LF and pure CRLF files round-trip exactly. | M1 |
| Reload after external change | `EditorState::set_value` resets undo history. | Reloading a clean buffer from disk loses its undo history. `replace_all` keeps history but emits change events (would mark the buffer dirty); revisit if it matters. | M1 |
| Panel trait | Spec's `Panel::title` would clash with GPUI Kit's dock `Panel::title`. | Named `title_text`. | M1 |
| Fuzzy matching | Spec says `nucleo`. | Uses `nucleo-matcher` (the matching core of the same project); the full `nucleo` crate adds a threaded matcher we don't need for a few dozen commands. Revisit for quick-open (M4). | M1 |
| Close dirty tab | Not specified. | Closing a tab with unsaved edits keeps the buffer in the workspace; reopening the file brings the edits back. There is no "save changes?" prompt yet. | M1 |
| Before-snapshots | pi doesn't report file edits; "before" is read the moment pi announces an `edit`/`write` call. Measured 6/6 correct (pi announces before it writes), but it is a race in principle. | Snapshot is taken on the runtime thread as the line arrives. If it ever loses, the diff is empty/wrong for that call; `edit` results also carry an exact `details.patch` we could fall back to. | M2 |
| Files changed by `bash` | Only `edit` and `write` are attributed. A `bash` call that edits files (sed, mv, cargo fmt) isn't in the Changes list. | The file watcher still reloads them in the editor. Candidate: attribute changes seen during a running `bash` call to its session. | M2 |
| Session history | pi sessions are trees (branches). | A resumed session shows its messages in file order, branches flattened. | M2 |
| Diff tabs after restart | Diffs compare against in-memory before-snapshots. | A diff tab restored from the saved layout says it's no longer available. | M2 |
| Context chip in the composer | The editor doesn't notify the chat when the selection moves. | The composer chip names only the file; the exact lines appear on the sent message. | M2 |
