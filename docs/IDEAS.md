# Ideas (not scheduled)

Things that came up while building, deliberately not built yet (spec rule 8).

- **Use `edit`'s `details.patch`** to double-check or repair a before-snapshot.
- **Incremental document sync.** Full text per change is fine up to the 1.7 MB benchmark file (typing p95 2.7 ms), but GPUI Kit's `InputEditsEvent` carries exact edits if bigger files need it.
- **Git panel, later** (designed on the canvas, being built): push/pull, branches (switch/create), blame, staging single hunks, merge-conflict view, pi drafting the commit message from the staged diff, a proper git-branch icon (GPUI Kit's default set only has GitHub's).
- **Smart Checkout** (JetBrains): when local changes block a checkout, offer stash → checkout → unstash instead of only showing git's error.

Collaborator mode, after the merge (from tonight's advisor rounds and testing; docs/COLLAB.md):

- **Centre the walked-to line**: needs a GPUI Kit API (docs/GAPS.md); ask upstream.
- **Explanations for files pi changed outside edit/write/watched bash** (kept as pins today, which show and walk). A real change record needs a "before" text; taking it from HEAD would make Revert drop your own uncommitted edits, so it'd need a no-revert kind of change. Rare in practice; skipped on collab-next.
- **Tours library as its own view**: search and delete (re-running an outdated tour is done: Update tour, on collab-next).
- **Per-project notes index** if listing every saved session's notes gets slow on big histories (`pide_core::notes` reads them all on demand).

Settings tab:

- **Keymap as a page** (today the page opens the Keymap dialog); search inside a page that scrolls to and highlights the matching row.
- **Watch the panel folders** so new or deleted script panels appear without Look Again; Move to Trash, Copy to project / to yours, and choosing a panel's dock and key from the Script Panels page (writes `panel.json`).
- **Project overrides for pide's own switches** (`.pide/settings.json` holds only tool windows today; the `Settings` global is app-wide).
