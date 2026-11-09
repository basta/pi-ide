//! Running a server's install command (only ever on the user's click).

pub use pide_runtime::process::{Streaming as Install, run_streaming};

/// Start an install command on the runtime.
pub fn run(command: Vec<String>) -> Install {
    run_streaming(command, None)
}
