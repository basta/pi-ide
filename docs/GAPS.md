# Gaps and deviations

Things GPUI Kit or pi can't do the way the spec assumed, and places the implementation knowingly differs.
Each needs the user's call before anyone works around it.

| Area | Gap / deviation | Current handling | Since |
| --- | --- | --- | --- |
| Line endings | A file mixing LF and CRLF can't be saved byte-identical: the editor works on `\n` text, and we restore one ending per file. | The majority ending wins on save; a notice bar says so when the file is opened. Pure LF and pure CRLF files round-trip exactly. | M1 |
| Reload after external change | `EditorState::set_value` resets undo history. | Reloading a clean buffer from disk loses its undo history. `replace_all` keeps history but emits change events (would mark the buffer dirty); revisit if it matters. | M1 |
| pi extension dialogs | `select`/`confirm`/`input`/`editor` requests block pi until answered; their UI is M2. | Answered `cancelled: true` automatically, with a notice. | M1 |
| Steering / follow-ups | Sending while pi works needs `steer`/`follow_up` (M2). | The chat input won't send while pi is busy. | M1 |
| Panel trait | Spec's `Panel::title` would clash with GPUI Kit's dock `Panel::title`. | Named `title_text`. | M1 |
| Fuzzy matching | Spec says `nucleo`. | Uses `nucleo-matcher` (the matching core of the same project); the full `nucleo` crate adds a threaded matcher we don't need for a few dozen commands. Revisit for quick-open (M4). | M1 |
| Close dirty tab | Not specified. | Closing a tab with unsaved edits keeps the buffer in the workspace; reopening the file brings the edits back. There is no "save changes?" prompt yet. | M1 |
