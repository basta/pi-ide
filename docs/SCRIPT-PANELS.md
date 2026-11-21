# Script panels

A script panel is a tool window written in JavaScript. pide runs it with
[gpui-shell](https://crates.io/crates/gpui-shell) (QuickJS): the script
describes GPUI elements and Rust draws them, so it looks and behaves like a
built-in panel. No rebuild of pide: save the file and the panel reloads.

This file is also the guide pi reads before writing a panel (pide copies it to
`~/.local/state/pide/builtin-panels/GUIDE.md`).

## Where panels live

| Folder | Shows in |
| --- | --- |
| `<project>/.pide/panels/<name>/` | that project (it's in the repo, so it shows in Review like any change) |
| `~/.config/pide/panels/<name>/` | every project |
| shipped with pide (`crates/panels/scripts/builtin/`) | every project; written to `~/.local/state/pide/builtin-panels/` |

`<name>` is lower case: `a-z`, `0-9`, `-`, `_`. Each panel is a tool window: a
stripe button, a `view.script.<scope>.<name>` command in Find Action, and its
place in the saved layout. **Script Panels: Look for New Panels** picks up a
folder created while pide runs.

```text
.pide/panels/crate-map/
  panel.json     what it is and what it may do (optional)
  main.js        export default class … extends View
```

## panel.json

```json
{
  "title": "Crate map",
  "dock": "right",
  "icon": "map",
  "key": "cmd-alt-m",
  "permissions": { "write": false, "run": ["cargo"], "network": [] }
}
```

- `dock`: `left`, `right` (default) or `bottom`.
- `icon`: `triangle-alert`, `map`, `search`, `terminal`, `folder`, `bot`, `github`, `refresh`; anything else is a file icon.
- `permissions`: reading the project is always allowed. `write` lets it write
  project files; `run` names the programs it may start (`["*"]` = any);
  `network` the hosts it may reach. Everything else is denied.

The first time a panel runs, pide shows what it asks for and waits for **Allow
and Run** (shipped panels skip this). A `panel.json` that asks for more asks
again. Grants: `~/.local/state/pide/panel-grants.json`.

## main.js

```js
import { View, div } from "gpui-kit";          // View, div, the GPUI style methods
import { h_flex, v_flex } from "gpui-base";    // layout helpers
import { Button } from "gpui-component";       // ready-made components
import { diagnostics, nextEvent, openFile, colors } from "pide";

export default class Example extends View {
  init(_props, cx) {
    this.items = diagnostics();
    // React to the project: loop on nextEvent in a task, notify to re-render.
    cx.spawn(async (cx) => {
      for (;;) {
        const e = await nextEvent();
        if (e.kind === "diagnostics") { this.items = diagnostics(); cx.notify(); }
      }
    });
  }
  render(cx) {
    const c = colors();
    return v_flex().size_full().bg(c.background).text_color(c.foreground)
      .child(div().px_3().py_1().text_xs().text_color(c.muted).child(`${this.items.length} items`))
      .child(v_flex().id("list").flex_1().overflow_y_scroll().children(
        this.items.map((d, i) => h_flex().id(`row-${i}`).px_3().py_0p5().cursor_pointer()
          .hover((s) => s.bg(c.list_hover))
          .on_click(() => openFile(d.path, d.line, d.column))
          .child(d.message))));
  }
}
```

- `render` runs again only after `cx.notify()`; between renders Rust redraws on its own.
- State lives on `this`. `init` gets an async context for `cx.spawn`, `cx.sleep(ms)` and timers.
- Elements with `on_click`, `hover` or scrolling need an `.id(…)`.
- Text sizes: `text_xs` (12, meta), `text_sm` (14, body), `text_base` (16, titles). Weight with `font_weight(600)`.
- Use `colors()` for every color, so the panel follows pide's theme.
- After the first load, `gpui-kit.d.ts` and `jsconfig.json` appear beside
  `main.js`: the complete API (every element, style method and component, and
  the `pide` module) with documentation. Read it rather than guessing a name.
- The shipped Problems panel (`builtin-panels/problems/main.js`) is a complete example.

## The `pide` module

Paths are relative to the project root; lines and columns are 1-based.

| Call | |
| --- | --- |
| `root()` | the project root (absolute) |
| `activeFile()` | `{ path, line, column, selection }` of the active editor tab, or `null` (line is `null` when called from inside a click) |
| `openFile(path, line?, column?)` | open in an editor tab |
| `diagnostics()` | `[{ path, line, column, endLine, endColumn, severity, message, source }]` |
| `await readFile(path)` | the open editor's text (unsaved edits included) or the file on disk |
| `await listFiles()` | project files, gitignore-aware (≤ 20000) |
| `runCommand(id)` | run any pide command (`view.problems`, `git.pull`, …); false if unknown |
| `askPi(text)` | put text in the pi box, focused, not sent |
| `colors()` | theme colors as `#rrggbb`: `background foreground muted border accent selection list_hover danger warning info success pi` |
| `await nextEvent()` | the next project event: `{ kind, path }`, kind `diagnostics`, `saved`, `changed`, `active`, `files` or `git` |

gpui-shell's own modules work too, within the panel's permissions: `fs/promises`
(read the project; write with `write`), `process` / `spawn` (with `run`),
`fetch` (with `network`), `window.localStorage` (always, per panel).

## When it breaks

A script that throws while loading shows the error with **Ask pi to Fix** (puts
the error and the folder in the pi box), **Open main.js** and **Retry**. A save
that breaks a running panel keeps the last working version on screen with the
error over it.

## Not yet

- A project's panels are registered for every open window (they load in the
  window you open them in).
- No pide component kit yet beyond `colors()`; no embedding of pide's editor,
  diff or PDF views.
- `pide` can't tell which window called it, so it finds the project through the
  panel's read grant (crates/panels/scripts/src/host.rs `with_project`).
- Toasts and dialogs from `gpui-component` need gpui-shell's own window root,
  which a panel inside pide doesn't have.
