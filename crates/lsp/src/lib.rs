//! Language servers (spec M3): one per project and language, started when a
//! file of its language opens, restarted after a crash. Talks JSON-RPC over the
//! server's stdio on the tokio runtime; the editor gets GPUI Kit providers.

pub mod convert;
pub mod install;
pub mod msvc;
pub mod process;
pub mod registry;
pub mod rpc;
pub mod store;

pub use store::{FileDiagnostics, LspEvent, LspStore, Server, ServerState};
