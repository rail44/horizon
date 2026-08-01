//! A spawned daemon child process and the scratch paths that live and die
//! with it.

use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};
use std::time::Duration;

use crate::binary::spawn_with_link_retry;

/// A short, SUN_LEN-safe socket path in the system temp dir. Keeping it
/// short is load-bearing: `temp_dir()` alone is ~50 bytes on macOS, so a
/// long descriptive file name pushes `bind()` into "path must be shorter
/// than SUN_LEN".
pub fn scratch_socket(tag: &str) -> PathBuf {
    let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
    std::env::temp_dir().join(format!("hzn-{tag}-{short_id}.sock"))
}

/// A throwaway file path in the system temp dir, unique per process and per
/// call. Regular files are free of the socket path's SUN_LEN limit, so these
/// stay descriptive.
pub fn scratch_file(tag: &str, extension: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "horizon-{tag}-{}-{}.{extension}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// Owns a spawned daemon child and its socket path; kills the child and
/// removes the socket file on drop so a failing assertion doesn't leak
/// either (nor whatever the daemon owns -- PTYs, in terminald's case)
/// across test runs.
///
/// This is the *plain* case: a daemon whose only scratch state is its
/// socket. `horizon-agentd` has persistence paths too and gets its own
/// handle, [`crate::AgentdProcess`].
pub struct DaemonProcess {
    pub child: Child,
    pub socket_path: PathBuf,
}

impl DaemonProcess {
    /// Spawns `command` (which the caller has already pointed at
    /// `socket_path`) with [`spawn_with_link_retry`].
    pub fn spawn(command: &mut Command, socket_path: PathBuf) -> Self {
        Self {
            child: spawn_with_link_retry(command),
            socket_path,
        }
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Polls `child` until it exits, or panics after a generous budget.
pub async fn wait_for_exit(child: &mut Child) -> ExitStatus {
    for _ in 0..200 {
        if let Ok(Some(status)) = child.try_wait() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the daemon did not exit in time");
}
