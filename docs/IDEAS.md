# Ideas (not scheduled)

Things that came up while building, deliberately not built yet (spec rule 8).

- **Per-hunk revert** in the pi diff tab (M2 reverts whole files).
- **Attribute `bash` file changes** to the session whose `bash` call was running (see GAPS.md).
- **macOS notifications** for background sessions once pide ships as an app bundle.
- **Use `edit`'s `details.patch`** to double-check or repair a before-snapshot.
- **Find usages** (Alt+F7) and **go to symbol** (Cmd+Alt+O) through `textDocument/references` and `workspace/symbol`, results in a bottom panel. Not in the M3 spec; JetBrains users will miss them.
- **File structure** (Cmd+F12) from `textDocument/documentSymbol`.
- **Incremental document sync.** Full text per change is fine up to the 1.7 MB benchmark file (typing p95 2.7 ms), but GPUI Kit's `InputEditsEvent` carries exact edits if bigger files need it.
- **Auto-import on completion**: apply `additionalTextEdits` (see GAPS.md).
