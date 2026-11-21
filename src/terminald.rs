//! The terminal runtime as this shell uses it: a `horizon-terminald` of
//! its own, which `Reload Terminal Runtime` has hand its terminals to its
//! next image (`docs/terminald-handover-design.md`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use horizon_shell_runtime::{HandOver, TerminalSessionHandle, TerminaldHandle, wait_for_drain};
use uuid::Uuid;

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

    /// Has the daemon hand every terminal to its next image, the build on
    /// disk, and attaches to them again on a new connection. A daemon
    /// built before handovers is drained instead, which ends its
    /// terminals: nothing else brings its next build in.
    pub fn hand_over(&self) -> Result<Vec<(Uuid, TerminalSessionHandle)>, String> {
        let old = self.handle();
        match old.hand_over() {
            HandOver::Done => {}
            // They go on in the same image, which let go of this
            // connection's attachments: attached to again below.
            HandOver::Refused(reason) => {
                eprintln!("horizon-terminald kept its terminals: {reason}");
            }
            HandOver::Unable => {
                eprintln!(
                    "horizon-terminald was built before handovers; draining it, which ends \
                     its terminals"
                );
                if old.begin_reload()
                    && let Err(error) = wait_for_drain(&self.socket)
                {
                    eprintln!("horizon-terminald did not drain cleanly: {error}");
                }
            }
        }
        old.stop_and_wait();
        let new = TerminaldHandle::start(&self.socket, &self.control_socket);
        *self.handle.lock().unwrap() = new.clone();
        let running = new.terminal_list()?;
        Ok(new.attach_terminals(running.iter().map(|terminal| terminal.session_id).collect()))
    }

    /// What `Reload Terminal Runtime` does with this runtime.
    pub fn reload(&self) -> ReloadTerminals {
        let runtime = self.clone();
        Arc::new(move || runtime.hand_over())
    }
}
