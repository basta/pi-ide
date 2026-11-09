# pide

A personal, native code editor in Rust on GPUI + GPUI Kit, with the pi coding agent built in.
Spec: the claude.ai doc "pi IDE — Implementation Spec". Contributor/agent guide: `AGENTS.md`.

Status: milestones **M0–M5** built (M6 scripted panels: options written up in `docs/M6-SCRIPTED-PANELS.md`, waiting on a decision). `docs/GAPS.md` lists what GPUI Kit or pi can't do yet and the PDF part that still needs a first run; `docs/IDEAS.md` what's deliberately not built. M0 results: `docs/SPIKE.md`.

## Run

```sh
cargo run -p pide --release -- ~/some/project   # or no folder: reopens the last project
```

As a Mac app: `dev/bundle-macos.sh` builds `target/release/pide.app` (copy it to /Applications to start pide from Spotlight or the Dock; it reopens the last project).

Each project gets its own window. Right-click in the project tree for New File / New Folder / Rename / Move to Trash / Copy Path / Reveal in Finder. Find Action → **File: Open Project…** (folder picker), **File: Recent Projects…** (fuzzy switcher; × forgets an entry), **File: Close Project**.

Requires `pi` (`npm i -g @earendil-works/pi-coding-agent`) for the chat panel; it starts on your first message.

## Keys (JetBrains-style, macOS)

**General**

| Key | Command |
| --- | --- |
| Cmd+Shift+A | Find Action (every command) |
| Cmd+, | pide settings (theme, switches; Keymap… from there) |
| Cmd+S | Save |
| Cmd+W / Cmd+Shift+T | Close tab / reopen the last closed tab |
| Cmd+1 / Cmd+6 | Project panel / Problems panel |
| Ctrl+Alt+R | Run a shell command in the project folder (Run panel: output with clickable `file:line`, Stop, Rerun, Ask pi) |
| Cmd+Q | Quit |

**Navigation**

| Key | Command |
| --- | --- |
| Cmd+Shift+O | Go to File |
| Cmd+E | Recent files (Enter flips to the previous file) |
| Cmd+[ / Cmd+] | Back / forward (after Go to File, Find Usages, a declaration in another file…) |
| Cmd+L | Go to line (`12` or `12:5`) |
| Option+F1 | Select the open file in the project tree |
| Cmd+Shift+F | Find in Files (replace too) |
| Cmd+F / Cmd+R | Find / replace in the file |
| Cmd+B | Go to declaration (also Cmd+click) |
| Cmd+Alt+B / Ctrl+Shift+B | Go to implementation / type declaration |
| Option+F7 | Find usages |
| Cmd+F12 / Cmd+Option+O | File structure / go to symbol in the project |
| F2 / Shift+F2 | Next / previous error in the file |

**Code** (in the editor)

| Key | Command |
| --- | --- |
| Option+Enter | Quick fixes |
| F1 / Ctrl+J | Quick documentation |
| Cmd+P | Parameter info |
| Shift+F6 | Rename |
| Cmd+Alt+L | Reformat file |
| Ctrl+Option+O | Optimize imports |

**Git**

| Key | Command |
| --- | --- |
| Cmd+9 | Local changes; Enter shows a file's diff against HEAD (↶ rolls back one change) |
| Cmd+Alt+Z | Roll back the change under the cursor (undoable) |
| Ctrl+Alt+Shift+↓ / ↑ | Next / previous change |

**pi**

| Key | Command |
| --- | --- |
| Cmd+Shift+I | Ask pi: the pi panel, cursor in its box (with the editor context) |
| Esc (in the pi box) | Back to the editor |
| `/` · `@` (in the pi box) | pi's commands · mention a project file (Tab completes) |
| Enter / Option+Enter (while pi works) | Steer / queue a follow-up |
| Cmd+F2 | Stop pi |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous session |

View → Move Tab to Right Split puts two files side by side.

**Settings → Keymap…** lists every command with its shortcut; pick one and type a new shortcut (`cmd-shift-k`). It's saved to `~/.config/pide/keymap.json`, where you can also override or add bindings by hand (Find Action → "Preferences: Open Keymap File"); it reloads on save:

```json
{ "cmd-alt-p": "palette.open", "cmd-1": null }
```

## pi Settings

**Settings → pi…** shows and changes pi's setup without editing its files: install, update and remove packages (pi's own commands, output in the tab), switch built-in tools and extensions, skills and prompt templates on and off, add MCP servers, pick the default model and thinking level, and open or create AGENTS.md / SYSTEM.md. Choose **Everyone** (`~/.pi/agent`) or **This project** (`.pi/`). pi reads settings when a session starts; **Restart sessions** applies them (busy sessions restart when their run ends).

Each pi session shows its cost, context use, thinking level and model at the top right of the chat; click the model or the thinking level to change it for that session. **pi → Compact Session** summarizes a long conversation to free context. Type `/` in the message box to see pi's commands (extension commands, prompt templates, `skill:` commands); Tab or a click completes one. Type `@` and part of a file name to mention a project file (`@src/main.rs`). Paste a screenshot (Cmd+V with an image on the clipboard) to send it with your next message. **Ask pi** buttons in the Run panel and on each problem put the output or the error in the message box, ready to send.

**Changes** (in the pi panel) lists the files a session changed; click one for a side-by-side or inline diff against the file before pi touched it. **Revert** undoes the whole file; the ↶ button beside each change in the diff undoes just that change; **Mark all reviewed** / **Revert all…** act on the whole session. The list is saved with the session and comes back when you resume it. A file pi changes while it's open in the editor reloads as an undoable edit (Cmd+Z takes it back). **pi → Export Session as HTML** writes the conversation to a page and opens it; **pi → Rename Session…** names it. Unsaved files are saved before a message goes to pi (setting).

## Previews

- **View → Markdown Preview** opens a live preview beside a Markdown file.
- **PDF files** open in a PDF tab (zoom, fit width, search with highlights). The first time, click **Install pdfium**: pide downloads the pinned pdfium build (about 4 MB) into `~/.local/share/pide/pdfium`.
- **File → Open in Browser** for HTML and SVG.

## Language servers

pide starts rust-analyzer for `.rs` files, pyright for `.py` files and typescript-language-server for `.ts` files. **Settings → Language Servers…** shows what's installed, installs what's missing (`rustup component add rust-analyzer`, `npm i -g pyright`, `npm i -g typescript-language-server typescript`) with the output in the tab, and restarts servers. A server that crashes restarts by itself up to 3 times in 3 minutes. Override a server's command in `settings.json`: `"lsp": { "rust-analyzer": { "command": "/path", "args": [] } }`.

Syntax highlighting: Rust, Python, TypeScript/TSX, JavaScript, JSON, TOML, YAML, Bash, HTML, CSS, Markdown.

**Settings → pide…** (Cmd+,) has switches for all of these. Settings live in `~/.config/pide/settings.json` (`"theme": "dark" | "light" | "system"`, `"agent.send_context": true`, `"agent.notify": true`, `"agent.save_before_send": true`, `"editor.format_on_save": false`); the Find Action commands "View: Theme: …" change them. Edits to the file apply as soon as it is saved. With `agent.notify` on, pide posts a macOS notification when pi finishes or asks a question while you are in another app (shown as coming from Script Editor; macOS asks once whether to allow it). With `agent.save_before_send` on, unsaved files are saved before a message goes to pi, since pi reads files from disk.

## Build requirements

- Rust >= 1.92 (developed on 1.98). Versions are pinned in `docs/VERSIONS.md`; build with `--locked`.
- macOS (primary target, Apple Silicon): Xcode command line tools.
- Linux (secondary): a Wayland or X11 session with a working Vulkan driver. GPUI Kit may also need
  `libwebkit2gtk-4.1-dev` at build time. Recommended: `mold` for linking.

## Files

- Logs: `~/.local/state/pide/pide.log`, pi traffic `~/.local/state/pide/pi-rpc.jsonl`, pi stderr `~/.local/state/pide/pi-stderr.log`.
- Language servers: every message in `~/.local/state/pide/lsp-rpc.jsonl`, stderr in `lsp-<server>-stderr.log`.
- Saved layouts: `~/.local/state/pide/layouts/`.
