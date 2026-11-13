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
7. **Cmd+K** opens the Commit tool window: staged and unstaged files, click one for the diff against HEAD (the ↶ in the diff rolls just that change back), **+** / **−** stage and unstage, ↶ rolls a file back. Write a message and **Cmd+Enter** commits; **Amend** rewrites the last commit. In the editor, **Cmd+Alt+Z** does the same for the change under the cursor.
8. **Cmd+9** opens the Git Log at the bottom: every branch with its graph, click a branch or tag on the left to see only its history, type to search (text, hash, author). Click a commit for its message and files, a file for that commit's diff; ↑/↓ move; **Ask pi: explain** drafts a question about the commit.
9. The status bar shows the branch with **↓n** (click to pull) and **↑n** (click to push). Click the branch for the Git popup: Update Project (**Cmd+T**), Push… (**Cmd+Shift+K**), Fetch, New Branch…, and every branch: Enter on one for Checkout, New Branch from…, Show in Git Log, Merge into / Rebase onto the current branch, Rename, Delete. Push… lists the outgoing commits and their files first (and warns when the remote has commits you don't).
10. In a file, **Code → Annotate with Git Blame**: a tab with who changed each line and when (newest commits shaded strongest); click a block for that commit's message, **Show in Git Log**, **Diff**, or **Why this change? (pi)**. **Code → Why This Line? (pi)** asks about the cursor line directly.
11. Right-click in the project tree: New File, Rename, Move to Trash, Copy Path, Reveal in Finder. **Option+F1** selects the open file in the tree.

## Tool windows

1. The thin stripes on the window's edges hold the tool windows: Project and Commit on top left; Git Log, Run, Problems, Find at the bottom left (they open in the bottom dock); pi on the right. Click one to show it, click it again to hide it. Keys: **Cmd+1** Project, **Cmd+K** Commit, **Cmd+9** Git Log, **Cmd+3** Find, **Cmd+4** Run, **Cmd+6** Problems, **Cmd+0** pi.
2. Right-click a stripe button: **Move to Left / Right / Bottom**. The move is remembered with the layout.
3. **Shift+Esc** hides the tool window you're in; **Cmd+Shift+F12** hides them all (again brings them back).

## pi

1. **Cmd+Shift+I** jumps to the pi box (Esc goes back). The header shows cost, context use, thinking level and model; click the model or the thinking level to change them for this session.
2. Type `/` for pi's commands, `@` and part of a file name to mention a file. Paste a screenshot with Cmd+V.
3. Ask pi to change something. While it works, keep editing elsewhere; a file it changes while open reloads as an undoable edit.
4. **Changes**: click a file for the diff, ↶ reverts one change, **Revert** the file, **Revert all…** the session. Changes it makes through `bash` (sed, formatters) are listed too.
5. Quit and reopen pide, resume the session from the ▾ menu: its Changes list is still there.
6. **Collaborator mode** (docs/COLLAB.md): pi explains each file it changed, can pin a spot it wants you to look at, and keeps a checklist for longer tasks (PLAN, above the chat). **Changes → Walk through** (or **pi → Walk Through Changes**) opens each note in the editor: a magenta mark in the gutter and a bar above the editor with **Reply** (drafts "file:line: " into that session), **Dismiss**, **Back**, **Next**. The diff tab lists pi's notes by hunk number. Sessions with notes you haven't stepped through are listed first in the ▾ menu, and the status bar shows "◆ n to review": click it to start.

## Settings

- **Cmd+,**: theme and switches (editor context, save before sending, notifications, reformat on save).
- **Settings → Keymap…**: change any shortcut by typing it.
- **Settings → pi…**: packages, tools, MCP servers, default model.

## Needs your decision

- PDF: open a PDF; on macOS it shows straight away (PDFKit), on Linux click **Install pdfium** first. Zoom, Fit width, search with highlights.
- M6 scripted panels: see `docs/M6-SCRIPTED-PANELS.md`.
- Comment / duplicate / move line: see `docs/GAPS.md` (JetBrains line commands).

## LaTeX

Needs a TeX install with latexmk (macOS: `brew install --cask basictex`, then `sudo tlmgr install latexmk` plus the packages your documents use) and texlab (Settings → Language Servers → Install, or `brew install texlab`).

- `.tex` / `.sty` / `.cls` files are highlighted (commands, environments, section titles, labels and citations, math, comments, bold/italic) and soft-wrap like Markdown.
- **Cmd+S builds.** texlab runs `latexmk -pdf -synctex=1` on the document the file belongs to (the file itself if it has `\documentclass`, else the document that `\input`s it, or a `% !TEX root = main.tex` comment). The PDF opens beside the source the first time, then reloads in place; the focus stays in the editor.
- LaTeX errors and warnings from the log show in **Problems** (Cmd+6) with Ask pi; a build with errors also shows a notice.
- **SyncTeX:** Cmd+Alt+Shift+J (Code → LaTeX: Show in PDF) scrolls the PDF to the cursor's line and marks it. **Cmd+click** in the PDF jumps to that source line.
- The PDF tab also reloads when the file changes from outside (latexmk in a terminal).
