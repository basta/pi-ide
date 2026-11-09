# pide

A personal, native code editor in Rust on GPUI + GPUI Kit, with the pi coding agent built in.
Spec: the claude.ai doc "pi IDE — Implementation Spec". Contributor/agent guide: `AGENTS.md`.

Status: **M3** (language servers), **M3.5** (pi Settings) **M4** (Go to File, Find in Files, git) and **M5** (Markdown preview, PDF viewer, open in browser) built; see `docs/GAPS.md` for the PDF part that still needs a first run. M0 results: `docs/SPIKE.md`.

## Run

```sh
cargo run -p pide --release -- ~/some/project   # or no folder: reopens the last project
```

As a Mac app: `dev/bundle-macos.sh` builds `target/release/pide.app` (copy it to /Applications to start pide from Spotlight or the Dock; it reopens the last project).

Each project gets its own window. Right-click in the project tree for New File / New Folder / Rename / Move to Trash / Copy Path / Reveal in Finder. Find Action → **File: Open Project…** (folder picker), **File: Recent Projects…** (fuzzy switcher; × forgets an entry), **File: Close Project**.

Requires `pi` (`npm i -g @earendil-works/pi-coding-agent`) for the chat panel; it starts on your first message.

## Keys (JetBrains-style, macOS)

| Key | Command |
| --- | --- |
| Cmd+Shift+A | Find Action (every command) |
| Cmd+S | Save |
| Cmd+W | Close tab |
| Cmd+1 | Toggle project panel |
| Cmd+Shift+I | Ask pi: show the pi panel, cursor in its message box (with the editor context) |
| Cmd+9 | Git: local changes; Enter shows a file's diff against HEAD (↶ rolls back one change) |
| Cmd+Alt+Z | Git: roll back the change under the cursor to HEAD (undoable) |
| Ctrl+Alt+Shift+↓ / ↑ | Git: next / previous change |
| Esc (in the pi box) | Back to the editor |
| Cmd+F2 | Stop pi (active session) |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous pi session |
| Enter / Option+Enter (pi input, while pi works) | Steer / queue a follow-up |
| Cmd+Shift+O | Go to File |
| Cmd+E | Recent files (Enter flips to the previous file) |
| Option+F1 | Select the open file in the project tree |
| Cmd+L | Go to line (`12` or `12:5`) |
| Cmd+F / Cmd+R | Find / replace in the file |
| Cmd+Alt+L | Reformat the file (language server, e.g. rustfmt through rust-analyzer) |
| Ctrl+Alt+R | Run a shell command in the project folder (Run panel: output, Stop, Rerun) |
| Cmd+[ / Cmd+] | Navigate back / forward (after Go to File, Find Usages, a declaration in another file…) |
| Cmd+Shift+F | Find in Files (replace too) |
| Cmd+6 | Problems panel |
| Cmd+B (editor) | Go to declaration (also Cmd+click) |
| Cmd+Alt+B / Ctrl+Shift+B | Go to implementation / type declaration |
| F1 / Ctrl+J | Quick documentation (the hover, from the keyboard) |
| F2 / Shift+F2 | Next / previous error in the file (warnings when there are no errors) |
| Cmd+Shift+T | Reopen the last closed tab (also File: Close Other Tabs / Close All Tabs in Find Action) |
| View → Move Tab to Right Split | Two files side by side |
| Option+F7 (editor) | Find usages |
| Cmd+F12 (editor) | File structure |
| Cmd+Option+O | Go to symbol (project) |
| Option+Enter (editor) | Quick fixes |
| Cmd+P (editor) | Parameter info |
| Shift+F6 (editor) | Rename |
| Ctrl+Option+O (editor) | Optimize imports |
| Cmd+Q | Quit |

Override or add bindings in `~/.config/pide/keymap.json` (Find Action → "Preferences: Open Keymap File"); it reloads on save:

```json
{ "cmd-alt-p": "palette.open", "cmd-1": null }
```

## pi Settings

**Settings → pi…** shows and changes pi's setup without editing its files: install, update and remove packages (pi's own commands, output in the tab), switch built-in tools and extensions, skills and prompt templates on and off, add MCP servers, pick the default model and thinking level, and open or create AGENTS.md / SYSTEM.md. Choose **Everyone** (`~/.pi/agent`) or **This project** (`.pi/`). pi reads settings when a session starts; **Restart sessions** applies them (busy sessions restart when their run ends).

Each pi session shows its cost, context use, thinking level and model at the top right of the chat; click the model or the thinking level to change it for that session. **pi → Compact Session** summarizes a long conversation to free context. Type `/` in the message box to see pi's commands (extension commands, prompt templates, `skill:` commands); Tab or a click completes one. Type `@` and part of a file name to mention a project file (`@src/main.rs`). Paste a screenshot (Cmd+V with an image on the clipboard) to send it with your next message. **Ask pi** buttons in the Run panel and on each problem put the output or the error in the message box, ready to send.

**Changes** (in the pi panel) lists the files a session changed; click one for a side-by-side or inline diff against the file before pi touched it. **Revert** undoes the whole file; the ↶ button beside each change in the diff undoes just that change.

## Previews

- **View → Markdown Preview** opens a live preview beside a Markdown file.
- **PDF files** open in a PDF tab (zoom, fit width, search with highlights). The first time, click **Install pdfium**: pide downloads the pinned pdfium build (about 4 MB) into `~/.local/share/pide/pdfium`.
- **File → Open in Browser** for HTML and SVG.

## Language servers

pide starts rust-analyzer for `.rs` files and pyright for `.py` files. **Settings → Language Servers…** shows what's installed, installs what's missing (`rustup component add rust-analyzer`, `npm i -g pyright`) with the output in the tab, and restarts servers. A server that crashes restarts by itself up to 3 times in 3 minutes. Override a server's command in `settings.json`: `"lsp": { "rust-analyzer": { "command": "/path", "args": [] } }`.

Settings live in `~/.config/pide/settings.json` (`"theme": "dark" | "light" | "system"`, `"agent.send_context": true`, `"agent.notify": true`, `"agent.save_before_send": true`); the Find Action commands "View: Theme: …" change them. With `agent.notify` on, pide posts a macOS notification when pi finishes or asks a question while you are in another app (shown as coming from Script Editor; macOS asks once whether to allow it). With `agent.save_before_send` on, unsaved files are saved before a message goes to pi, since pi reads files from disk.

## Build requirements

- Rust >= 1.92 (developed on 1.98). Versions are pinned in `docs/VERSIONS.md`; build with `--locked`.
- macOS (primary target, Apple Silicon): Xcode command line tools.
- Linux (secondary): a Wayland or X11 session with a working Vulkan driver. GPUI Kit may also need
  `libwebkit2gtk-4.1-dev` at build time. Recommended: `mold` for linking.

## Files

- Logs: `~/.local/state/pide/pide.log`, pi traffic `~/.local/state/pide/pi-rpc.jsonl`, pi stderr `~/.local/state/pide/pi-stderr.log`.
- Language servers: every message in `~/.local/state/pide/lsp-rpc.jsonl`, stderr in `lsp-<server>-stderr.log`.
- Saved layouts: `~/.local/state/pide/layouts/`.
