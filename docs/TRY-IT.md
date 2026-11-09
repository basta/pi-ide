# Trying the night's work (10 minutes)

Build and open a project you know:

```sh
cargo run -p pide --release -- ~/some/project
```

## Editor

1. **Cmd+Shift+O**, type part of a file name, Enter. Then **Cmd+E**: the file you were in before is first; Enter flips back.
2. In a Rust file, put the cursor on a function and press **Option+F7** (usages), **Cmd+B** (declaration), **Cmd+[** to come back, **F1** (docs).
3. Type the start of something from another module (`HashMa`) and pick it from completion: the `use` line is added.
4. Make a file messy, **Cmd+Alt+L** reformats it; Cmd+Z undoes that.
5. **F2** jumps to the next error. In the Problems panel (**Cmd+6**), **Ask pi** drafts a fix request.
6. **Ctrl+Alt+R**, run `cargo check` (or your tests): output streams into the Run panel; click a `file:line` to jump; **Ask pi** sends the output to pi.
7. **Cmd+9** lists your uncommitted files; Enter shows the diff against HEAD; the ↶ next to a change rolls just that change back. In the editor, **Cmd+Alt+Z** does the same for the change under the cursor.
8. Right-click in the project tree: New File, Rename, Move to Trash, Copy Path, Reveal in Finder. **Option+F1** selects the open file in the tree.

## pi

1. **Cmd+Shift+I** jumps to the pi box (Esc goes back). The header shows cost, context use, thinking level and model; click the model or the thinking level to change them for this session.
2. Type `/` for pi's commands, `@` and part of a file name to mention a file. Paste a screenshot with Cmd+V.
3. Ask pi to change something. While it works, keep editing elsewhere; a file it changes while open reloads as an undoable edit.
4. **Changes**: click a file for the diff, ↶ reverts one change, **Revert** the file, **Revert all…** the session. Changes it makes through `bash` (sed, formatters) are listed too.
5. Quit and reopen pide, resume the session from the ▾ menu: its Changes list is still there.

## Settings

- **Cmd+,**: theme and switches (editor context, save before sending, notifications, reformat on save).
- **Settings → Keymap…**: change any shortcut by typing it.
- **Settings → pi…**: packages, tools, MCP servers, default model.

## Needs your decision

- PDF: open a PDF, click **Install pdfium** (or ask for macOS PDFKit instead).
- M6 scripted panels: see `docs/M6-SCRIPTED-PANELS.md`.
- Comment / duplicate / move line: see `docs/GAPS.md` (JetBrains line commands).
