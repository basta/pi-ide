# Visual design: "Two voices" (2026-11-17)

pide is where you read what pi wrote. The look has one idea: **you can tell
pi's words from yours at a glance.** pi speaks in a serif; everything you
wrote, and the app itself, is in a sans and a mono. Everything else stays
quiet so the code, the prose and the PDF are what you look at.

Workshopped on the Design canvas (critique of the old look and three
directions): https://claude.ai/artifact/WtdbqA41pkd39VoCsBVFDF

## Type

| Role | Family | Where |
| --- | --- | --- |
| UI | Atkinson Hyperlegible Next | theme `font.family` |
| Code and prose files | Atkinson Hyperlegible Mono | theme `mono_font.family` |
| pi's voice | Newsreader (italic for asides) | `pide_core::colors::VOICE`: note lenses, note cards (`editor/src/pi_float.rs`), chat replies (`agent_chat/src/conversation.rs`) |

The fonts are bundled (`crates/app/fonts/`, SIL OFL 1.1, licenses beside them)
and registered at startup (`app/src/theme.rs`, `install`), so Linux and macOS
draw the same thing instead of DejaVu on one and SF/Menlo on the other.
Static instances from Google Fonts: Regular, Medium, SemiBold, Italic (UI);
Regular, SemiBold, Italic (mono); Regular, SemiBold, Italic (Newsreader).

## Color

`crates/app/themes/pide.json`: pide Dark and pide Light (GPUI Kit theme
format; keys not set fall back to GPUI Kit's defaults, the `highlight` block
is complete). Rules from docs/COLORS.md still apply.

| | Dark | Light |
| --- | --- | --- |
| Desk: tab bars, title bar, status bar, sidebars | `#1b2027` | `#eceff2` |
| Page: editor, panels | `#222831` | `#fbfbfc` |
| Raised: popovers, cards, the PDF backdrop | `#2a313b` | `#ffffff` / `#e6eaee` |
| Ink / muted / faint | `#e2e5e9` / `#939ca8` / `#5f6874` | `#1f252d` / `#5b6573` / `#8a939e` |
| Accent (selection, active, links) | `#79a7ee` | `#2f6fd0` |
| pi (ochre, the theme's yellow) | `#e3b65e` | `#946200` |
| Warning (orange, so it never reads as pi) | `#e8894a` | `#c25e14` |

Text is never pure white on black. Syntax colors are softened, and Typst and
Markdown headings are bold ink, not orange: a section title shouldn't be the
loudest thing in a prose file.

## Not done yet (ideas from the canvas)

- One header strip per pane: the pi panel still stacks pi / session tab /
  Chat-Changes.
- The PDF backdrop as its own darker/lighter "well".
