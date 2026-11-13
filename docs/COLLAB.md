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
  ide_explain_change for each changed file" note; no `before_agent_start` hook needed.
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
| `ide_pin` | `path, start_line, end_line?, note` | pin card in the chat, gutter mark + note bar in the editor |
| `ide_explain_change` | `path, summary, hunks?: [{line, note}]` | summary under the file in Changes, hunk notes in the editor and the pi diff tab |
| `ide_progress` | `steps: [{text, done}]` | a checklist at the top of the session, updated in place |
| `ide_diagnostics` | `paths?` | (pi looks) the errors and warnings the editor's language servers report, as a tool card in the chat |

`ide_diagnostics` waits until 2 s after pi's last `edit`/`write`/`bash` before asking, so the language
server has re-checked the file. It only knows files the editor has open (or had open): pide doesn't
start servers for files nobody opened; a file it doesn't know is named in the answer. A file open with a
running server and no entry counts as clean (servers drop a file's entry once its problems are gone).

## Anchors

Pins and hunk notes are anchored by 1-based line plus that line's text (read by the extension when
the tool ran). The editor re-finds the text near the line in the current buffer. On load (resume),
the saved anchors are re-found in the file on disk; one whose text is gone is marked outdated
(the note still shows, at the old line, labelled "outdated").

## Decisions

- An `ide_explain_change` for a file pide has no change record for (it changed outside `edit`,
  `write` and watched `bash`) is kept as pins on that file, so it isn't lost and still walks.
- The walkthrough steps through every stop of the session: per changed file its hunk notes (or one
  stop at its first changed line when pi gave none), then pins, files in first-touched order.
  Stepping onto a stop marks it seen; ticking a file reviewed in Changes marks its stops seen.
  A session "needs review" while it has unseen stops.
- Only open sessions count for the Overview group and the status-bar chip (closed sessions' data
  isn't loaded).

## Morning summary (kept current)

**Done** (branch `collab-mode`, pushed; screenshots on the orphan branch `wip-shots`, `shots/index.json`):

- MVP, steps 1–7: the extension and transport; pins, explanations and the checklist; core state saved per
  session and re-anchored on resume; split pi panel with pin cards, checklist, reasons in Changes; editor
  marks and the note bar (Reply / Dismiss / Back / Next walk across files); hunk notes in the diff tab;
  TO REVIEW group and the status-bar chip; docs. Reviewed; 7 review findings fixed.
- `ide_diagnostics`: pi sees what the editor's language servers report (tested with pyright in the app).

**Left / next:** "catch me up" per session; polish from devctl testing.

**Needs your decision:**

- Hunk notes count as "seen" when stepped onto; the Changes "reviewed" checkbox stays separate (ticking
  it marks the file's notes seen, not the other way round). OK, or should finishing a file's notes tick it?
