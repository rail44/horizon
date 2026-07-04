//! Session hosting: `docs/agent-runtime-split-design.md` step 3. Each
//! `Control::SessionNew` spawns a dedicated OS thread that owns the real
//! session loop (the same `providers`/`tools`/`persistence` machinery
//! Horizon used to run in-process), and command/event envelopes are routed
//! to/from that thread by session id.
//!
//! **Why a dedicated thread per session, not an async task.** `LiveState`/
//! `ToolSessionState` are `Rc`-based and `tools::state::SESSION_RUNTIMES` is
//! a `thread_local!` (see their doc comments in the crate) — both assume
//! everything for one session runs on a single, consistent OS thread, the
//! way Horizon's floem UI thread provided in-process. A dedicated thread per
//! session reproduces exactly that: `register_session_runtime` and every
//! later `session_runtime` lookup for the same session id (from
//! `resolve_approval`, driven by an incoming `ApproveToolCall`/
//! `DenyToolCall` envelope) happen on the same thread, so the thread-local
//! registry works correctly without making any of this `Send`. Blocking is
//! also what makes the host-tool round trip simple (see
//! [`AgentdHostTools::execute_auto`]): the session thread genuinely blocks
//! on a channel recv while Horizon answers over the wire, which would
//! deadlock a single-threaded async runtime but is harmless on its own
//! dedicated thread.
//!
//! **Session lifetime is scoped to the connection that created it.** Since
//! `horizon-agentd` serves one connection at a time by construction (see
//! `main.rs`) and reconnect/`session_load` are step 4, sessions spawned here
//! live in the [`Connection`] that owns them; they are not handed off to a
//! later connection. A session thread whose connection has gone away simply
//! has nowhere to send its outgoing events (see the `outgoing` channel
//! closing in that case) — cleaned up only when the process exits. Revisit
//! this once step 4 defines what a session should do across a reconnect.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};

use horizon_agent::config::AgentConfig;
use horizon_agent::contract::{
    self, Command, Error as AgentError, Event, Initialization, ProviderEvent, ProviderId,
    ProviderRegistry, SessionId, SessionState, ToolCallId,
};
use horizon_agent::live::LiveState;
use horizon_agent::persistence::event_log::WriterHandle;
use horizon_agent::tools::{
    process_agent_provider_event, register_session_runtime, resolve_approval,
    should_fold_completion, unregister_session_runtime, ApprovalDecision, ApprovalOutcome,
    BashCompletion, HostTools, ToolSessionState,
};
use horizon_agent::wire::{
    Control, Envelope, EnvelopeBody, HostToolRequest, HostToolResponse, SessionNew, SessionSummary,
};
use tokio::sync::mpsc::UnboundedSender;

/// How long a session thread waits for Horizon to answer a `host_tool_*`
/// round trip before giving up. Generous but finite: a client that never
/// answers must not hang a session forever.
const HOST_TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// Process-lifetime state, built once in `main` and shared (via `Arc`) by
/// every connection `horizon-agentd` ever serves.
pub(crate) struct AgentdState {
    pub(crate) providers: ProviderRegistry,
    pub(crate) agent_config: AgentConfig,
    /// `None` when the event log couldn't be opened at startup (mirrors
    /// Horizon's own graceful degrade in `app::runtime::agent::
    /// open_agent_runtime_state_store`): sessions still run, just without
    /// persistence.
    pub(crate) writer: Option<WriterHandle>,
}

struct SessionEntry {
    provider_id: ProviderId,
    inbound: Sender<Command>,
}

/// One connection's live sessions and the plumbing they share: the outgoing
/// envelope queue (drained by `main`'s writer task) and the map that routes
/// an incoming `Control::HostToolResponse` back to whichever session thread
/// is blocked waiting for it.
#[derive(Clone)]
pub(crate) struct Connection {
    outgoing: UnboundedSender<Envelope>,
    sessions: Arc<Mutex<HashMap<SessionId, SessionEntry>>>,
    pending_host_tool_requests: Arc<Mutex<HashMap<String, Sender<HostToolResponse>>>>,
    state: Arc<AgentdState>,
}

impl Connection {
    pub(crate) fn new(outgoing: UnboundedSender<Envelope>, state: Arc<AgentdState>) -> Self {
        Self {
            outgoing,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            pending_host_tool_requests: Arc::new(Mutex::new(HashMap::new())),
            state,
        }
    }

    /// Spawns the session thread for a `Control::SessionNew`. Reuses the
    /// crate's existing spawn shape (`ProviderRegistry::start_session`) --
    /// the same call `app::runtime::agent::spawn_agent_session` makes
    /// in-process.
    pub(crate) fn handle_session_new(&self, new: SessionNew) {
        let (inbound_tx, inbound_rx) = unbounded::<Command>();
        self.sessions.lock().unwrap().insert(
            new.session_id,
            SessionEntry {
                provider_id: new.provider_id.clone(),
                inbound: inbound_tx,
            },
        );

        let state = self.state.clone();
        let outgoing = self.outgoing.clone();
        let pending = self.pending_host_tool_requests.clone();
        let sessions = self.sessions.clone();
        let session_id = new.session_id;
        let provider_id = new.provider_id;
        thread::spawn(move || {
            run_session(
                session_id,
                provider_id,
                &state,
                outgoing,
                pending,
                inbound_rx,
            );
            sessions.lock().unwrap().remove(&session_id);
        });
    }

    /// Routes a `Command` envelope scoped to `session_id` to that session's
    /// thread. A miss (unknown session id -- stale/mistargeted envelope) is
    /// logged and dropped rather than panicking.
    pub(crate) fn route_command(&self, session_id: SessionId, command: Command) {
        let sender = self
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|entry| entry.inbound.clone());
        match sender {
            Some(sender) => {
                let _ = sender.send(command);
            }
            None => eprintln!("horizon-agentd: command for unknown session {session_id:?}"),
        }
    }

    /// Routes an incoming `Control::HostToolResponse` back to whichever
    /// session thread's [`AgentdHostTools::execute_auto`] call is blocked
    /// waiting for this exact `request_id`.
    pub(crate) fn handle_host_tool_response(&self, response: HostToolResponse) {
        let sender = self
            .pending_host_tool_requests
            .lock()
            .unwrap()
            .remove(&response.request_id.0);
        if let Some(sender) = sender {
            let _ = sender.send(response);
        }
    }

    pub(crate) fn session_list(&self) -> Vec<SessionSummary> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .map(|(session_id, entry)| SessionSummary {
                session_id: *session_id,
                provider_id: entry.provider_id.clone(),
            })
            .collect()
    }
}

/// The agent (child) side of the host-tool channel (guardrail 4 in
/// `docs/agent-runtime-split-design.md`): sends a `host_tool_request` over
/// the connection and blocks this session's dedicated thread on the matching
/// `host_tool_response`. Only `workspace.snapshot` is ever routed here today
/// (the same tool id Horizon's own `agent::host_tools::WorkspaceHostTools`
/// answers in-process) -- everything else falls through to `None`, letting
/// `execute_agent_tool` try the crate's own `tools::fs` auto tools next.
struct AgentdHostTools {
    session_id: SessionId,
    outgoing: UnboundedSender<Envelope>,
    pending: Arc<Mutex<HashMap<String, Sender<HostToolResponse>>>>,
}

impl HostTools for AgentdHostTools {
    fn execute_auto(&self, tool_id: &str, input: &serde_json::Value) -> Option<serde_json::Value> {
        if tool_id != "workspace.snapshot" {
            return None;
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        self.pending
            .lock()
            .unwrap()
            .insert(request_id.clone(), reply_tx);

        let envelope = Envelope {
            v: horizon_agent::wire::CONTRACT_VERSION,
            session_id: Some(self.session_id),
            body: EnvelopeBody::Control(Control::HostToolRequest(HostToolRequest {
                request_id: contract::RequestId(request_id.clone()),
                tool_id: tool_id.to_string(),
                input: input.clone(),
            })),
        };
        if self.outgoing.send(envelope).is_err() {
            self.pending.lock().unwrap().remove(&request_id);
            return None;
        }

        let response = reply_rx.recv_timeout(HOST_TOOL_TIMEOUT).ok();
        self.pending.lock().unwrap().remove(&request_id);
        response.map(|response| response.output)
    }
}

/// The session's whole lifetime, from `Initialize` through to the
/// provider's channel closing. Runs entirely synchronously on its own
/// dedicated thread -- see the module doc for why. Faithfully mirrors
/// `app::runtime::agent::spawn_agent_session`'s in-process shape, minus the
/// floem signals/effects: register the tool/live state, send `Initialize`,
/// then fold every provider event / bash completion / inbound command as it
/// arrives, forwarding the resulting (non-ephemeral) events to Horizon over
/// the wire exactly as `LiveState::extend_provider_events` folded them
/// in-process.
fn run_session(
    session_id: SessionId,
    provider_id: ProviderId,
    state: &AgentdState,
    outgoing: UnboundedSender<Envelope>,
    pending: Arc<Mutex<HashMap<String, Sender<HostToolResponse>>>>,
    inbound_rx: Receiver<Command>,
) {
    let Some(handle) = state.providers.start_session(&provider_id, session_id) else {
        let _ = outgoing.send(Envelope::event(
            session_id,
            Event::Error(AgentError {
                message: format!("Unknown provider `{}`.", provider_id.0),
            }),
        ));
        return;
    };

    let tool_state = ToolSessionState::for_current_dir(state.agent_config.tools);
    let live_state = match &state.writer {
        Some(writer) => {
            LiveState::with_event_log(session_id, Some(provider_id.clone()), writer.clone())
        }
        None => LiveState::with_disabled_persistence(),
    };
    let (bash_results_tx, bash_results_rx) = unbounded::<BashCompletion>();
    register_session_runtime(
        session_id,
        tool_state.clone(),
        live_state.clone(),
        bash_results_tx,
    );

    let host = AgentdHostTools {
        session_id,
        outgoing: outgoing.clone(),
        pending,
    };

    let commands_tx = handle.sender();
    let _ = commands_tx.send(Command::Initialize(Initialization {
        session_id,
        provider_id: provider_id.clone(),
    }));

    let provider_events = handle.events();

    loop {
        crossbeam_channel::select! {
            recv(provider_events) -> message => match message {
                Ok(provider_event) => handle_provider_event(
                    &host,
                    &tool_state,
                    &live_state,
                    &commands_tx,
                    &outgoing,
                    session_id,
                    provider_event,
                ),
                Err(_) => break,
            },
            recv(bash_results_rx) -> message => {
                if let Ok(completion) = message {
                    fold_bash_completion(&live_state, &commands_tx, &outgoing, session_id, completion);
                }
            },
            recv(inbound_rx) -> message => match message {
                Ok(command) => dispatch_inbound_command(
                    &live_state,
                    &commands_tx,
                    &outgoing,
                    session_id,
                    command,
                ),
                Err(_) => break,
            },
        }
    }

    unregister_session_runtime(session_id);
}

/// One provider event through the same processing pipeline
/// `app/runtime/agent.rs`'s effect used to run in-process
/// (`process_agent_provider_event` for tool execution/policy mapping, then
/// `LiveState::extend_provider_events` for the fold/persist) -- except the
/// resulting frame isn't published to a local `Frames` signal, it's
/// forwarded to Horizon as ordinary event envelopes. Ephemeral tool-call
/// progress (`ProviderEvent::tool_call_progress`) is folded into the local
/// frame (so a later `resolve_approval`'s `frame.tool_call_request` lookup
/// stays correct) but not forwarded -- see the module's step 3 notes in
/// `docs/agent-runtime-split-design.md` for why this trims the
/// streaming-tool-call-argument-preview feature in agentd mode.
fn handle_provider_event(
    host: &dyn HostTools,
    tool_state: &ToolSessionState,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    outgoing: &UnboundedSender<Envelope>,
    session_id: SessionId,
    provider_event: ProviderEvent,
) {
    let processing = process_agent_provider_event(host, tool_state, provider_event);
    for command in processing.provider_commands {
        let _ = commands_tx.send(command);
    }

    let to_forward: Vec<Event> = processing
        .horizon_events
        .iter()
        .filter(|event| event.tool_call_progress.is_none())
        .map(|event| event.event.clone())
        .collect();
    let _ = live_state.extend_provider_events(processing.horizon_events);
    for event in to_forward {
        let _ = outgoing.send(Envelope::event(session_id, event));
    }
}

/// The async-execution analogue of [`handle_provider_event`]'s fold, for a
/// `bash` call approved earlier (`ApprovalOutcome::Started` below) whose
/// result has now arrived on its own channel -- mirrors `app/runtime/
/// agent.rs::fold_bash_completion` exactly, forwarding the same events over
/// the wire instead of updating a local `Frames` signal.
fn fold_bash_completion(
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    outgoing: &UnboundedSender<Envelope>,
    session_id: SessionId,
    completion: BashCompletion,
) {
    let result = completion.result;
    if !should_fold_completion(&live_state.frame(), &result.call_id) {
        return;
    }

    let events = vec![
        Event::ToolCallFinished(result.clone()),
        Event::StateChanged(SessionState::WaitingForUser),
    ];
    let _ = live_state.extend_provider_events(events.clone().into_iter().map(Into::into));
    for event in events {
        let _ = outgoing.send(Envelope::event(session_id, event));
    }

    let _ = commands_tx.send(Command::ToolCallResult(result));
}

/// A `Command` envelope arriving from Horizon for this session.
/// `ApproveToolCall`/`DenyToolCall` are resolved right here (decision 2:
/// "Approval decisions stay in Horizon... resolved in agentd") via the same
/// `resolve_approval` Horizon's in-process pane click handler
/// (`app::command_actions::resolve_and_send_approval`) uses; everything else
/// forwards straight to the provider, unchanged.
fn dispatch_inbound_command(
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    outgoing: &UnboundedSender<Envelope>,
    session_id: SessionId,
    command: Command,
) {
    match command {
        Command::ApproveToolCall { call_id } => resolve_and_forward(
            live_state,
            commands_tx,
            outgoing,
            session_id,
            call_id,
            ApprovalDecision::Approve,
        ),
        Command::DenyToolCall { call_id, reason } => resolve_and_forward(
            live_state,
            commands_tx,
            outgoing,
            session_id,
            call_id,
            ApprovalDecision::Deny { reason },
        ),
        other => {
            let _ = commands_tx.send(other);
        }
    }
}

fn resolve_and_forward(
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    outgoing: &UnboundedSender<Envelope>,
    session_id: SessionId,
    call_id: ToolCallId,
    decision: ApprovalDecision,
) {
    let frame = live_state.frame();
    match resolve_approval(&frame, session_id, call_id, decision) {
        ApprovalOutcome::Executed {
            events, command, ..
        } => {
            for event in events {
                let _ = outgoing.send(Envelope::event(session_id, event));
            }
            let _ = commands_tx.send(command);
        }
        ApprovalOutcome::Started { events, .. } => {
            for event in events {
                let _ = outgoing.send(Envelope::event(session_id, event));
            }
        }
        ApprovalOutcome::Forward(command) => {
            let _ = commands_tx.send(command);
        }
        ApprovalOutcome::AlreadyResolved => {}
    }
}
