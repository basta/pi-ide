# Ideas (not scheduled)

Things that came up while building, deliberately not built yet (spec rule 8).

- **Use `edit`'s `details.patch`** to double-check or repair a before-snapshot.
- **Incremental document sync.** Full text per change is fine up to the 1.7 MB benchmark file (typing p95 2.7 ms), but GPUI Kit's `InputEditsEvent` carries exact edits if bigger files need it.
- **Git panel, later** (designed on the canvas, being built): push/pull, branches (switch/create), blame, staging single hunks, merge-conflict view, pi drafting the commit message from the staged diff, a proper git-branch icon (GPUI Kit's default set only has GitHub's).
- **Smart Checkout** (JetBrains): when local changes block a checkout, offer stash → checkout → unstash instead of only showing git's error.

Terminal, after v1 (`crates/panels/terminal`):

- **Find in the terminal**: alacritty_terminal has regex search over the scrollback (`term/search.rs`); needs a find bar.
- **Shells back after a restart** (JetBrains restores each tab's folder), and **split panes**.
- **Follow the shell's folder** for relative `file:line` links: shells that report it with OSC 7 (fish, zsh/bash with a hook, PowerShell with a prompt function); today links resolve from the start folder.
- **Underline links under the mouse** while ⌘/Ctrl is held, and MSVC's `file(line,col)` form.
- **Run panel on the terminal**: real colours and progress bars instead of stripped ANSI.

Collaborator mode, after the merge (from tonight's advisor rounds and testing; docs/COLLAB.md):

- **Centre the walked-to line**: needs a GPUI Kit API (docs/GAPS.md); ask upstream.
- **Explanations for files pi changed outside edit/write/watched bash** (kept as pins today, which show and walk). A real change record needs a "before" text; taking it from HEAD would make Revert drop your own uncommitted edits, so it'd need a no-revert kind of change. Rare in practice; skipped on collab-next.
- **Tours library as its own view**: search and delete (re-running an outdated tour is done: Update tour, on collab-next).
- **Per-project notes index** if listing every saved session's notes gets slow on big histories (`pide_core::notes` reads them all on demand).
