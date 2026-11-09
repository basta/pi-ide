# pide

A personal, native code editor in Rust on GPUI + GPUI Kit, with the pi coding agent built in.
Spec: the claude.ai doc "pi IDE — Implementation Spec". Contributor/agent guide: `AGENTS.md`.

Status: **M2.5** (projects: one window per project, recent projects) in progress; M2 signed off. M0 results: `docs/SPIKE.md`.

## Run

```sh
cargo run -p pide --release -- ~/some/project   # or no folder: reopens the last project
```

Each project gets its own window. Find Action → **File: Open Project…** (folder picker), **File: Recent Projects…** (fuzzy switcher; × forgets an entry), **File: Close Project**.

Requires `pi` (`npm i -g @earendil-works/pi-coding-agent`) for the chat panel; it starts on your first message.

## Keys (JetBrains-style, macOS)

| Key | Command |
| --- | --- |
| Cmd+Shift+A | Find Action (every command) |
| Cmd+S | Save |
| Cmd+W | Close tab |
| Cmd+1 | Toggle project panel |
| Cmd+F2 | Stop pi (active session) |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous pi session |
| Enter / Option+Enter (pi input, while pi works) | Steer / queue a follow-up |
| Cmd+Q | Quit |

Override or add bindings in `~/.config/pide/keymap.json` (Find Action → "Preferences: Open Keymap File"); it reloads on save:

```json
{ "cmd-alt-p": "palette.open", "cmd-1": null }
```

Settings live in `~/.config/pide/settings.json` (`"theme": "dark" | "light" | "system"`, `"agent.send_context": true`); the Find Action commands "View: Theme: …" change them.

## Build requirements

- Rust >= 1.92 (developed on 1.98). Versions are pinned in `docs/VERSIONS.md`; build with `--locked`.
- macOS (primary target, Apple Silicon): Xcode command line tools.
- Linux (secondary): a Wayland or X11 session with a working Vulkan driver. GPUI Kit may also need
  `libwebkit2gtk-4.1-dev` at build time. Recommended: `mold` for linking.

## Files

- Logs: `~/.local/state/pide/pide.log`, pi traffic `~/.local/state/pide/pi-rpc.jsonl`, pi stderr `~/.local/state/pide/pi-stderr.log`.
- Saved layouts: `~/.local/state/pide/layouts/`.
