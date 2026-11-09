# M0 spike — GPUI Kit editor on a 50K-line Rust file

**Verdict: the editor is fast enough to build on, on this machine.** No frame took longer than 16 ms in any scripted phase:
scrolling, fast scrolling, typing in the middle of the file, or a 1,000-line paste. Memory stays around 115 MB.

**Caveat that matters:** this was measured on **macOS 15.0, Apple M1, Metal**, not on the spec's primary target
(Linux, Vulkan). It shows GPUI Kit's editor design holds up. It does not show the Linux renderer does. Re-run the same
benchmark on your Linux machine before signing off (see "Run it yourself").

Date: 2026-11-09 · `gpui-kit` 0.9.0 (GPUI `gpui-pre` 0.3.11) · Rust 1.98.0 · release build.

## Setup

- `spike/` is a throwaway binary (not part of the future workspace). It opens one file in GPUI Kit's `Editor`
  (`EditorState` with `language("rust")`, line numbers, indent guides) using the `tree-sitter-rust` feature.
- Fixture: `spike/fixtures/large.rs`, 50,000 lines / 1.7 MB of real Rust (GPUI's own sources concatenated), so the
  highlighter sees realistic code, not one repeated line.
- Numbers come from GPUI's built-in profiler (`profiler` feature, `FrameTimingCollector` and the per-window
  histograms), not from timing outside the framework. "Draw" = time spent in `Window::draw` on the UI thread;
  "change→drawn" = time from the first invalidation to the end of that frame's draw.
- `--bench` scripts each phase at one step per 16 ms through the editor's public API (`set_scroll_offset`, `insert`).

## Results

| Phase | Frames | Draw p50 / p95 / max (ms) | Frames > 16 ms | Change→drawn p95 (ms) | Editor call p95 (ms) |
| --- | --- | --- | --- | --- | --- |
| Settle: 3 s idle after open, includes initial parse | 58 | 1.95 / 2.93 / 5.25 | 0 | 16.8 | — |
| Smooth scroll: 3 lines/frame, 1,000 frames | 999 | 3.31 / 4.59 / 12.24 | 0 | 15.6 | 0.02 |
| Fast scroll: 60 lines/frame, whole file | 833 | 4.68 / 6.76 / 13.20 | 0 | 15.2 | 0.01 |
| Typing: 400 chars at line 25,000 | 404 | 2.89 / 5.00 / 12.59 | 0 | 20.7 | 2.78 (max 6.0) |
| Paste 1,000 lines at the cursor | 23 | 1.51 / 3.94 / 5.42 | 0 | 20.6 | 1.97 (single call) |

- **Input latency.** GPUI recorded 145 real OS input events during the run (mouse or keyboard; the scripted inserts don't count). Input→frame: p50 10.8 ms,
  p95 16.1 ms, max 17.1 ms, about one frame at 60 Hz. Each scripted `insert` costs about 1–3 ms on the UI thread,
  including the incremental tree-sitter update.
- **Scroll smoothness.** Frames presented at 16.7 ms intervals, p95 19 ms, and none over 25 ms (no visibly dropped
  frames). The display is 60 Hz, so this says nothing yet about 120 Hz.
- **Memory (RSS).** 49 MB with the window open → 112 MB after the first parse and layout → 117 MB after all phases.
- **Startup.** Process start to first frame: 1,050 ms on the very first (cold) launch, then **208–296 ms warm**.
  Reading the file takes 1 ms and building `EditorState` takes 1–4 ms; the rest is app, window and GPU setup.
  M1's target is under 1 s; the cold launch misses it slightly, so re-check on Linux.
- **Worst frame in the whole run:** 45.9 ms. It wasn't in any measured phase, so it's the first frame
  (initial layout and font loading) before the benchmark started.

## Broken or surprising

1. **Dependency resolution fails with GPUI's `profiler` feature.** It pulls `gpui-fast`, which pins `taffy =0.15.0`,
   but a fresh lockfile picks 0.15.1. Fix: `cargo update -p taffy --precise 0.15.0`. Recorded in `docs/VERSIONS.md`.
   Only matters if we keep the profiler on in M1, which is how we'd meet the spec's "log frames over 16 ms" rule.
2. **Build time.** A cold debug build took about 10 minutes and a cold release build 21 minutes (about 800 crates) on
   an M1. Incremental builds are seconds. M1 should set `[profile.dev.package."*"] opt-level = 2` and use `mold` on Linux.
3. **The editor lives in `gpui-base`, not `gpui-component`.** `EditorState` is defined in `gpui_base::input` and
   re-exported as `gpui_kit::component::input::EditorState`. The buffer is `ropey` **2.0.0-beta.1** (a beta).
   Its API differs from ropey 1.x (`len_lines` takes a `LineType`), so check any rope code against 2.0.
4. **The crates.io packages ship no examples.** The examples are vendored from the `v0.9.0` git tag into `reference/`.
5. **pi version.** pi 0.79.6 is installed, and the spec's RPC notes are for v0.83. That doesn't affect M0, but it
   should be upgraded (or its own `docs/rpc.md` read) before M1.

## Manual check (macOS) — all passed

Checked by hand by the user on 2026-11-09 (the terminal couldn't screenshot the window, so these were confirmed by
eye, not by me):

- [x] Rust syntax highlighting, including deep in the file and while scrolling fast.
- [x] Folding from the gutter chevrons.
- [x] Multi-cursor (Cmd+Alt+↓ / Alt+click) and block selection.
- [x] In-file search (Cmd+F) and replace (Cmd+Shift+F).
- [x] Typing, scrolling and undo feel right by hand.

## Run it yourself

```sh
cd spike
cargo run --release -- --bench            # scripted; writes spike/results-<os>-<arch>.md, then quits
cargo run --release                       # manual; stats printed when you close the window
cargo run --release -- path/to/file.rs    # any other file
```

On Linux, run it under both Wayland and X11 (see the spec's open question on GPU and session type).
NVIDIA on Wayland is the riskiest setup.

## Recommendation

Go ahead with M1 on GPUI Kit's editor, once the benchmark has been re-run on your Linux machine.
If Linux shows frames over 32 ms while scrolling, stop and revisit before building panels on top of it.
