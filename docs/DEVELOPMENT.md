# Developing pide

pide is built with Rust on [GPUI](https://www.gpui.rs/) and GPUI Kit. The contributor and agent guide, with the crate layout, rules and gotchas, is [AGENTS.md](../AGENTS.md). The original spec is the claude.ai doc "pi IDE — Implementation Spec".

## Status

Milestones **M0–M5** are built. M6 (scripted panels) has its options written up in [M6-SCRIPTED-PANELS.md](M6-SCRIPTED-PANELS.md) and waits on a decision. Collaborator mode, the notes-in-the-code work that came after, is described in [COLLAB.md](COLLAB.md). The terminal (2026-11, outside the milestones) is `alacritty_terminal` under a GPUI view: `crates/panels/terminal`.

- [GAPS.md](GAPS.md): what GPUI Kit or pi can't do yet.
- [IDEAS.md](IDEAS.md): what is deliberately not built.
- [SPIKE.md](SPIKE.md): M0 benchmark results.
- [VERSIONS.md](VERSIONS.md): pinned versions.
- [COLORS.md](COLORS.md): semantic colors.

## Build requirements

- Rust >= 1.92 (developed on 1.98). Versions are pinned in `docs/VERSIONS.md`; build with `--locked`.
- macOS (primary target, Apple Silicon): Xcode command line tools.
- Linux (secondary): a Wayland or X11 session with a working Vulkan driver. GPUI Kit may also need
  `libwebkit2gtk-4.1-dev` at build time. Recommended: `mold` for linking.
- Windows (experimental, x64): Visual Studio Build Tools with the C++ workload and a Windows SDK.
  GPUI draws with Direct3D 11. Install pi with npm as usual; pide finds `pi.cmd` and runs its
  script with `node` directly. pide's folders follow `XDG_CONFIG_HOME`/`XDG_STATE_HOME`/
  `XDG_DATA_HOME` (without them, the folder pide is started from).

## Build and run

```sh
cargo run -p pide --release --locked -- ~/some/project   # or no folder: reopens the last project
```

As a Mac app: `dev/bundle-macos.sh` builds `target/release/pide.app` (copy it to /Applications to start pide from Spotlight or the Dock; it reopens the last project).

To drive the app from scripts and tests (hidden window, rendered screenshots), build with `--features devctl` and use `dev/pidectl`; see AGENTS.md.

## Logs

Logs and raw protocol traffic are listed in [GUIDE.md](GUIDE.md#where-pide-keeps-its-files). pi's traffic in `pi-rpc.jsonl` is the source of truth for its protocol.
