//! One tokio runtime on a dedicated background thread (spec: Async).
//!
//! LSP and pi I/O run here. Results go back to the UI over channels
//! (`tokio::sync::mpsc` receivers work on GPUI's executor) and a `cx.spawn`
//! task applies them to entities. Never `block_on` from the UI thread.

use std::future::Future;
use std::sync::OnceLock;

pub mod monitor;

pub use tokio;

static HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();

/// Start the runtime thread. Idempotent.
pub fn init() {
    HANDLE.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("pide-runtime".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build tokio runtime");
                tx.send(rt.handle().clone()).expect("send runtime handle");
                rt.block_on(std::future::pending::<()>());
            })
            .expect("spawn runtime thread");
        rx.recv().expect("receive runtime handle")
    });
}

/// The runtime's handle. Panics if [`init`] was not called.
pub fn handle() -> &'static tokio::runtime::Handle {
    HANDLE.get().expect("pide_runtime::init() not called")
}

/// Spawn a future on the runtime thread.
pub fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    handle().spawn(fut)
}
