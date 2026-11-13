# Colors — audit and rules (2026-11-12)

The eye goes to the brightest, most saturated thing on screen. Before this pass
that was usually a *setting* (a toggle, a checkbox, a chip) or a decoration
(red Revert buttons, a solid yellow bar), not the content or the one action
that matters. This file records what was wrong and the rules that replaced it.
The rules live in code in `pide_core::colors`.

## Root cause: GPUI Kit's theme names don't mean what they sound like

In GPUI Kit's default **dark** theme:

| Token | Actual color | What we assumed |
| --- | --- | --- |
| `primary` | `neutral-50`, i.e. **white**, the same as `foreground` | "brand / accent color" |
| `accent` | `neutral-800`, the **same gray as `border` and `secondary`** | "highlight" |
| `link` | `#fafafa`, **white** | "blue link" |
| `danger` / `warning` / `success` | red-400 / yellow-400 / green-400, saturated | fine for text, much too loud as fills |
| `list_active` | `#1e40af33`, a blue tint | correct for selection |
| `blue`, `green`, `red`… | base palette 400s | real hues |

So every `primary()` button and every `.text_color(theme.primary)` was a solid
white block or white text, the loudest thing in its view. Every
`.bg(theme.accent)` "selected" row looked the same as a hovered row.

## Findings

1. **Toggles shouted.** Chat/Changes, Side by side/Inline, the Context chip,
   search options, PDF Fit width, theme picker, pi Settings scope and thinking
   level: the *selected* option was a `primary()` (white) button, so the
   brightest thing in the pi panel was "Context: no file open".
2. **Checkboxes shouted.** The Commit window filled every checked box white:
   a column of white squares louder than the file names.
3. **The busy dot was white** and the active pi session tab had a white top
   border: "working" and "active" looked like "important".
4. **File status colors disagreed between panels.** Modified was blue in
   Commit and the Git popup, yellow (`warning`) in Git Log, pi's Changes and
   the file tree badges, and yellow in the editor gutter. Untracked was
   yellow ("warning"), deleted was red ("error").
5. **Destructive buttons on every row.** Each pi change had a red *Revert*,
   each package a red *Remove*. A list of red buttons draws the eye away from
   the list itself.
6. **Notice bars were solid fills.** The editor's read-only / conflict /
   deleted bars were full yellow or red with `*_foreground` text (red-600 on
   red-400: low contrast and alarming for a routine state).
7. **Hover = selected** in the pi session sidebar (`theme.accent` for both).
8. **Links in Run output were white** (`theme.link`), so only the underline
   told them apart.
9. **Labels in status colors.** The *Steer / Follow-up* tag on queued
   messages was yellow (it's a label, not a warning); "not installed"
   language servers were red (it's a state, and the Install button is the
   call to action); "starting" was yellow (transient, not a problem).
10. Small ones: the unsaved dot on editor tabs was yellow (warning); "set for
    this project" in pi Settings was white; the active parameter in signature
    help was "primary" = the same white as the rest of the signature (only
    bold set it apart); the status bar used `secondary` (border gray) instead
    of the theme's `status_bar`.

## Rules

**Neutral by default. A hue always means something, and the same thing
everywhere.** Hierarchy comes from contrast (foreground vs `muted_foreground`)
and weight first, hue second.

| Meaning | Color | API |
| --- | --- | --- |
| Body / content | `foreground` | — |
| Meta (paths, counts, hints) | `muted_foreground` | — |
| The one main action in a view (Commit, a dialog's OK, a banner's fix) | white fill | `Button::primary()` (only for that) |
| Selected option of a toggle / segmented control / chip | blue tint + body text; off = ghost | `colors::toggle(button, on, cx)` |
| Selected row | blue tint | `theme.list_active` |
| Hovered row | neutral | `theme.list_hover` (never `theme.accent`) |
| Accent: checked checkbox, active tab marker, links, "set here", work in progress (busy dot, starting) | blue | `colors::accent(cx)` |
| File status | modified blue · added green · deleted gray · untracked red · conflict red (JetBrains) | `colors::change(Change::…, cx)`, `Change::from_letter` |
| Problem states | error `danger`, warning `warning`, ok `success`, as text or a small dot | — |
| Inline notice bar | tint of danger/warning, body text | `colors::notice_bg(error, cx)` |
| pi's notes in the code (collaborator mode): gutter/scrollbar marks, the editor's note bar (tint), "to review" chip | magenta | `colors::pi_note(cx)` |
| Destructive action | plain button in lists; red only for the final confirm (or Stop) | — |
| Line counts (`+3 −1`) | green / red, 12 px | — |
| Git refs in the Log | HEAD yellow, local green, remote magenta, tag cyan (tinted chips) | — |

Never use `theme.primary` or `theme.link` as a color, or `theme.accent` for
selection: in the dark theme they are white, white and border-gray.

## What changed

- `crates/core/src/colors.rs`: `accent`, `change`/`Change`, `toggle`,
  `notice_bg`.
- Toggles: all `when(x, |b| b.primary())` → `colors::toggle`.
- Commit checkboxes, pi session tab marker, busy dot, Run links,
  "set for …", LSP "starting" → accent blue.
- File status colors through `colors::change` in Commit, Git popup, Git Log,
  pi Changes, file tree badges; gutter "modified" → blue.
- Per-row Revert / Remove and *Revert all…* → plain buttons (*Revert all…*
  still confirms); sidebar *New* → ghost.
- Editor notice bars → tinted with body text; unsaved dot → muted.
- pi session sidebar: hover `list_hover`, active `list_active`.
- Queued-message tag, "not installed" → muted. Status bar → `status_bar`.
- Signature help: signature muted, active parameter bold body color.

## Left as is (on purpose)

- Syntax highlighting and GPUI Kit's diff colors (theme-owned).
- Search match highlight (`warning` at 35 %): a highlight should pop.
- Stop buttons stay red (`danger`): the action is "kill what's running now".
- pi's "waiting for you" dialog card keeps its yellow border: it does need you.
