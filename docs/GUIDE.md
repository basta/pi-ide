# Using pide

The reference for pide's panels and settings. For a guided first run, see [TRY-IT.md](TRY-IT.md); for shortcuts, [KEYS.md](KEYS.md).

## Projects and windows

Each project gets its own window. Right-click in the project tree for New File / New Folder / Rename / Move to Trash / Copy Path / Reveal in Finder. Find Action → **File: Open Project…** (folder picker), **File: Recent Projects…** (fuzzy switcher; × forgets an entry), **File: Close Project**.

## pi Settings

**Settings → pi…** shows and changes pi's setup without editing its files: install, update and remove packages (pi's own commands, output in the tab), switch built-in tools and extensions, skills and prompt templates on and off, add MCP servers, pick the default model and thinking level, and open or create AGENTS.md / SYSTEM.md. Choose **Everyone** (`~/.pi/agent`) or **This project** (`.pi/`). pi reads settings when a session starts; **Restart sessions** applies them (busy sessions restart when their run ends).

Each pi session shows its cost, context use, thinking level and model at the top right of the chat; click the model or the thinking level to change it for that session. **pi → Compact Session** summarizes a long conversation to free context. Type `/` in the message box to see pi's commands (extension commands, prompt templates, `skill:` commands); Tab or a click completes one. Type `@` and part of a file name to mention a project file (`@src/main.rs`). Paste a screenshot (Cmd+V with an image on the clipboard) to send it with your next message. **Ask pi** buttons in the Run panel and on each problem put the output or the error in the message box, ready to send.

**Changes** (in the pi panel) lists the files a session changed; click one for a side-by-side or inline diff against the file before pi touched it. **Revert** undoes the whole file; the ↶ button beside each change in the diff undoes just that change; **Mark all reviewed** / **Revert all…** act on the whole session. The list is saved with the session and comes back when you resume it. A file pi changes while it's open in the editor reloads as an undoable edit (Cmd+Z takes it back). **pi → Export Session as HTML** writes the conversation to a page and opens it; **pi → Rename Session…** names it. Unsaved files are saved before a message goes to pi (setting).

## Previews

- **View → Markdown Preview** opens a live preview beside a Markdown file.
- **PDF files** open in a PDF tab (zoom, fit width, search with highlights). The first time, click **Install pdfium**: pide downloads the pinned pdfium build (about 4 MB) into `~/.local/share/pide/pdfium`.
- **Images** (PNG, JPEG, GIF, WebP, SVG, …) open in an image tab.
- **File → Open in Browser** for HTML and SVG.

## Language servers

pide starts rust-analyzer for `.rs` files, pyright for `.py` files, typescript-language-server for `.ts` files and clangd for C, C++ and CUDA (`.c .h .cpp .cc .cxx .hpp .hxx .inl .cu .cuh` …). clangd needs a `compile_commands.json` (CMake: `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`); when there's none in the project root or `build/`, pide passes clangd the newest one in a build folder (`out/build/<preset>/`, `build*/`, `cmake-build-*/`). Choose one yourself with `"lsp": { "clangd": { "args": ["--compile-commands-dir=out/build/Debug"] } }` or a `.clangd` file with `CompilationDatabase:`. On Windows pide installs clangd with `winget install LLVM.LLVM`, and starts it in Visual Studio's developer environment (what `vcvars64.bat` sets, found with vswhere; skipped when pide itself runs in a Developer Prompt), so MSVC projects find ATL/MFC and the rest of the toolset's headers, as in CLion. **Settings → Language Servers…** shows what's installed, installs what's missing (`rustup component add rust-analyzer`, `npm i -g pyright`, `npm i -g typescript-language-server typescript`) with the output in the tab, and restarts servers. A server that crashes restarts by itself up to 3 times in 3 minutes. Override a server's command in `settings.json`: `"lsp": { "rust-analyzer": { "command": "/path", "args": [] } }`.

Syntax highlighting: Rust, Python, TypeScript/TSX, JavaScript, JSON, TOML, YAML, Bash, HTML, CSS, Markdown.

**Settings → pide…** (Cmd+,) has switches for all of these. Settings live in `~/.config/pide/settings.json` (`"theme": "dark" | "light" | "system"`, `"agent.send_context": true`, `"agent.notify": true`, `"agent.save_before_send": true`, `"editor.format_on_save": false`); the Find Action commands "View: Theme: …" change them. Edits to the file apply as soon as it is saved. With `agent.notify` on, pide posts a macOS notification when pi finishes or asks a question while you are in another app (shown as coming from Script Editor; macOS asks once whether to allow it). With `agent.save_before_send` on, unsaved files are saved before a message goes to pi, since pi reads files from disk.

## Where pide keeps its files

- Logs: `~/.local/state/pide/pide.log`, pi traffic `~/.local/state/pide/pi-rpc.jsonl`, pi stderr `~/.local/state/pide/pi-stderr.log`.
- Language servers: every message in `~/.local/state/pide/lsp-rpc.jsonl`, stderr in `lsp-<server>-stderr.log`.
- Saved layouts: `~/.local/state/pide/layouts/`.
