# AGENTS.md — working on pide

pide is a personal native code editor in Rust on GPUI + GPUI Kit, with the pi coding agent built in.
The spec is the claude.ai doc "pi IDE — Implementation Spec" (https://claude.ai/artifact/ES6XEPGZHMPVQ6cWXYMLPE).
Milestones run in order (M0–M6); don't start the next one until the user signs off the current one.
Primary target: macOS on Apple Silicon. Linux is secondary.

## Crate layout

Dependencies only go downward: `app` → panels/`editor`/`agent-pi` → `core`/`runtime`. `core` depends on nothing in this repo.

| Crate | Package | What it owns |
| --- | --- | --- |
| `crates/app` | `pide` | Binary. One window per project (`projects.rs`: open/focus, recent projects, Open Folder…, Recent Projects switcher, close → quit on last window), in-window menu bar (`menus.rs`: items are `RunCommand`s; add new commands there too), `DockArea` + layout save/restore, Find Action palette, keymap file watching, status bar, `RunCommand` dispatch. Registers each window's `Services` and installs the file opener. |
| `crates/core` | `pide-core` | `CommandRegistry` + `RunCommand`, keymap (`keymap.json`, hot reload), `settings.json` (theme, send-context), `Workspace` + `Buffer` (source of truth for file contents, save/reload/conflicts, file watcher), typed events, `Panel` trait, `Agent` + `AgentBackend` trait, `Sessions` (several agents at once, saved pi sessions on disk), `changes` (what pi changed per session, revert), `context` (editor context block), `opener` (open file / open diff without panel→panel deps), `paths`. |
| `crates/runtime` | `pide-runtime` | The single tokio runtime on its own thread; the UI-stall monitor (logs anything over 16 ms). |
| `crates/editor` | `pide-editor` | Editor tabs (`EditorPanel`): GPUI Kit `EditorState` around a workspace buffer, dirty marker, read-only/conflict/deleted bars, `editor.*` commands. The pi diff tab (`diff_panel`). Provides the editor-context source. |
| `crates/agent-pi` | `pide-agent-pi` | `AgentBackend` over `pi --mode rpc`, one process per running session: lifecycle (lazy start, idle stop after 5 min, resume with `--session`), JSONL connection, wire → `AgentEvent` mapping, before-snapshots for `edit`/`write`, raw log. |
| `crates/lsp` | `pide-lsp` | Language servers (M3): registry of known servers + `settings.json` overrides + install detection, JSON-RPC over stdio on the tokio runtime (raw log `lsp-rpc.jsonl`), `LspStore` per workspace (start on demand, document sync, diagnostics, progress, crash restart), position conversion (GPUI Kit columns are characters; servers default to UTF-16), install runner. |
| `crates/panels/file_tree` | `pide-file-tree` | Project tree panel. |
| `crates/panels/language_servers` | `pide-language-servers` | Language Servers tab: status, Install (streams output), Restart. |
| `crates/panels/agent_chat` | `pide-agent-chat` | pi panel: session tabs (narrow) / sidebar (≥ 640 px), per-session chat with history, steer/follow-up, extension dialog cards, Changes view, context toggle, `agent.*` commands. |
| `spike/` | `spike` | M0 throwaway benchmark. Not part of the app. |
| `reference/` | — | Upstream GPUI Kit examples at the pinned tag. Not built. Read these before writing GPUI code. |

## Rules (from the spec — they exist because this is where agents go wrong)

1. **Current GPUI API only:** `Entity<T>`, `Context<T>`, `Window`, `App`. `View`, `ViewContext`, `WindowContext`, `Model` don't exist anymore.
2. **Copy from `reference/` at the pinned version.** Don't write GPUI from memory. Import GPUI only via `gpui_kit` — never add a `gpui`/`gpui-pre` dependency.
3. **`cargo check` after every file you change.** Never leave the workspace not building.
4. **Don't write editing code** (buffer, cursor, rendering, highlighter). If GPUI Kit's editor can't do something, write it in `docs/GAPS.md` and ask the user.
5. **Don't guess pi's protocol.** pi logs every raw line to `~/.local/state/pide/pi-rpc.jsonl`; derive types from what's there. Tests use recorded fixtures (`crates/agent-pi/tests/fixtures/`). Read the installed pi's `docs/rpc*.md` and `docs/json.md` (`$(npm root -g)/@earendil-works/pi-coding-agent/docs`).
6. **No highlighting = a missing tree-sitter Cargo feature**, not a bug. Features are set once, in the workspace `Cargo.toml`.
7. **Panels never import each other.** Shared needs go into `core` as an event or command — ask the user first. (Example: the file tree opens files through the `FileOpener` global the app installs, not by depending on `pide-editor`.)
8. **Keep scope to the current milestone.** Ideas go in `docs/IDEAS.md`.
9. **Keep this file up to date.**
10. **Commits:** small, one per working step, clear messages. **No AI attribution anywhere** — no `Co-Authored-By`, no session links, no "Generated with".

## Conventions and gotchas learned so far

- **No I/O on the UI thread.** File reads/writes go through `cx.background_executor()`; pi and (later) LSP I/O run on the tokio runtime (`pide_runtime::spawn`) and come back over `tokio::sync::mpsc` into a `cx.spawn` task. Exceptions, on purpose: the saved layout (a few KB) is read once at startup.
- **Re-entrancy:** `AgentBackend` methods run inside the `Agent`'s own update. Use the `&mut Agent` argument; calling `cx.entity().update(..)`/`.read(..)` on it there panics.
- **Panels reach each other only through `core`:** `opener::open_file` / `open_diff` (installed by the app), `context::current(window, cx)` (installed by the editor), `Services { workspace, sessions }`.
- **One window per project (M2.5).** `Services` is per window, not a global: get it with `Services::of(window, cx)` (in a panel's `new`, then keep the handles in fields). Anything else that is per project (editor dock host, open-tab lists, the shell) is keyed by `WindowId` and dropped in the app's `on_window_closed`. Never add a project-wide `Global`.
- **One `Agent` per session.** `Sessions` holds them; `Services::active_agent(window, cx)` is the one the pi panel shows. New `AgentEvent`s go in `core/src/events.rs`, parsing in `agent-pi/src/wire.rs`, with a recorded fixture test.
- **Dev hooks:** `PIDE_PI_ARGS` adds flags to every pi process (`PIDE_PI_ARGS="-e dev/pi-ask-test.ts"` loads the dialog test extension: send `/ask-test`). `PIDE_BENCH=<file>` runs the frame benchmark.
- **Every action is a command.** Register with `CommandRegistry::register(cx, CommandSpec::new("area.verb", "Title").key("cmd-…"), handler)`. Keys are JetBrains-style macOS defaults. Keybindings all dispatch `RunCommand { id }`, handled by the shell's root view.
- **LSP positions:** everything to or from a server goes through `pide_lsp::convert` (characters ↔ the server's encoding). Never hand a server range to GPUI Kit unconverted.
- **LSP store:** `LspStore::for_workspace(&workspace, cx)`; it lives as long as the workspace, and dropping it kills its servers. Logs: `lsp-rpc.jsonl`, `lsp-<server>-stderr.log`.
- **Keymap hot reload** snapshots GPUI Kit's own bindings once (`keymap::init`, after every crate's `init`) and re-adds them on each reload — so call `keymap::init` last.
- **Panel trait:** ours is `pide_core::Panel` (`title_text`, `default_dock`) on top of GPUI Kit's dock `Panel`/`BasePanel` (`panel_name`, `title`, `dump`). It's `title_text`, not `title`, because GPUI Kit already has a `title` method.
- **Saved layouts** rebuild panels by `panel_name` through `register_panel`; a panel that needs state (an editor's path) writes it in `dump()` as `PanelInfo::panel(json)`. Bump `DOCK_VERSION` in `app/src/shell.rs` when the default layout changes incompatibly.
- **`use gpui_kit::*` shadows `#[test]`** with GPUI's test macro. In test modules that glob-import it, write `#[::core::prelude::v1::test]`.
- **Buffers:** the editor gets `\n` text; `Buffer` remembers CRLF and BOM and restores them on save (byte-identical round trip; tests in `core/src/buffer.rs`). Non-UTF-8 files open read-only and are never saved.
- **`taffy` must stay at 0.15.0** in `Cargo.lock` (GPUI's `profiler` feature needs it). After `cargo update`, run `cargo update -p taffy --precise 0.15.0`.

## Running and logs

```sh
cargo run -p pide -- <project-dir>        # debug (dependencies optimised; our crates fast to rebuild); no dir = last project
cargo run -p pide --release -- <dir>      # for performance checks
cargo test --workspace --locked
# frame-time check in the real app (window must be visible and frontmost; macOS throttles hidden windows):
XDG_STATE_HOME=/tmp/pide-bench PIDE_BENCH=$PWD/spike/fixtures/large.rs ./target/release/pide .
grep bench /tmp/pide-bench/pide/pide.log
```

- App log: `~/.local/state/pide/pide.log` (`RUST_LOG=debug` for more). UI stalls over 16 ms show up as `ui thread blocked` / `slow frame` warnings.
- pi: `~/.local/state/pide/pi-rpc.jsonl` (every raw line, both directions), `~/.local/state/pide/pi-stderr.log`.
- Layouts: `~/.local/state/pide/layouts/<hash of project root>.json`. Recent projects: `~/.local/state/pide/recent.json`. Keymap: `~/.config/pide/keymap.json`.
