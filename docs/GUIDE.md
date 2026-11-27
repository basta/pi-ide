# Using pide

The reference for pide's panels and settings. For a guided first run, see [TRY-IT.md](TRY-IT.md); for shortcuts, [KEYS.md](KEYS.md).

## Projects and windows

Each project gets its own window. Right-click in the project tree for New File / New Folder / Rename / Move to Trash / Copy Path / Reveal in Finder. Find Action → **File: Open Project…** (folder picker), **File: Recent Projects…** (fuzzy switcher; × forgets an entry), **File: Close Project**.

## Settings

**Settings…** (Cmd+,) opens one Settings tab with every setting in pide. Pages are on the left: Appearance, Editor, Keymap, Tool Windows, Script Panels, Languages (Language Servers, Typst), pi (General, then pi's own setup) and Settings Files. The search box finds pages by their settings' names ("spell" finds Editor and Language Servers). Pages that can differ per project have an **Everyone / This project** switch at the top; the others say "Applies to every project". Everything applies as soon as you change it. **Settings → pi…**, **Language Servers…** and **Tool Windows…** open the tab on that page.

**Tool Windows** turns any tool window on or off, built-in or script panel. A tool window that's off loses its stripe button and its key, and closes. For **This project** the choice goes into `.pide/settings.json` (`"tool_windows.disabled"` / `"tool_windows.enabled"`, overriding Everyone's `"tool_windows.disabled"` in `settings.json`); **Reset** makes the project follow Everyone again.

**Script Panels** lists every script panel this project can show, where it comes from, what it asks for and whether you allowed it. From there: switch it on or off, **Show** it, **Open main.js**, **Reveal Folder**, **Revoke** what you allowed (it closes and asks again next time), **Ask pi to Fix** one that doesn't load, **New Panel with pi…**, and **Look Again** for new or deleted panel folders.

### pi

**Settings → pi…** shows and changes pi's setup without editing its files: install, update and remove packages (pi's own commands, output on the page), switch built-in tools and extensions, skills and prompt templates on and off, add MCP servers, pick the default model and thinking level, and open or create AGENTS.md / SYSTEM.md. Choose **Everyone** (`~/.pi/agent`) or **This project** (`.pi/`). pi reads settings when a session starts; **Restart sessions** applies them (busy sessions restart when their run ends).

Each pi session shows its cost, context use, thinking level and model at the top right of the chat; click the model or the thinking level to change it for that session. **pi → Compact Session** summarizes a long conversation to free context. Type `/` in the message box to see pi's commands (extension commands, prompt templates, `skill:` commands); Tab or a click completes one. Type `@` and part of a file name to mention a project file (`@src/main.rs`). Paste a screenshot (Cmd+V with an image on the clipboard) to send it with your next message. **Ask pi** buttons in the Run panel and on each problem put the output or the error in the message box, ready to send.

**Changes** (in the pi panel) lists the files a session changed; click one for a side-by-side or inline diff against the file before pi touched it. **Revert** undoes the whole file; the ↶ button beside each change in the diff undoes just that change; **Mark all reviewed** / **Revert all…** act on the whole session. The list is saved with the session and comes back when you resume it. A file pi changes while it's open in the editor reloads as an undoable edit (Cmd+Z takes it back). **pi → Export Session as HTML** writes the conversation to a page and opens it; **pi → Rename Session…** names it. Unsaved files are saved before a message goes to pi (setting).

## Previews

- **View → Markdown Preview** opens a live preview beside a Markdown file.
- **PDF files** open in a PDF tab (zoom, fit width, search with highlights). The first time, click **Install pdfium**: pide downloads the pinned pdfium build (about 4 MB) into `~/.local/share/pide/pdfium`.
- **Images** (PNG, JPEG, GIF, WebP, SVG, …) open in an image tab.
- **File → Open in Browser** for HTML and SVG.

## Language servers

pide starts rust-analyzer for `.rs` files, pyright for `.py` files, typescript-language-server for `.ts` files and clangd for C, C++ and CUDA (`.c .h .cpp .cc .cxx .hpp .hxx .inl .cu .cuh` …). clangd needs a `compile_commands.json` (CMake: `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`); when there's none in the project root or `build/`, pide passes clangd the newest one in a build folder (`out/build/<preset>/`, `build*/`, `cmake-build-*/`). Choose one yourself with `"lsp": { "clangd": { "args": ["--compile-commands-dir=out/build/Debug"] } }` or a `.clangd` file with `CompilationDatabase:`. On Windows pide installs clangd with `winget install LLVM.LLVM`, and starts it in Visual Studio's developer environment (what `vcvars64.bat` sets, found with vswhere; skipped when pide itself runs in a Developer Prompt), so MSVC projects find ATL/MFC and the rest of the toolset's headers, as in CLion. **Settings → Language Servers…** shows what's installed, installs what's missing (`rustup component add rust-analyzer`, `npm i -g pyright`, `npm i -g typescript-language-server typescript`) with the output on the page, and restarts servers. A server that crashes restarts by itself up to 3 times in 3 minutes. Override a server's command in `settings.json`: `"lsp": { "rust-analyzer": { "command": "/path", "args": [] } }`.

Syntax highlighting: Rust, Python, TypeScript/TSX, JavaScript, JSON, TOML, YAML, Bash, HTML, CSS, Markdown, C, C++ (CUDA too), CMake.

**Settings…** (Cmd+,) has switches for all of these. Settings live in `~/.config/pide/settings.json` (`"theme": "dark" | "light" | "system"`, `"agent.send_context": true`, `"agent.notify": true`, `"agent.save_before_send": true`, `"editor.format_on_save": false`); the Find Action commands "View: Theme: …" change them. Edits to the file apply as soon as it is saved. With `agent.notify` on, pide posts a macOS notification when pi finishes or asks a question while you are in another app (shown as coming from Script Editor; macOS asks once whether to allow it). With `agent.save_before_send` on, unsaved files are saved before a message goes to pi, since pi reads files from disk.

## CMake

The **CMake** tool window (bottom dock; **Build** menu, or View → Tool Windows) works like CLion's:

- **Profile** (the ⚙ picker): one per configure preset in `CMakePresets.json` / `CMakeUserPresets.json` (with `inherits`, `include`, `condition` and the `${sourceDir}`-style macros); without presets, `Debug` and `Release` in `cmake-build-debug` / `cmake-build-release` (Ninja when it's there). The build preset that names the profile's configure preset is used for building.
- **Reload CMake Project** (⟳) configures the profile with `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON` and asks CMake's File API for the code model: the **target** picker lists its executables and libraries. **Reset Cache and Reload Project** (⋯) deletes `CMakeCache.txt` and `CMakeFiles/` first. Saving a `CMakeLists.txt`, `*.cmake` or preset file shows a notice to reload; nothing reloads by itself.
- **Build** (Ctrl+F9 / ⌘F9) builds the selected target, or all targets; a profile that was never configured is configured first. **Rebuild** and **Clean** are in ⋯ and the Build menu.
- **Run** (Shift+F10) builds the selected executable target, then runs it in the project folder with its **Run Arguments…** (⋯). **Stop** ends what runs; starting something new stops the old one (a running program would keep its `.exe` locked).
- Errors and warnings in the output (MSVC, nvcc, clang, gcc, CMake, linkers) are listed on the left; click one, or a `file(line)` in the output, to open it there.
- clangd follows the selected profile: it reads that build folder's `compile_commands.json` (unless `settings.json` gives clangd its own `--compile-commands-dir`).
- On Windows everything runs in Visual Studio's developer environment (vswhere → `vcvars64.bat`), as CLion's Visual Studio toolchain does, so `cl`, `link` and `ninja` are found without a Developer Prompt.

pide remembers the profile, target and run arguments per project (`~/.local/state/pide/cmake/`).

## Terminal

The **Terminal** tool window (Option+F12 / Alt+F12, bottom dock) runs your shell in the project folder: PowerShell 7 (`pwsh`, else Windows PowerShell) on Windows, your login shell (`$SHELL -l`) on macOS and Linux. Each **+** is another shell in its own tab; a tab takes the title the shell gives it.

- Full-screen programs work: vim, less, htop, lazygit, with colours (256 and true colour) and the mouse. Hold Shift to select text in a program that takes the mouse.
- While the terminal has focus, keys go to the shell: Ctrl+R, Ctrl+E, Ctrl+K and Esc are the shell's, not pide's. What still works: Find Action, Go to File, pide settings, the tool-window keys with a digit or an F-key, Option+F12 / Alt+F12, Shift+Esc (hide it).
- **Copy** / **Paste**: ⌘C / ⌘V on macOS, Ctrl+Shift+C / Ctrl+Shift+V elsewhere. Ctrl+C copies when something is selected (otherwise it interrupts, as usual). Drag to select, double-click for a word, triple-click for a line. Pasting several lines doesn't run them one by one where the shell supports bracketed paste.
- ⌘-click (Ctrl+click) a `file:line` or `file:line:col` in the output to open it there; relative paths are taken from the folder the shell started in.
- Right-click: Copy, Paste, **Ask pi About This** (the selection, or the screen, into pi's message box), Clear, New Tab, Close Tab.
- In the project tree, right-click a folder or file → **Open in Terminal** for a shell there.
- `exit` closes the tab; closing the last one hides the tool window, and opening it again starts a fresh shell. A shell that ends with an error keeps its tab, so you can read why.

Settings (`settings.json`):

- `"terminal.shell"`: another shell, `"bash"` or with arguments, `["C:\\Program Files\\Git\\bin\\bash.exe", "-l"]`.
- `"terminal.font_family"`: the editor's monospace font by default. Prompt themes like oh-my-posh and starship need a Nerd Font for their icons, e.g. `"CaskaydiaCove Nerd Font"`.
- `"terminal.font_size"`: the editor's size by default.

## Where pide keeps its files

- Logs: `~/.local/state/pide/pide.log`, pi traffic `~/.local/state/pide/pi-rpc.jsonl`, pi stderr `~/.local/state/pide/pi-stderr.log`.
- Language servers: every message in `~/.local/state/pide/lsp-rpc.jsonl`, stderr in `lsp-<server>-stderr.log`.
- Saved layouts: `~/.local/state/pide/layouts/`.
