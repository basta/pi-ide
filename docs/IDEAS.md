# Ideas (not scheduled)

Things that came up while building, deliberately not built yet (spec rule 8).

- **Per-hunk revert** in the pi diff tab (M2 reverts whole files).
- **Attribute `bash` file changes** to the session whose `bash` call was running (see GAPS.md).
- **macOS notifications** for background sessions once pide ships as an app bundle.
- **Use `edit`'s `details.patch`** to double-check or repair a before-snapshot.
- **Incremental document sync.** Full text per change is fine up to the 1.7 MB benchmark file (typing p95 2.7 ms), but GPUI Kit's `InputEditsEvent` carries exact edits if bigger files need it.
