//! The control plane's bridge: what the `horizon` command asks arrives on
//! a thread of the socket's listener and is answered on the UI's, where
//! the shell is (`docs/cli-control-plane-design.md`). The socket, the
//! protocol and the commands' names are the GPUI shell's
//! (`horizon-control`, `horizon-shell-runtime`'s `control_invoke`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::task::Waker;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use horizon_control::contract::EnvelopeBody;
use horizon_control::host::executor::{ControlExecutor, ControlRequest, error_body};
use horizon_control::host::listener;

use crate::workspace::Shell;

/// How long a request waits for the UI thread.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

struct Pending {
    request: ControlRequest,
    reply: Sender<EnvelopeBody>,
}

type Wake = Arc<Mutex<Option<Waker>>>;

/// The listener's end: hands a request over, wakes the UI thread and
/// waits for its answer.
pub struct Asking {
    pending: Sender<Pending>,
    waker: Wake,
}

impl ControlExecutor for Asking {
    fn execute(&self, request: ControlRequest) -> EnvelopeBody {
        let (reply, answer) = crossbeam_channel::bounded(1);
        if self.pending.send(Pending { request, reply }).is_err() {
            return error_body("control plane UI bridge is no longer running");
        }
        if let Some(waker) = self.waker.lock().unwrap().as_ref() {
            waker.wake_by_ref();
        }
        (answer.recv_timeout(ANSWER_WITHIN))
            .unwrap_or_else(|_| error_body("timed out waiting for the UI thread to answer"))
    }
}

/// The UI thread's end.
pub struct Requests {
    pending: Receiver<Pending>,
    waker: Wake,
}

impl Requests {
    /// Answers what has been asked since the last call: for each poll of
    /// the page, with the waker that makes it poll again.
    pub fn answer(&self, waker: &Waker, shell: &Shell) {
        *self.waker.lock().unwrap() = Some(waker.clone());
        for pending in self.pending.try_iter() {
            let reply = pending.reply;
            shell.control_answering(&pending.request, move |answer| {
                let _ = reply.send(answer);
            });
        }
    }
}

/// The two ends, joined.
pub fn bridge() -> (Asking, Requests) {
    let (pending, requests) = crossbeam_channel::unbounded();
    let waker = Wake::default();
    let asking = Asking {
        pending,
        waker: Arc::clone(&waker),
    };
    (
        asking,
        Requests {
            pending: requests,
            waker,
        },
    )
}

/// Listens on `socket`. A socket that cannot be bound is reported on
/// stderr, and nothing arrives.
pub fn listen(socket: PathBuf) -> Requests {
    let (asking, requests) = bridge();
    listener::spawn(socket, asking);
    requests
}
