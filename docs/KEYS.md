# Keyboard shortcuts

JetBrains-style. On macOS as written; on Linux, Cmd is the Super key in pide and Option is Alt.

Every command is also in **Find Action** (Cmd+Shift+A), with its shortcut beside it.

## General

| Key | Command |
| --- | --- |
| Cmd+Shift+A | Find Action (every command) |
| Cmd+, | Settings (every page: theme, editor, keymap, tool windows, script panels, languages, pi) |
| Cmd+S | Save |
| Cmd+W / Cmd+Shift+T | Close tab / reopen the last closed tab |
| Cmd+1 / Cmd+6 | Project panel / Problems panel |
| Ctrl+Alt+R | Run a shell command in the project folder (Run panel: output with clickable `file:line`, Stop, Rerun, Ask pi) |
| Cmd+Q | Quit |

## Navigation

| Key | Command |
| --- | --- |
| Cmd+Shift+O | Go to File |
| Cmd+E | Recent files (Enter flips to the previous file) |
| Cmd+[ / Cmd+] | Back / forward (after Go to File, Find Usages, a declaration in another file…) |
| Cmd+L | Go to line (`12` or `12:5`) |
| Option+F1 | Select the open file in the project tree |
| Cmd+Shift+F | Find in Files (replace too; file mask `*.rs, !*.lock`) |
| Cmd+F / Cmd+R | Find / replace in the file |
| Cmd+B | Go to declaration (also Cmd+click) |
| Cmd+Alt+B / Ctrl+Shift+B | Go to implementation / type declaration |
| Option+F7 | Find usages |
| Cmd+F12 / Cmd+Option+O | File structure / go to symbol in the project |
| F2 / Shift+F2 | Next / previous error in the file |

## Code (in the editor)

| Key | Command |
| --- | --- |
| Option+Enter | Quick fixes |
| F1 / Ctrl+J | Quick documentation |
| Cmd+P | Parameter info |
| Shift+F6 | Rename |
| Cmd+Alt+L | Reformat file |
| Ctrl+Option+O | Optimize imports |

## Git

| Key | Command |
| --- | --- |
| Cmd+9 | Local changes; Enter shows a file's diff against HEAD (↶ rolls back one change) |
| Cmd+Alt+Z | Roll back the change under the cursor (undoable) |
| Ctrl+Alt+Shift+↓ / ↑ | Next / previous change |

## pi

| Key | Command |
| --- | --- |
| Cmd+Shift+I | Ask pi: the pi panel, cursor in its box (with the editor context) |
| Esc (in the pi box) | Back to the editor |
| `/` · `@` (in the pi box) | pi's commands · mention a project file (Tab completes) |
| Enter / Option+Enter (while pi works) | Steer / queue a follow-up |
| Cmd+F2 | Stop pi |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous session |

View → Move Tab to Right Split puts two files side by side.

## Changing shortcuts

**Settings → Keymap…** lists every command with its shortcut; pick one and type a new shortcut (`cmd-shift-k`). It's saved to `~/.config/pide/keymap.json`, where you can also override or add bindings by hand (Find Action → "Preferences: Open Keymap File"); it reloads on save:

```json
{ "cmd-alt-p": "palette.open", "cmd-1": null }
```
