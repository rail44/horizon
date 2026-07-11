//! The GPUI shell's connection to `horizon-sessiond`, reusing the shared
//! `horizon_agent::client` (connect/spawn/handshake) and `wire`. One OS
//! thread runs a current-thread tokio runtime with the read/write loops;
//! events route to per-session crossbeam senders.
//!
//! Host-tool requests route out on the returned receiver and are
//! answered on the UI thread (`WorkspaceShell::adopt_sessiond` wires the
//! responder); `session_list` + `attach_session` serve both
//! startup resume and `Reload Session Runtime`
//! (`WorkspaceShell::spawn_startup_resume`/`reload_session_runtime`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::{unbounded, Receiver, Sender};
use horizon_agent::contract::{self, Command, ProviderEvent};
use horizon_agent::wire::{
    self, Control, Envelope, EnvelopeBody, HostToolRequest, HostToolResponse,
};

type AgentSessionId = contract::SessionId;

#[derive(Clone)]
pub struct SessiondHandle {
    outgoing: tokio::sync::mpsc::UnboundedSender<Envelope>,
    session_events: Arc<Mutex<HashMap<AgentSessionId, Sender<ProviderEvent>>>>,
    // Answers the one-at-a-time session_list round trip (same
    // no-request-id simplification as the Floem shell).
    pending_session_list: Arc<Mutex<Option<Sender<Vec<wire::SessionSummary>>>>>,
}

impl SessiondHandle {
    /// Connects to `horizon-sessiond` (spawning it if needed), blocking the
    /// caller until the handshake finishes — acceptable once at startup. The
    /// returned receiver carries host-tool requests (e.g.
    /// `workspace.snapshot`) the shell must answer via
    /// [`Self::respond_host_tool`].
    pub fn connect(
        socket_path: &Path,
        control_socket: &Path,
    ) -> Result<(Self, Receiver<HostToolRequest>), String> {
        let socket_path = socket_path.to_path_buf();
        let control_socket = control_socket.to_path_buf();
        let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = outcome_tx.send(Err(format!(
                        "could not start a runtime for horizon-sessiond: {err}"
                    )));
                    return;
                }
            };
            runtime.block_on(run_connection(&socket_path, &control_socket, outcome_tx));
        });

        // Bounded so a wedged sessiond (accepting but never handshaking)
        // can't hang the caller — which may be the UI thread on the lazy
        // path. `Reload Session Runtime` (drain/respawn) is the recovery
        // for the wedged daemon itself.
        outcome_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|_| {
                Err("timed out waiting for the horizon-sessiond handshake".to_string())
            })
    }

    /// Starts a fresh session over this connection and hands back the
    /// command/event handle — same contract as the Floem shell's
    /// `start_session`.
    pub fn start_session(
        &self,
        session_id: AgentSessionId,
        provider_id: contract::ProviderId,
        role_id: Option<horizon_agent::roles::RoleId>,
    ) -> contract::SessionHandle {
        let handle = self.register_session_routing(session_id);
        let _ = self
            .outgoing
            .send(Envelope::control(Control::SessionNew(wire::SessionNew {
                session_id,
                provider_id,
                role_id,
                workspace_root: std::env::current_dir().ok(),
            })));
        handle
    }

    fn register_session_routing(&self, session_id: AgentSessionId) -> contract::SessionHandle {
        let (command_tx, command_rx) = unbounded::<Command>();
        let (event_tx, event_rx) = unbounded::<ProviderEvent>();
        self.session_events
            .lock()
            .unwrap()
            .insert(session_id, event_tx);

        let outgoing = self.outgoing.clone();
        thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                if outgoing
                    .send(Envelope::command(session_id, command))
                    .is_err()
                {
                    break;
                }
            }
        });

        contract::SessionHandle::new(command_tx, event_rx)
    }

    pub fn respond_host_tool(&self, response: HostToolResponse) {
        let _ = self
            .outgoing
            .send(Envelope::control(Control::HostToolResponse(response)));
    }

    /// Attaches to a session that sessiond already hosts (resumed from its log):
    /// sends `session_load`, so committed events replay onto the handle.
    pub fn attach_session(&self, session_id: AgentSessionId) -> contract::SessionHandle {
        let handle = self.register_session_routing(session_id);
        let _ = self
            .outgoing
            .send(Envelope::control(Control::SessionLoad(wire::SessionLoad {
                session_id,
            })));
        handle
    }

    /// Asks sessiond for every session it hosts, blocking up to five
    /// seconds — called from the startup background thread, never the UI
    /// thread.
    pub fn session_list(&self) -> Vec<wire::SessionSummary> {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        *self.pending_session_list.lock().unwrap() = Some(reply_tx);
        if self
            .outgoing
            .send(Envelope::control(Control::SessionList))
            .is_err()
        {
            return Vec::new();
        }
        reply_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap_or_default()
    }

    /// Asks sessiond to drain: flush and exit. Best-effort and
    /// fire-and-forget — the caller (`Reload Session Runtime`) doesn't wait
    /// for a reply, just for the old process to actually be gone, observed
    /// indirectly via [`wait_for_drain`].
    pub fn drain(&self) {
        let _ = self.outgoing.send(Envelope::control(Control::Drain));
    }
}

/// How long [`wait_for_drain`] polls for the old `horizon-sessiond` to
/// actually stop accepting connections before giving up.
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Poll interval for [`wait_for_drain`].
const DRAIN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Blocks the calling thread until nothing answers a connection attempt on
/// `socket_path` — i.e. the drained old `horizon-sessiond` has actually
/// exited — or [`DRAIN_TIMEOUT`] elapses, in which case it's reported as a
/// failure rather than silently falling through to a spawn-or-connect
/// attempt that might reattach to a still-alive old process. Synchronous
/// (`std::os::unix::net`, not tokio) since the caller has no async runtime
/// of its own.
pub fn wait_for_drain(socket_path: &Path) -> Result<(), String> {
    let deadline = std::time::Instant::now() + DRAIN_TIMEOUT;
    loop {
        if std::os::unix::net::UnixStream::connect(socket_path).is_err() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "horizon-sessiond did not drain within {:.1}s",
                DRAIN_TIMEOUT.as_secs_f64()
            ));
        }
        thread::sleep(DRAIN_POLL_INTERVAL);
    }
}

async fn run_connection(
    socket_path: &Path,
    control_socket: &Path,
    outcome_tx: std::sync::mpsc::Sender<
        Result<(SessiondHandle, Receiver<HostToolRequest>), String>,
    >,
) {
    let (mut reader, mut writer, _hello) =
        match horizon_agent::client::connect_and_split(socket_path, control_socket).await {
            Ok(parts) => parts,
            Err(err) => {
                let _ = outcome_tx.send(Err(err));
                return;
            }
        };

    let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();
    let session_events: Arc<Mutex<HashMap<AgentSessionId, Sender<ProviderEvent>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let (host_tool_tx, host_tool_rx) = unbounded::<HostToolRequest>();
    let pending_session_list = Arc::new(Mutex::new(None));

    let handle = SessiondHandle {
        outgoing: outgoing_tx.clone(),
        session_events: session_events.clone(),
        pending_session_list: pending_session_list.clone(),
    };
    if outcome_tx.send(Ok((handle, host_tool_rx))).is_err() {
        return;
    }

    let write_task = async move {
        while let Some(envelope) = outgoing_rx.recv().await {
            if wire::write_envelope(&mut writer, &envelope).await.is_err() {
                break;
            }
        }
    };
    let read_task = async move {
        loop {
            match wire::read_envelope(&mut reader).await {
                Ok(Some(envelope)) => dispatch_incoming(
                    envelope,
                    &session_events,
                    &outgoing_tx,
                    &host_tool_tx,
                    &pending_session_list,
                ),
                Ok(None) | Err(_) => return,
            }
        }
    };
    tokio::select! {
        _ = write_task => {}
        _ = read_task => {}
    }
}

fn dispatch_incoming(
    envelope: Envelope,
    session_events: &Arc<Mutex<HashMap<AgentSessionId, Sender<ProviderEvent>>>>,
    outgoing: &tokio::sync::mpsc::UnboundedSender<Envelope>,
    host_tool_tx: &Sender<HostToolRequest>,
    pending_session_list: &Arc<Mutex<Option<Sender<Vec<wire::SessionSummary>>>>>,
) {
    match envelope.body {
        EnvelopeBody::Event(event) => {
            let Some(session_id) = envelope.session_id else {
                return;
            };
            if let Some(sender) = session_events.lock().unwrap().get(&session_id) {
                let _ = sender.send(ProviderEvent::from(event));
            }
        }
        EnvelopeBody::Control(Control::ToolCallProgress(progress)) => {
            let Some(session_id) = envelope.session_id else {
                return;
            };
            if let Some(sender) = session_events.lock().unwrap().get(&session_id) {
                let _ = sender.send(ProviderEvent::tool_call_progress(progress));
            }
        }
        // Host tools are answered on the UI thread (the workspace lives
        // there) — routed out to the shell's pump, mirroring the Floem
        // shell's wire_host_tool_responder.
        EnvelopeBody::Control(Control::HostToolRequest(request)) => {
            let _ = host_tool_tx.send(request);
        }
        EnvelopeBody::Control(Control::SessionListResult(summaries)) => {
            if let Some(reply) = pending_session_list.lock().unwrap().take() {
                let _ = reply.send(summaries);
            }
        }
        EnvelopeBody::Control(Control::Ping) => {
            let _ = outgoing.send(Envelope::control(Control::Pong));
        }
        EnvelopeBody::Control(_) | EnvelopeBody::Command(_) => {}
    }
}
