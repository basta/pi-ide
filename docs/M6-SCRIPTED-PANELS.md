# M6 — Scripted panels: options

Written 2026-11-10 (overnight). The spec says: if GPUI Kit's `gpui-shell` has
matured, make scripted panels a second implementation of `Panel`; if not, write
up the options instead of building one. **It has not matured yet**, so this is
the write-up. Nothing was built for M6.

## Where gpui-shell stands (0.9.1, published 2026-11-09)

- Its own README: *"This crate is at milestone M0: a feasibility baseline, not a
  stable interface."* Four releases since August (0.1.0 → 0.9.1), 788 downloads.
- **What works:** QuickJS scripts describe GPUI element trees (replayed in Rust,
  so repaints don't enter the VM); hot reload; a capability model (fs, network,
  storage, clipboard, process are denied by default and granted per plugin in
  `gpui-shell.json`); generated TypeScript declarations; scripts can now
  contribute dock panels (`DockArea.register_panel`).
- **What's missing for pide:** "the rest of the contribution registry and a CLI
  that uses `PluginManager`" — i.e. the *plugin* path a host like pide needs is
  not end-to-end yet; only standalone scripts run end to end. No multi-window.
- **Cost to adopt:** it requires `gpui-kit ^0.9.1` and `gpui-pre =0.3.12`;
  pide pins `gpui-kit =0.9.0` / `gpui-pre 0.3.11`. Adopting means upgrading
  the whole toolkit to a one-day-old release first.

## Options

| | gpui-shell (JS) | Rhai | WASM (wasmtime) |
| --- | --- | --- | --- |
| Language for panels | JavaScript / TypeScript | Rhai (Rust-like script) | Anything that compiles to WASM (Rust, JS via a runtime, …) |
| UI bridge to GPUI | **Provided** (elements, styles, gpui-component catalog) | We'd write it: a DSL → GPUI elements | We'd write it, plus a WIT interface |
| Hot reload | Provided | Easy (re-eval script) | Re-instantiate module; needs a build step |
| Sandboxing | Capability model provided | Good (no I/O unless we expose it) | Strongest (memory isolation, WASI caps) |
| pi writing panels | Good: models know JS/TS well; TS declarations exist | Weaker: niche language | Needs a toolchain per panel |
| Size / build cost | QuickJS + its crates; toolkit upgrade | Small, pure Rust | Large (wasmtime), slow builds |
| Maturity | M0, moving fast, same maintainers as GPUI Kit | Mature engine; our bridge would be new | Mature engine; our bridge would be new |

## Recommendation

**Wait for gpui-shell, and re-check at its next release.** It is the only option
where the hard part (turning script UI into GPUI elements, with styles, inputs,
hot reload and capabilities) is already done by the people who maintain our
toolkit, and JS/TS is the language pi writes best. Building a Rhai or WASM UI
bridge ourselves would be most of the work of gpui-shell again, and we'd own it.

Concretely, when gpui-shell's plugin path (`PluginManager`) lands:

1. Upgrade pide to the matching GPUI Kit release (on a branch; run the M0
   benchmark and the test suite).
2. Add a `ScriptPanel` that implements `pide_core::Panel` by hosting a
   gpui-shell runtime, loading `~/.config/pide/panels/<name>/` (its
   `gpui-shell.json` declares capabilities; pide shows them and asks before the
   first load).
3. Expose a small host module (open file, current selection, run a command,
   read workspace files) behind capabilities.
4. Add a pi skill that knows that host module and the TS declarations, so
   "pi, make me a panel that …" writes into `panels/` and hot reload shows it.

**If you want something sooner**, a narrow interim is possible without any
toolkit change: "panels" that are pi prompts plus a Markdown result rendered in
a tab (e.g. a TODO panel that pi refreshes). That is not scripted UI, so it
doesn't meet M6, but it covers the "ask pi for a panel" idea cheaply.

## Decision needed

- Wait for gpui-shell's plugin path (recommended), or
- upgrade GPUI Kit now and prototype on gpui-shell's standalone path, or
- build a Rhai bridge ourselves.
