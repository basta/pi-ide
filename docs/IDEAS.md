# Ideas (not scheduled)

Things that came up while building, deliberately not built yet (spec rule 8).

- **Use `edit`'s `details.patch`** to double-check or repair a before-snapshot.
- **Incremental document sync.** Full text per change is fine up to the 1.7 MB benchmark file (typing p95 2.7 ms), but GPUI Kit's `InputEditsEvent` carries exact edits if bigger files need it.
- **Git panel, later** (designed on the canvas, being built): push/pull, branches (switch/create), blame, staging single hunks, merge-conflict view, pi drafting the commit message from the staged diff, a proper git-branch icon (GPUI Kit's default set only has GitHub's).
- **Smart Checkout** (JetBrains): when local changes block a checkout, offer stash → checkout → unstash instead of only showing git's error.
