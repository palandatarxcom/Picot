// ABOUTME: Process-wide tokio runtime handle the host stack spawns its tasks on.
// ABOUTME: The framework-free replacement for the app framework's async-runtime spawner.

use std::future::Future;
use std::sync::OnceLock;
use tokio::runtime::{Handle, Runtime};

static INSTALLED: OnceLock<Handle> = OnceLock::new();
static OWNED: OnceLock<Runtime> = OnceLock::new();

/// Install the runtime host tasks run on.
///
/// The app installs its own global tokio runtime (the one the app framework's
/// async runtime owns today, so host tasks keep running on exactly the runtime
/// they used before the 2026-10-15 split); `picot-daemon` installs the
/// runtime it builds for its own loop. One process, one host runtime: the first
/// install wins and later calls return `false`.
///
/// Call this before the first [`spawn`]; otherwise the private fallback runtime
/// is created first and the install is ignored.
pub fn install(handle: Handle) -> bool {
    INSTALLED.set(handle).is_ok()
}

/// Spawn a host task on the installed runtime.
///
/// `tokio::spawn` is not usable at the current call sites:
/// `NativePiManager::start_event_pump`, `PiRpcBridge::attach`, and
/// `SharedEphemeralHub::spawn_pump` run on the Tauri setup thread, outside any
/// ambient tokio context, where `tokio::spawn` panics. `tokio::spawn` was
/// therefore never an option; this is the Tauri-free equivalent of the
/// the app framework's async-runtime spawner the host stack used to call.
///
/// Without an installed handle (unit tests, standalone host use) a private
/// multi-thread runtime is created on first use.
pub fn spawn<F>(task: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match INSTALLED.get() {
        Some(handle) => handle.spawn(task),
        None => owned().spawn(task),
    }
}

fn owned() -> &'static Runtime {
    OWNED.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("picot-host-worker")
            .build()
            .expect("picot-host runtime")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// The host call sites sit on threads without an ambient tokio runtime
    /// (Tauri setup thread, std thread), which is exactly why the pre-split
    /// code could not use `tokio::spawn`. Spawning must work from there.
    #[test]
    fn spawn_runs_without_an_ambient_runtime() {
        let (tx, rx) = mpsc::channel();
        spawn(async move {
            tx.send(7_u32).expect("receiver alive");
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).expect("task ran"),
            7
        );
    }
}
