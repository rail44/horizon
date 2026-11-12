//! The terminal runtime as this shell uses it: a `horizon-terminald` of
//! its own, which `Reload Terminal Runtime` puts another in the place of.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use horizon_shell_runtime::{TerminaldHandle, wait_for_drain};

use crate::workspace::ReloadTerminals;

#[derive(Clone)]
pub struct Terminald {
    handle: Arc<Mutex<TerminaldHandle>>,
    socket: PathBuf,
    control_socket: PathBuf,
}

impl Terminald {
    /// Connects to the daemon on `socket`, starting it if it is not
    /// running.
    pub fn start(socket: &Path, control_socket: &Path) -> Self {
        Self {
            handle: Arc::new(Mutex::new(TerminaldHandle::start(socket, control_socket))),
            socket: socket.to_path_buf(),
            control_socket: control_socket.to_path_buf(),
        }
    }

    /// The runtime as it is now.
    pub fn handle(&self) -> TerminaldHandle {
        self.handle.lock().unwrap().clone()
    }

    /// Ends the daemon, and with it every terminal it runs, and starts
    /// one in its place. Waits for the first to be gone.
    pub fn restart(&self) {
        let old = self.handle();
        if old.begin_reload()
            && let Err(error) = wait_for_drain(&self.socket)
        {
            eprintln!("horizon-terminald did not drain cleanly: {error}");
        }
        old.stop_and_wait();
        *self.handle.lock().unwrap() = TerminaldHandle::start(&self.socket, &self.control_socket);
    }

    /// What `Reload Terminal Runtime` does with this runtime.
    pub fn reload(&self) -> ReloadTerminals {
        let runtime = self.clone();
        Arc::new(move || runtime.restart())
    }
}
