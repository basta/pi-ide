# Ideas (not scheduled)

Things that came up while building, deliberately not built yet (spec rule 8).

- **Use `edit`'s `details.patch`** to double-check or repair a before-snapshot.
- **Incremental document sync.** Full text per change is fine up to the 1.7 MB benchmark file (typing p95 2.7 ms), but GPUI Kit's `InputEditsEvent` carries exact edits if bigger files need it.
- **Git panel, later** (designed on the canvas, being built): push/pull, branches (switch/create), blame, staging single hunks, merge-conflict view, pi drafting the commit message from the staged diff, a proper git-branch icon (GPUI Kit's default set only has GitHub's).
- **Smart Checkout** (JetBrains): when local changes block a checkout, offer stash → checkout → unstash instead of only showing git's error.

Collaborator mode, after the merge (from tonight's advisor rounds and testing; docs/COLLAB.md):

- **Story order for the main walkthrough**: let Walk through follow the story tour's order when a session has one (today the story is a separate tour).
- **Centre the walked-to line**: needs a GPUI Kit API (docs/GAPS.md); ask upstream.
- **Explanations for files pi changed outside edit/write/watched bash** (kept as pins today).
- **Tours library as its own view**: search, delete, re-run a tour against the current code (stops marked outdated today).
- **Per-project notes index** if listing every saved session's notes gets slow on big histories (`pide_core::notes` reads them all on demand).
- **Language-server check before ide_diagnostics answers**: open the file in the server when pi asks about a file the editor never opened.
