//! The GPUI shell's connection to `horizon-agentd`: a lean mirror of the
//! Floem shell's `agentd_runtime` connection half, reusing the shared
//! `horizon_agent::client` (connect/spawn/handshake) and `wire`. One OS
//! thread runs a current-thread tokio runtime with the read/write loops;
//! events route to per-session crossbeam senders. The Floem twin of this
//! module dies with the Floem shell at M5 — the duplication is
//! transition-scoped, not permanent.
//!
//! Deliberately not carried over yet: `session_list`-at-startup resume
//! (the GPUI workspace starts fresh; recorded for M5 parity) and real
//! host-tool answers — `workspace.snapshot` requests get an error
//! payload until the workspace snapshot moves somewhere both shells can
//! call it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::{unbounded, Sender};
use horizon_agent::contract::{self, Command, ProviderEvent};
use horizon_agent::wire::{self, Control, Envelope, EnvelopeBody, HostToolResponse};

type AgentSessionId = contract::SessionId;

#[derive(Clone)]
pub struct AgentdHandle {
    outgoing: tokio::sync::mpsc::UnboundedSender<Envelope>,
    session_events: Arc<Mutex<HashMap<AgentSessionId, Sender<ProviderEvent>>>>,
}

impl AgentdHandle {
    /// Connects to `horizon-agentd` (spawning it if needed), blocking the
    /// caller until the handshake finishes — acceptable once at startup,
    /// mirroring the Floem shell's `AgentdConnection::connect`.
    pub fn connect(socket_path: &Path, control_socket: &Path) -> Result<Self, String> {
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
                        "could not start a runtime for horizon-agentd: {err}"
                    )));
                    return;
                }
            };
            runtime.block_on(run_connection(&socket_path, &control_socket, outcome_tx));
        });

        outcome_rx.recv().unwrap_or_else(|_| {
            Err("horizon-agentd connection thread exited before reporting an outcome".to_string())
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
}

async fn run_connection(
    socket_path: &Path,
    control_socket: &Path,
    outcome_tx: std::sync::mpsc::Sender<Result<AgentdHandle, String>>,
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

    let handle = AgentdHandle {
        outgoing: outgoing_tx.clone(),
        session_events: session_events.clone(),
    };
    if outcome_tx.send(Ok(handle)).is_err() {
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
                Ok(Some(envelope)) => dispatch_incoming(envelope, &session_events, &outgoing_tx),
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
        // Host tools are answered with an error payload until the
        // workspace snapshot is callable from this shell (module doc).
        EnvelopeBody::Control(Control::HostToolRequest(request)) => {
            let _ = outgoing.send(Envelope::control(Control::HostToolResponse(
                HostToolResponse {
                    request_id: request.request_id,
                    output: serde_json::json!({
                        "error": "host tools are not available in this shell yet"
                    }),
                },
            )));
        }
        EnvelopeBody::Control(Control::Ping) => {
            let _ = outgoing.send(Envelope::control(Control::Pong));
        }
        EnvelopeBody::Control(_) | EnvelopeBody::Command(_) => {}
    }
}
