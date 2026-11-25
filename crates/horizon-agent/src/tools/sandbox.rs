//! The sandbox runtime this process contains `bash` calls with
//! (`docs/sandbox-runtime-design.md`), and whether it can contain one now.
//! `horizon-agentd` names the runtime at start; until then, Fence on `PATH`.
//! Whether it is ready is probed once and again at each turn's start, so a
//! runtime installed or removed while sessions run is noticed by the next
//! turn.

use horizon_sandbox::runtime::{FenceRuntime, SandboxRuntime, Unavailable};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

struct Holder {
    runtime: RwLock<Arc<dyn SandboxRuntime>>,
    probed: Mutex<Option<Result<(), Unavailable>>>,
}

fn holder() -> &'static Holder {
    static HOLDER: OnceLock<Holder> = OnceLock::new();
    HOLDER.get_or_init(|| Holder {
        runtime: RwLock::new(Arc::new(FenceRuntime::new(fence_on_path()))),
        probed: Mutex::new(None),
    })
}

/// `fence` as found on `PATH`, or the bare name (which then fails to start
/// and reads as unavailable).
pub fn fence_on_path() -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("fence"))
                .find(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("fence"))
}

/// Contain `bash` calls with `runtime` from now on.
pub fn configure(runtime: Arc<dyn SandboxRuntime>) {
    let holder = holder();
    *holder
        .runtime
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = runtime;
    reprobe();
}

pub(crate) fn current() -> Arc<dyn SandboxRuntime> {
    Arc::clone(
        &holder()
            .runtime
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

/// Whether the runtime can contain a call, from the last probe.
pub fn ready() -> bool {
    availability().is_ok()
}

/// The last probe's answer, probing first when there is none.
pub fn availability() -> Result<(), Unavailable> {
    let holder = holder();
    let mut probed = holder
        .probed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    probed.get_or_insert_with(|| current().probe()).clone()
}

/// Forget the last probe; the next question probes again.
pub fn reprobe() {
    *holder()
        .probed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}
