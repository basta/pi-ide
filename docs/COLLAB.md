# Collaborator mode

> **Guiding principle.** pide sits between hands-off vibecoding and writing code by hand. The user no
> longer writes code by hand, but wants to understand the codebase. When choosing, favor what helps
> them understand and follow pi's code (clear explanations, walkthroughs, reviewable changes) over
> manual-editing features or autonomy that skips them.

pi's work lands in the code, where you can follow it and answer it. Each pi session is a thread.
When pi finishes, its changes come with a reason per file and per hunk; you step through them in
the editor and reply in place.

## Transport (spike, 2026-11-14, pi 1.5.0)

pide ships its own pi extension: `crates/agent-pi/ext/pide.ts`, compiled in with `include_str!`
(`backend::EXTENSION_SOURCE`), written to `$XDG_STATE_HOME/pide/pi-extension/pide.ts` on the runtime
thread before each pi starts, and passed with `-e` ahead of `PIDE_PI_ARGS`.

**How a tool call reaches pide: through the RPC stream pi already sends.** No socket and no new protocol.
pi's `docs/json.md` documents `tool_execution_end` with `toolName` and `result`, and `result` is the
tool's return value including `details` (`docs/extensions.md`, Custom Tools: "`details` … for rendering
and state"). Each pide tool validates its input, reads the anchor line from disk, and returns

```json
{"content": [...for the model...], "details": {"pide": 1, "kind": "pin" | "explain" | "progress" | "refused", ...}}
```

`agent-pi/src/wire.rs` turns a non-error `tool_execution_end` whose `details.pide == 1` into
`AgentEvent::Pin` / `ExplainChange` / `Progress` (after the usual `ToolEnd`). Recorded run:
`crates/agent-pi/tests/fixtures/collab_tools_session.jsonl` (streaming deltas and the system prompt
dropped to keep it small).

**pi asking pide** (`ide_diagnostics`): the extension calls `ctx.ui.input("pide:query", <json>, {timeout: 5000})`.
That's a documented blocking dialog (`docs/rpc-extension-ui.md`): pi sends an `extension_ui_request` and
waits for an `extension_ui_response` with the same id. pide recognizes the title (`wire::QUERY_TITLE`),
never shows it, and answers at once with JSON in `value` from a `collab::QueryHandler` the app installs
per project (`app/src/collab_queries.rs`, in-memory state only). Outside pide nobody answers; the dialog
times out and the tool says so. Recorded: `tests/fixtures/collab_diagnostics_query.jsonl`.

Alternatives looked at and not used:

- `ctx.ui.setWidget` / `setStatus` (fire-and-forget `extension_ui_request`): works, but would carry
  JSON inside display strings, and the model's tool result wouldn't say what pide got.
- `pi.events`: an in-process bus between extensions. It doesn't reach the RPC client.
- A per-window Unix socket: not needed.

Findings:

- `promptGuidelines` on a tool lands as a bullet in the system prompt's `<rules>` while the tool is
  active (checked in the recorded system message). That is the "after changing code, call
  ide_explain_change for each changed file" note.
- What pide is (the `<editor-context>` block, the user reviewing changes in pide) is a `<pide>`
  section of the system prompt: a `before_agent_start` handler sets
  `event.systemPromptOptions.sections.pide`. pi sends structured sections once and only patches a
  change, so the prompt cache survives (returning `systemPrompt` would replace the whole prompt).
  Checked with a second extension dumping `ctx.getSystemPrompt()`: the section comes after `<cwd>`.
- Plain JSON Schema works for `parameters`, so the extension has no imports beyond `node:fs` and
  `node:path` and loads from any folder.
- pi's `ctx.cwd` can differ in spelling from pide's root (`/private/tmp/x` vs `/tmp/x`), so the
  tools send paths relative to the project when they're inside it; pide resolves them like the
  `edit`/`write` paths (canonical parent dir).
- The pin cap (5 per prompt) is counted in the extension and reset on `agent_start`. A refused pin
  is an error result telling the model to mention it in its reply instead.
- The throwaway `pi --mode rpc --no-session` that pi Settings uses to list commands doesn't load
  the extension (it would add nothing there).

## Tools

| Tool | Arguments | pide shows |
| --- | --- | --- |
| `ide_pin` | `path, start_line, code, end_line?, quote?, note` | pin card in the chat, gutter mark + note bar in the editor |
| `ide_explain_change` | `path, summary, hunks?: [{line, note}]` | summary under the file in Changes, hunk notes in the editor and the pi diff tab |
| `ide_progress` | `steps: [{text, done}]` | a checklist at the top of the session, updated in place |
| `ide_tour` | `title, stops: [{path, line, end_line?, note}]` | a tour card in the chat; the stops walk in the note bar |
| `ide_diagnostics` | `paths?` | (pi looks) the errors and warnings the editor's language servers report, as a tool card in the chat |

`ide_diagnostics` waits until 2 s after pi's last `edit`/`write`/`bash` before asking, so the language
server has re-checked the file. It only knows files the editor has open (or had open): pide doesn't
start servers for files nobody opened; a file it doesn't know is named in the answer. A file open with a
running server and no entry counts as clean (servers drop a file's entry once its problems are gone).

## Catch me up

**Catch me up** (pi panel toolbar) asks for a short briefing on the session: Asked / Done (by file) /
Needs you / Start here. The panel writes down what it knows (prompts, pi's replies clipped, tool calls,
changed files with pi's reasons and hunk notes, pins, the plan; at most 24k characters, newest kept) and
the backend asks the model once on the side (`AgentBackend::ask_aside`: `pi -p --no-session --no-tools
--thinking low --model <the session's>`). The session's context and history are untouched. The briefing
is shown above the chat, saved with the session (`Saved::catch_up`), and refreshed on demand.

## Ask why, Explain this

- **Ask why** (Changes): files pi changed without an `ide_explain_change` are marked "no reason given";
  the button sends one prompt in the session asking pi to explain just those, without changing code
  (queued as a follow-up if pi is busy). The walkthrough is only as good as pi's explanations; this
  closes the gap on demand, never automatically.
- **Explain This Code** (Code / pi menus, `agent.explain_this`): for the selection, or the code around the
  cursor, pi explains without changing anything: 2–5 pins in reading order and a short overview in
  the chat. The pins join the walkthrough (the note bar steps through them). This covers code pi
  didn't just change, which is most of what there is to understand.

## Review

**pi → Review Changes** (Cmd+Shift+R, the "◆ n to review" status chip, Review in Changes) opens one
center tab with everything pi changed and said in the open sessions (`editor/src/review_panel.rs`;
the data is `pide_core::review`, built off the UI thread from each session's change list and pins,
nothing new stored). One GPUI Kit `Diff` holds every changed file, session by session, each from the
text before that session's pi first touched it (a file two sessions changed shows under each, with
only its own part). Under each file's header: pi's reason, or "changed without saying why" with
**Ask pi why**. Each note sits under the line it's about (a `DiffAnnotation`, re-found by its text),
with its thread, a reply box (Enter sends, as on the editor's cards; off while pi is busy), Open in
editor, Mark seen, Resolve; notes on lines the diff doesn't show go under the header. File headers
have Open, Revert file and Reviewed; each hunk has a numbered revert in the gutter.

The index on the left starts with **Needs you**: pins you haven't answered or resolved, then files
without a reason. Then each session's files (click to scroll there) and its notes on files it didn't
change. A busy session's new changes wait until it settles, so the page doesn't move while you read.
Closed sessions aren't included (like the chip and the Overview). Design and plan:
https://claude.ai/artifact/LKtZdRns37q3hc8RTn9ByR

## Tours

**pi → Take a Tour…** asks how something works ("how does a session resume after a restart?"). It
starts a session for the question (a thread of its own), and pi answers without changing code by
calling `ide_tour` once: 4–15 stops in reading order across files (`path`, `line`, `note`; the
extension adds each line's text and up to 8 lines of snippet). The stops become pins with ids
`tour:<call id>:<n>`, so they walk in the note bar in pi's order (a tour session has no changed files,
so nothing reorders them) and survive a resume like any pin. The chat shows one tour card: the title,
the numbered stops (click to go there), Start / Continue.

**Tell it as a story** (Changes) asks pi, in the session, to retell its change as a tour ("The change, step
by step") in the order that explains it, from the entry point onward, instead of file by file.
**TOURS** in the session list (▾) lists every tour in the project, from open sessions and saved ones;
click one to open its session (a saved one is resumed; pi itself starts only on your next message) and
walk from its first unseen stop.

## Notes in the code

pi's notes on the open file float over the code and take no width from it
(`editor/src/pi_float.rs`). Each note's lines get a faint magenta tint (the current one stronger). At
rest a note is a "lens": one dim line after the code on its first line, "◆ pi <the note's first
words> · 2 replies"; when the line is too long for that, a small "◆ pi" pill at the editor's right
edge. Clicking a lens or pill opens its card (it becomes the walkthrough's current stop and is marked
seen); Back / Next open the next note's card. The card sits just below the note's lines (above them
when there's no room), with the whole note, its thread, Back / Next, ✕, Resolve (= Dismiss) and a
reply box (Cmd+Enter or Send). ✕ or Esc collapses it back into its lens. GPUI Kit can't make room
between lines, so the open card covers the code under it; that's why it collapses. (Round 1 put cards
in a 300 px margin; it took too much width from the code.)

**Replies in place.** A reply is added to the note's thread (`Pin::thread`, `HunkNote::thread`,
`FileChange::thread`, saved with the session) and sent to pi in the session with the location prefix
and "don't change code unless I ask". When the run settles, pi's last text of that run joins the thread
(`Agent::reply_to_stop` / `answering`). Send is disabled while pi is busy, so a run answers exactly one
card. If pi re-explains the file meanwhile, the answer and the thread follow the note with the same
line text. Next step: an `ide_reply(thread, text)` tool so pi answers on the card explicitly.

**Positions.** GPUI Kit's `EditorState::range_to_bounds` gives a range's bounds from the editor's last
paint; the overlay corrects them by the scroll since then and asks for one more frame when what it drew
from was stale (a line came into view, the card's height changed). For a line above the laid-out rows
`range_to_bounds` reports the first laid-out line, not `None`, so visibility also checks
`visible_row_range()`. A note whose line is off screen shows nothing (the scrollbar marks still do).

## Notes across sessions

pi's notes belong to the session that made them, but the code is shared. **pi → pi's Notes in This
File** lists every session's notes on the current file: open sessions from memory, closed ones from
their saved state (`pide_core::notes`, re-anchored against the file, read off the UI thread), shown
in the locations list with the session's name. In the editor, other open sessions' notes are dimmer
gutter marks (the bar and the walkthrough stay with the active session). No new store: each
session's saved state is the source.

## Anchors

pi's line numbers are often off by a few, so `ide_pin`, `ide_tour` stops and hunk notes also take
`code`: the line's text copied from the file. The extension snaps to the nearest line containing it; a
pin whose code isn't in the file is refused, a tour stop or hunk note is left out, and pi is told why
(recorded tour of pide itself: 2 of 8 line numbers were off and snapped). Pins and hunk notes are
then anchored by 1-based line plus that line's text. The editor re-finds the text near the line in the current buffer. On load (resume),
the saved anchors are re-found in the file on disk; one whose text is gone is marked outdated
(the note still shows, at the old line, labelled "outdated").

## Notes on part of a line

`ide_pin`, hunk notes and tour stops take an optional `quote`: the exact words on the line the note is
about (within that one line). In prose a paragraph is one line, so without it a note marks the whole
paragraph. The extension finds the words on the placed line (when they place it better than `code`, it
uses their line), and sends `span: {quote, col}` with `col` a UTF-8 byte offset; words that aren't on
the line, or appear on it more than once, are dropped and the note covers the whole line (pi is told
why). pi quotes words rather than giving columns: models count characters badly.

In the editor only the words are tinted (framed when the note is current), the card opens under the
row where they end, and a note whose words end before the line's last visual row gets a pill on their
row. Going to the note puts the cursor on the words. The anchor keeps `span`; the words are found again
nearest to where they were, and when the line itself changed (any edit in a paragraph does) the note
is re-placed by its words alone (`Anchor::locate`), taking the changed line as its new anchor text.

## Decisions

- An `ide_explain_change` for a file pide has no change record for (it changed outside `edit`,
  `write` and watched `bash`) is kept as pins on that file, so it isn't lost and still walks.
- The walkthrough steps through every stop of the session: per changed file its hunk notes (or one
  stop at its first changed line when pi gave none), then pins, files in first-touched order.
  Stepping onto a stop marks it seen; ticking a file reviewed in Changes marks its stops seen, and
  stepping onto the last unseen stop of a file ticks it reviewed. Reply (note bar, pin card) drafts
  "file:line (about your note "…"): " so pi knows which note you answer.
  A session "needs review" while it has unseen stops.
- Only open sessions count for the Overview group and the status-bar chip (closed sessions' data
  isn't loaded).

## Morning summary (kept current)

Branch `collab-mode` (on top of main, which hasn't moved, so the merge is a fast-forward). A fresh
clone of the pushed branch passes `cargo test --workspace --locked` (checked at the end of the night;
it reused this machine's `target/` for the dependencies; no release build was made, to save disk). Screenshots: orphan branch `wip-shots` and the gallery https://claude.ai/artifact/55ZpVNwYu4UoTALd83LMZ6.
Try it with the 10-minute script in docs/TRY-IT.md ("Ten minutes with collaborator mode").

**What you get** (each built, reviewed by a second agent, fixed, and tested in the app via devctl):

| Feature | Where | Keep? (my recommendation) |
| --- | --- | --- |
| pide's pi extension: pins, explanations, checklist, tours, diagnostics; anchors snap to quoted code | `crates/agent-pi/ext/pide.ts` | keep (the base of everything) |
| Walkthrough: note bar (Reply / Dismiss / Back / Next), magenta gutter + scrollbar marks | editor | keep |
| Changes: reason per file, "no reason given · ask why", Walk through, Tell it as a story | pi panel | keep |
| Tours: Take a Tour…, tour cards, TOURS in the session list | pi panel | keep (most useful for understanding) |
| Explain This Code | Code / pi menu | keep |
| Catch Me Up (one side call, session untouched) | pi toolbar | keep |
| `ide_diagnostics` (pi sees your editor's errors; it now checks its own work unprompted) | extension + app | keep |
| Overview: TO REVIEW group, "◆ n to review" chip, "◆ n pi notes" in Commit | session list, status bar, Commit | keep |
| pi's Notes in This File, dim marks for other sessions | pi menu, editor | optional: cut if the dim marks feel noisy |
| Understand card (? and empty sessions) | pi panel | keep (the map of the above) |

**Validated on pide itself** (a worktree in /tmp, rust-analyzer off): a 12-stop tour of "how a session
comes back after a restart" placed all 12 stops correctly (1 line number snapped); a real small change
was explained, walked and shown in Commit. Found and fixed on the way: the review chip opened an older
session; `ide_diagnostics` didn't say the language server was down. Found and not a bug: devctl shots
of a just-opened tab can be empty (documented in AGENTS.md). A cold run of the TRY-IT script on a
fresh project and state, with pyright on, worked step by step: pi's ide_diagnostics saw pyright's real
error, fixed it, and checked again ("no problems"); story, catch-up and Commit's marker worked from the
new commands.

**Decisions for you** (defaults in bold):

1. Merge `collab-mode` into main **as one merge** (it's 43 small commits, all green), or squash per feature?
2. Stepping through all of a file's notes **ticks it reviewed** (and ticking marks its notes seen). Keep
   **yes**, or keep the two separate?
3. The extension loads into every pi pide starts; **Settings → pi's collaborator tools** (`agent.collab`,
   default **on**) turns it off for sessions started afterwards, so the merge is reversible without a
   revert (checked: with it off, pi starts with plain `--mode rpc` and a session works normally; the
   collaborator UI just stays empty). Keep it on by default?
4. Pins are capped at **5 per prompt** (tours: 15 stops). Keep **yes**, or raise?
5. Dim marks for other sessions' notes: keep **yes**, or show only the active session's?

**Not done / ideas:** a change story that reorders the main walkthrough itself (today it's a separate
tour); notes for files pi changed outside edit/write/bash; centring the walked-to line in the editor
(GPUI Kit scrolls it just into view, often to the bottom edge; no API for it, tried a workaround,
see docs/GAPS.md).

## collab-next (on top of collab-mode)

`collab-mode` is frozen as the merge candidate. `collab-next` continues from it with the follow-ups in
docs/IDEAS.md; take it instead if you want these too (same rules: reviewed per milestone, tested in the
app, pushed):

- **ide_diagnostics for files the editor hasn't opened**: pide reads them in the background and hands
  them to their language server (`LspStore::check_file`), answers "pending" for a few seconds, and the
  extension asks again. Before, pi's self-check only covered files open in the editor. Tested: an
  unopened file with a type error came back as pending, then with pyright's error.
- **Tours stay current**: a tour card counts its outdated stops (code changed since; found again on
  resume) and offers **Update tour**: pi redoes it in the tour's session against the code as it is
  now, and the new tour replaces the old one. Tested: after renaming two parameters in the demo, the
  card showed "3 outdated"; the updated tour matched the new code (and pi pointed out the rename had
  broken the bodies).
- **The walkthrough follows the story**: once a session has a story (Tell It as a Story), Walk Through
  Changes and the review chip start with the story's stops, in its order, until you've seen them.
- Reviews of these found 8 issues (stale diagnostics right after an edit, files that can't be read,
  overwriting an editor's buffer from disk, replaced-tour bookkeeping); all fixed.
- **Reasons survive the commit**: committing files pi explained saves their reasons and hunk notes by
  commit hash (`pide_core::commit_notes`, `state/commit-notes/<project>/<hash>.json`); **Git Log**
  shows them under "Why, per pi" in the commit's details, with a link that reopens the session.
  Tested: a commit of pi's stats.py fix shows pi's reason in the log.
- **Why This Line? (pi)** (Code menu) answers from what pi already said, without asking it again: a
  note at or near the line in an open session opens in the note bar; else git blame finds the commit
  and shows pi's reason kept with it; only if neither exists does it draft a question to pi.
  Tested: with every session closed, a committed line showed pi's reason from the commit.

Checked as one chain on a project that is a subfolder of its repo: pi changed and explained a file,
it was committed from the Commit panel, amended, every session closed, pide restarted; Why This
Line on a changed line still showed pi's reason (paths stay project-relative; the amend carried
the notes). `cargo test --workspace --locked` passes; clippy is clean for collab-next's code.

Limits: commit notes are keyed by commit hash, so a rebase or cherry-pick (new hashes) leaves them
behind under the old hash; a partial commit keeps the whole file's notes, including ones about hunks
that weren't committed.
