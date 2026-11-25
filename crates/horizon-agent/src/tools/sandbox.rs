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

/// A session's temporary directory: its contained calls' `TMPDIR`, kept
/// across them for as long as the session lasts. `None` when it cannot be
/// made; the call then gets the runtime's own.
pub(crate) fn session_temp_dir(session_id: crate::contract::SessionId) -> Option<PathBuf> {
    let dir = sessions_temp_root().join(session_id.as_uuid().to_string());
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

/// Removes a session's temporary directory, as the session ends.
pub(crate) fn remove_session_temp_dir(session_id: crate::contract::SessionId) {
    let _ = std::fs::remove_dir_all(sessions_temp_root().join(session_id.as_uuid().to_string()));
}

fn sessions_temp_root() -> PathBuf {
    std::env::temp_dir()
        .join("horizon-sandbox")
        .join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_keeps_one_temp_dir_until_it_ends() {
        let session = crate::contract::SessionId::new();
        let dir = session_temp_dir(session).unwrap();
        std::fs::write(dir.join("note"), "kept").unwrap();
        assert_eq!(session_temp_dir(session), Some(dir.clone()));
        assert!(dir.join("note").exists());

        remove_session_temp_dir(session);

        assert!(!dir.exists());
    }
}
