# Pinned versions

Checked 2026-11-09 with `cargo search` and the GPUI Kit v0.9.0 tag. Re-check before bumping anything.

| What | Version | How it's pinned | Notes |
| --- | --- | --- | --- |
| Rust toolchain | 1.98.0 (stable, aarch64-apple-darwin on the dev Mac) | — | GPUI Kit needs >= 1.92. |
| `gpui-kit` | 0.10.0 | `gpui-kit = "=0.10.0"` | Upgraded 2026-11-21 for gpui-shell (no source changes needed). Repo `longbridge/gpui-kit`. The vendored examples in `reference/gpui-kit-v0.9.0/` are from 0.9.0. |
| `gpui-shell` | 0.10.0 | `=0.10.0`, no default features, `quickjs` + `components` | Script panels. Not `webview`: it pulls `gpui-fast`, which would switch GPUI Kit to another GPUI build. Linking its tests on Linux needs `libxkbcommon-dev` and `libxkbcommon-x11-dev`. |
| `gpui-pre` (GPUI, via gpui-kit) | 0.3.12 | transitive, locked | gpui-kit 0.10.0 resolves 0.3.12. Never depend on `gpui` / `gpui-pre` directly. |
| `gpui-component` / `gpui-base` | 0.9.0 | transitive | Editor lives in `gpui_base::input::EditorState`, re-exported as `gpui_kit::component::input::EditorState`. |
| `taffy` | 0.15.0 | `cargo update -p taffy --precise 0.15.0` | **Gotcha:** gpui-kit's `profiler` feature pulls `gpui-fast`, which pins `taffy =0.15.0`; a fresh lock picks 0.15.1 and fails to resolve. |
| `tree-sitter-rust` | 0.24.2 | via gpui-kit feature `tree-sitter-rust` | Other planned features: `tree-sitter-python`, `tree-sitter-markdown`, `tree-sitter-toml`; JSON comes with the base `tree-sitter` feature. |
| pi (`@earendil-works/pi-coding-agent`) | 1.5.0 | global npm install (`npm i -g @earendil-works/pi-coding-agent@1.5.0`) | Upgraded from 0.79.6 on 2026-11-09. The spec's RPC notes are for v0.83; the changelog since shows no RPC-breaking changes (adds `agent_settled` with `aborted`). Read the installed `docs/rpc*.md` before M1 RPC work. |
| `gix` | 0.90.0 | `gix = "0.90"` in `crates/panels/git` | Features `status`, `sha1`, `parallel` only (no network, no default features). Added 2026-11-10. |

Planned, not yet added (latest seen): `nucleo` 0.5.0, `notify` 9.0.0-rc.7 (pre-release; prefer latest stable 8.x unless 9 is final by M1),
`async-lsp` 0.2.4, `lsp-types` 0.97.0 (gpui-kit already resolves exactly 0.97.0 — keep them equal). Buffer type is `ropey` 2.0.0-beta.1 (via gpui-kit; `len_lines` takes a `LineType`).
