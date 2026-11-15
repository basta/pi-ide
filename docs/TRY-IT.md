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
6. pi can check the errors your editor shows (`ide_diagnostics`) and fixes them before it's done.

## Ten minutes with collaborator mode (following pi's code)

You don't write the code; this is how you keep up with it. Details: docs/COLLAB.md.

1. **What's here.** Open a new session (or click **?** in the pi toolbar): the Understand card lists the
   five ways to follow the code and what the marks in the editor mean. Click any row to run it.
2. **Understand something first.** **pi → Take a Tour…**, ask "how does X work?". pi answers in a
   session of its own with a tour card (numbered stops across files). **Start tour**: the editor opens
   at stop 1 with a bar above it (pi's note, **Back** / **Next**). Select code and **Code → Explain
   This Code (pi)** for a smaller question.
3. **Ask for a change.** Type it in the pi box. For a longer task pi shows a PLAN checklist above
   the chat. When it's done, each changed file has a one-line reason in **Changes**, and pi may pin a
   spot it wants you to decide on (a PINNED card in the chat, with the code).
4. **Review it where it happened.** Click **◆ n to review** in the status bar (or **Walk Through
   Changes**): the editor goes to pi's first note (the line often lands at the top or bottom edge of
   the view; GPUI Kit can't centre it yet); magenta marks in the gutter are pi's notes,
   blue/green are git's. **Next** steps through every note across files; stepping through a file's
   last note ticks it reviewed. **Reply** puts "file:line (about your note "…"): " in the pi box, so
   you answer in place. A file pi changed without saying why says **no reason given · ask why**.
5. **See the whole change.** **Tell It as a Story** (Changes, or the pi menu) retells the change as a
   tour in the order that explains it. **Catch Me Up** writes a short briefing (asked / done / needs
   you / start here); handy when you come back to a session later.
6. **Commit.** The Commit tool window (**Cmd+K**) marks files with **◆ n pi notes** you haven't stepped
   through; click to walk them. **✦ Draft with pi** writes the message.
7. **Later.** Tours stay in **▾ → TOURS**. **pi → pi's Notes in This File** lists every session's
   notes on the file you're in, also from closed sessions.

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

## Typst

Needs tinymist, which bundles the Typst compiler (Settings → Language Servers → Install, or `brew install tinymist`). No separate `typst` install.

- `.typ` files are highlighted (headings, `#` code: calls, `let`/`set`/`show`, strings and numbers, math, labels and references, raw, bold/italic, comments) and soft-wrap like Markdown.
- Completion, hover, go to definition, outline and diagnostics come from tinymist. Compile errors show in **Problems** as you type.
- **Cmd+S exports the PDF** (`tinymist.exportPdf`) for the document the file belongs to: the `.typ` that `#include`s it (same folder or above), else the file itself. The PDF opens beside the source the first time, then reloads in place.
- No forward/inverse search yet (tinymist has no SyncTeX).
