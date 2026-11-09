//! pi as a child process speaking JSONL over stdio (`pi --mode rpc`).
//!
//! Protocol notes come from pi 1.5.0's `docs/rpc.md`, `rpc-commands.md`,
//! `json.md` and `rpc-extension-ui.md`, checked against a recorded session
//! (`tests/fixtures/read_tool_session.jsonl`). Every raw line in both
//! directions is appended to `$XDG_STATE_HOME/pide/pi-rpc.jsonl`.

pub mod backend;
mod bash_watch;
pub mod config;
pub mod conn;
pub mod wire;

pub use backend::PiBackend;
