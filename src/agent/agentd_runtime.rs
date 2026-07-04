//! Step 3 of `docs/agent-runtime-split-design.md`: the live, multiplexing
//! connection to `horizon-agentd` -- "one per process" (decision 4) -- that
//! [`AgentdConnection::start_session`] hands out a
//! [`contract::SessionHandle`] against, indistinguishable at every existing
//! call site (`session::Registry::agent_sender`, the pane's approve/deny/
//! cancel commands, ...) from the in-process handle
//! `providers::ProviderRegistry::start_session` returns. That's the point:
//! "the fold must not know which transport delivered the events" extends to
//! commands too, so nothing outside `app::runtime::agent` has to branch on
//! whether agentd is in the picture.
//!
//! Also hosts the Horizon side of the host-tool channel (guardrail 4):
//! [`wire_host_tool_responder`] answers `workspace.snapshot` requests
//! arriving from agentd by reading Horizon's own `Workspace`, reusing
//! `agent::host_tools::workspace_snapshot` -- the exact function Horizon's
//! in-process `WorkspaceHostTools` calls today.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::{unbounded, Receiver, Sender};
use floem::ext_event::create_signal_from_channel;
use floem::prelude::*;
use floem::reactive::create_effect;

use horizon_agent::wire::{
    self, Control, Envelope, EnvelopeBody, HostToolRequest, HostToolResponse, SessionNew,
};

use crate::agent::contract::{
    self, Command, Error as AgentError, Event, ProviderEvent, ProviderId,
};
use crate::agent::host_tools::workspace_snapshot;
use crate::workspace::Workspace;

type AgentSessionId = contract::SessionId;

/// A `host_tool_request` that arrived from agentd, paired with the session
/// it's scoped to -- handed to [`wire_host_tool_responder`]'s effect via a
/// dedicated channel (kept separate from [`AgentdConnection`] itself, which
/// is cloned freely into every session's spawn path) so there's exactly one
/// receiver to wire up, once, at startup.
#[derive(Clone)]
pub(crate) struct HostToolRequestEnvelope {
    session_id: AgentSessionId,
    request: HostToolRequest,
}

/// The live connection to `horizon-agentd`, cheaply `Clone` (an `Arc`-backed
/// handle) so it can live in `AppState` and be threaded into every agent
/// session's spawn path via `SessionRuntimeState`. Sessions are looked up by
/// id in [`Self::session_events`] to route an incoming event envelope to the
/// right [`contract::SessionHandle`]'s receiver -- the connection-side twin
/// of `horizon-agentd`'s own `session::Connection::sessions` map.
#[derive(Clone)]
pub(crate) struct AgentdConnection {
    outgoing: tokio::sync::mpsc::UnboundedSender<Envelope>,
    session_events: Arc<Mutex<HashMap<AgentSessionId, Sender<ProviderEvent>>>>,
}

impl AgentdConnection {
    /// Connects to `horizon-agentd` at `socket_path` (spawning it if
    /// necessary) and completes the hello handshake, **blocking the calling
    /// thread** until that finishes -- acceptable at Horizon startup (see
    /// `agent::agentd_client`'s module doc), unlike a per-frame UI
    /// operation. On success, spawns a dedicated OS thread (its own
    /// current-thread tokio runtime) that keeps running the connection's
    /// read/write loop for the rest of the process's life; on failure,
    /// nothing is left running.
    ///
    /// Returns the connection plus the receiver [`wire_host_tool_responder`]
    /// needs -- kept separate from `Self` (see [`HostToolRequestEnvelope`]'s
    /// doc comment) rather than exposed as a method callable more than once.
    pub(crate) fn connect(
        socket_path: &Path,
    ) -> Result<(Self, Receiver<HostToolRequestEnvelope>), String> {
        let socket_path = socket_path.to_path_buf();
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
            runtime.block_on(run_connection(&socket_path, outcome_tx));
        });

        outcome_rx.recv().unwrap_or_else(|_| {
            Err("horizon-agentd connection thread exited before reporting an outcome".to_string())
        })
    }

    /// Spawns a session over this connection: sends `session_new` and hands
    /// back a [`contract::SessionHandle`] whose `sender()` forwards each
    /// `Command` as a `command` envelope (via a small draining thread --
    /// commands arrive from the UI thread, which isn't async) and whose
    /// `events()` receives whatever this session's event envelopes
    /// demultiplex to (see [`dispatch_incoming`]). Indistinguishable, from
    /// the caller's side, from `providers::ProviderRegistry::start_session`'s
    /// in-process handle.
    pub(crate) fn start_session(
        &self,
        session_id: AgentSessionId,
        provider_id: ProviderId,
    ) -> contract::SessionHandle {
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

        let _ = self
            .outgoing
            .send(Envelope::control(Control::SessionNew(SessionNew {
                session_id,
                provider_id,
                config_overrides: None,
            })));

        contract::SessionHandle::new(command_tx, event_rx)
    }

    /// A connection with no live socket behind it: every outgoing envelope
    /// is silently dropped (the paired receiver is discarded immediately).
    /// For tests that only need to prove *dispatch* -- does agentd-mode
    /// code correctly route around Horizon's own persistence, does
    /// `start_session` produce the right shape -- without spawning a real
    /// `horizon-agentd` process (see `app::runtime::agent`'s no-double-write
    /// test).
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let (outgoing, _receiver) = tokio::sync::mpsc::unbounded_channel();
        Self {
            outgoing,
            session_events: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn send_host_tool_response(&self, session_id: AgentSessionId, response: HostToolResponse) {
        let envelope = Envelope {
            v: wire::CONTRACT_VERSION,
            session_id: Some(session_id),
            body: EnvelopeBody::Control(Control::HostToolResponse(response)),
        };
        let _ = self.outgoing.send(envelope);
    }
}

/// Wires up the Horizon side of the host-tool channel, once, for the
/// lifetime of the connection: every `host_tool_request` arriving from
/// agentd is answered by reading `workspace` on the UI thread (the same
/// `create_signal_from_channel` + `create_effect` cross-thread-to-UI bridge
/// `app::runtime::agent::spawn_agent_session` uses for provider events and
/// bash completions) and sent back as a `host_tool_response`.
pub(crate) fn wire_host_tool_responder(
    connection: AgentdConnection,
    host_tool_requests: Receiver<HostToolRequestEnvelope>,
    workspace: RwSignal<Workspace>,
) {
    let requests = create_signal_from_channel(host_tool_requests);
    create_effect(move |_| {
        if let Some(envelope) = requests.get() {
            let output = workspace
                .with_untracked(|ws| answer_host_tool_request(ws, &envelope.request.tool_id));
            connection.send_host_tool_response(
                envelope.session_id,
                HostToolResponse {
                    request_id: envelope.request.request_id,
                    output,
                },
            );
        }
    });
}

/// The tool catalog this side of the host-tool channel answers -- today,
/// just `workspace.snapshot` (the one Horizon-coupled tool that exists; see
/// `agent::host_tools`). An unrecognized `tool_id` answers `null` rather
/// than dropping the request, so a session waiting on
/// `AgentdHostTools::execute_auto`'s timeout in `horizon-agentd` fails fast
/// with "cannot be executed automatically" instead of hanging the full
/// timeout for a tool this build doesn't know about.
fn answer_host_tool_request(workspace: &Workspace, tool_id: &str) -> serde_json::Value {
    match tool_id {
        "workspace.snapshot" => workspace_snapshot(workspace),
        _ => serde_json::Value::Null,
    }
}

/// Runs for the lifetime of a successful connection: completes the
/// handshake, reports the outcome back to [`AgentdConnection::connect`]'s
/// caller exactly once, then keeps reading/writing until the socket closes.
async fn run_connection(
    socket_path: &Path,
    outcome_tx: std::sync::mpsc::Sender<
        Result<(AgentdConnection, Receiver<HostToolRequestEnvelope>), String>,
    >,
) {
    let (mut reader, mut writer, _hello) =
        match crate::agent::agentd_client::connect_and_split(socket_path).await {
            Ok(parts) => parts,
            Err(err) => {
                let _ = outcome_tx.send(Err(err));
                return;
            }
        };

    let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();
    let session_events = Arc::new(Mutex::new(HashMap::new()));
    let (host_tool_tx, host_tool_rx) = unbounded::<HostToolRequestEnvelope>();

    let connection = AgentdConnection {
        outgoing: outgoing_tx,
        session_events: session_events.clone(),
    };
    if outcome_tx.send(Ok((connection, host_tool_rx))).is_err() {
        // Nobody is waiting for this connection any more (the calling
        // thread gave up or the caller was dropped) -- nothing left to do.
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
                Ok(Some(envelope)) => dispatch_incoming(envelope, &session_events, &host_tool_tx),
                Ok(None) | Err(_) => {
                    mark_connection_lost(&session_events);
                    return;
                }
            }
        }
    };

    tokio::select! {
        _ = write_task => {}
        _ = read_task => {}
    }
}

/// Routes one incoming envelope: an event to the session it's scoped to
/// (looked up in `session_events`, the connection-side twin of agentd's own
/// session map), a `host_tool_request` to the responder effect's channel.
/// Horizon never expects a `Command` or most `Control` variants from agentd
/// -- silently ignored rather than treated as an error, matching
/// `horizon-agentd`'s own tolerance for out-of-place messages.
fn dispatch_incoming(
    envelope: Envelope,
    session_events: &Arc<Mutex<HashMap<AgentSessionId, Sender<ProviderEvent>>>>,
    host_tool_tx: &Sender<HostToolRequestEnvelope>,
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
        EnvelopeBody::Control(Control::HostToolRequest(request)) => {
            if let Some(session_id) = envelope.session_id {
                let _ = host_tool_tx.send(HostToolRequestEnvelope {
                    session_id,
                    request,
                });
            }
        }
        _ => {}
    }
}

/// The connection dropped (or a malformed message closed it) -- per this
/// step's explicit scope ("if the connection drops, surface an error on
/// affected sessions; no auto-reconnect"), pushes a synthetic `Event::Error`
/// into every currently-registered session's event stream so it folds
/// through the ordinary path and shows up in that session's transcript,
/// rather than the pane silently going quiet.
fn mark_connection_lost(
    session_events: &Arc<Mutex<HashMap<AgentSessionId, Sender<ProviderEvent>>>>,
) {
    let senders: Vec<Sender<ProviderEvent>> =
        session_events.lock().unwrap().values().cloned().collect();
    let event = Event::Error(AgentError {
        message: "Lost connection to horizon-agentd (no auto-reconnect in this build -- \
                  restart Horizon to reconnect; see docs/agent-runtime-split-design.md)."
            .to_string(),
    });
    for sender in senders {
        let _ = sender.send(ProviderEvent::from(event.clone()));
    }
}
