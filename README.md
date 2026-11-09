# pide

A personal, native code editor in Rust on GPUI + GPUI Kit, with the pi coding agent built in.
The spec lives in the claude.ai doc "pi IDE — Implementation Spec". Current state: **M0 spike** — see `docs/SPIKE.md`.

## Build requirements

- Rust >= 1.92 (developed on 1.98). Versions are pinned in `docs/VERSIONS.md`.
- macOS: Xcode command line tools.
- Linux (primary target): a Wayland or X11 session with a working Vulkan driver. GPUI Kit may also need
  `libwebkit2gtk-4.1-dev` at build time. Recommended: `mold` for linking. Full package list to be confirmed in M1.

## Layout

- `spike/` — M0 throwaway benchmark binary (`cargo run --release -- --bench`).
- `docs/` — `SPIKE.md`, `VERSIONS.md`.
- `reference/` — upstream GPUI Kit examples at the pinned tag (not built).
