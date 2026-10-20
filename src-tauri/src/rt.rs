//! The process's tokio runtime, in both the desktop app and the server build.
//!
//! Code that may run outside the runtime (the main thread, Tauri event callbacks) spawns
//! through here instead of `tokio::spawn`, which needs to already be inside it. The
//! desktop app hands this runtime to Tauri (`tauri::async_runtime::set`), so Tauri's
//! async commands run on it too.

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start the tokio runtime")
    })
}

/// A handle to the runtime, e.g. to give to Tauri.
#[cfg(feature = "desktop")]
pub fn handle() -> tokio::runtime::Handle {
    runtime().handle().clone()
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(future)
}

pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    runtime().spawn_blocking(f)
}

/// Runs a future to completion on the runtime. Not from inside it.
#[cfg(not(feature = "desktop"))]
pub fn block_on<F: Future>(future: F) -> F::Output {
    runtime().block_on(future)
}
