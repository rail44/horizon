//! Step 3-4 of `docs/agent-runtime-split-design.md`: the live, multiplexing
//! connection to `horizon-agentd` -- "one per process" (decision 4) -- that
//! [`AgentdConnection::start_session`]/[`AgentdConnection::attach_session`]
//! hand out a [`contract::SessionHandle`] against, indistinguishable at
//! every existing call site (`session::Registry::agent_sender`, the pane's
//! approve/deny/cancel commands, ...) from the in-process handle
//! `providers::ProviderRegistry::start_session` used to return before step 4
//! retired that path. That's the point: "the fold must not know which
//! transport delivered the events" extends to commands too, so nothing
//! outside `app::runtime::agent` has to know a session's history might have
//! come from a replay rather than a live stream from the start.
//!
//! Also hosts the Horizon side of the host-tool channel (guardrail 4):
//! [`wire_host_tool_responder`] answers `workspace.snapshot` requests
//! arriving from agentd by reading Horizon's own `Workspace`, reusing
//! `agent::host_tools::workspace_snapshot` -- the exact function Horizon's
//! former in-process `WorkspaceHostTools` used to call.
//!
//! Step 4 additions: [`AgentdConnection::session_list`]/[`reconnect_all_sessions`]
//! implement "on connect: `hello` -> `session_list` -> `session_load` for
//! every session" (startup, and the tail of a `Reload Agent Runtime`), and
//! [`reload_agent_runtime`] is the command's whole drain/respawn/reconnect
//! sequence.

use std::collections::HashMap;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use floem::ext_event::create_signal_from_channel;
use floem::prelude::*;
use floem::reactive::create_effect;

use horizon_agent::wire::{
    self, Control, Envelope, EnvelopeBody, HostToolRequest, HostToolResponse, SessionLoad,
    SessionNew, SessionSummary,
};

use crate::agent::contract::{
    self, Command, Error as AgentError, Event, ProviderEvent, ProviderId,
};
use crate::agent::host_tools::workspace_snapshot;
use crate::agent::live::LiveState;
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::{PaneKind, Workspace};

type AgentSessionId = contract::SessionId;

/// [`AgentdConnection::connect`]'s result: the connection plus the receivers
/// [`wire_host_tool_responder`]/[`wire_skipped_lines_status`] need. Named
/// purely to satisfy clippy's `type_complexity` lint at the two places this
/// exact shape appears ([`AgentdConnection::connect`]'s return type and
/// [`run_connection`]'s `outcome_tx` parameter) -- no semantic weight beyond
/// that.
type ConnectOutcome = Result<
    (
        AgentdConnection,
        Receiver<HostToolRequestEnvelope>,
        Receiver<String>,
    ),
    String,
>;

/// How long [`AgentdConnection::session_list`] waits for agentd's reply
/// before giving up and treating it as "no sessions" -- a same-host Unix
/// socket round trip, so generous relative to how long that should ever
/// actually take.
const SESSION_LIST_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Answers [`Self::session_list`]'s blocking round trip: set just before
    /// sending `Control::SessionList`, taken (and replied to) by
    /// `dispatch_incoming` when the matching `Control::SessionListResult`
    /// arrives. `session_list` has no request id on the wire (unlike
    /// host-tool requests) -- acceptable because Horizon only ever has one
    /// such round trip outstanding at a time (startup, or a `Reload Agent
    /// Runtime`), never issued concurrently with another.
    pending_session_list: Arc<Mutex<Option<Sender<Vec<SessionSummary>>>>>,
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
    /// Returns the connection plus the receivers [`wire_host_tool_responder`]/
    /// [`wire_skipped_lines_status`] need -- kept separate from `Self` (see
    /// [`HostToolRequestEnvelope`]'s doc comment) rather than exposed as
    /// methods callable more than once.
    pub(crate) fn connect(socket_path: &Path) -> ConnectOutcome {
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
    /// back a [`contract::SessionHandle`] via [`Self::register_session_routing`].
    /// Indistinguishable, from the caller's side, from `providers::
    /// ProviderRegistry::start_session`'s former in-process handle.
    /// `role_id` is `None` for every production call site today (the GUI
    /// has no role-picking command yet -- see
    /// `docs/plans/agent-foundation/03-roles-and-config-agent.md`); it's a
    /// parameter now so a future "New Configuration Agent" command has
    /// somewhere to plug in without another signature change.
    pub(crate) fn start_session(
        &self,
        session_id: AgentSessionId,
        provider_id: ProviderId,
        role_id: Option<horizon_agent::roles::RoleId>,
    ) -> contract::SessionHandle {
        let handle = self.register_session_routing(session_id);
        let _ = self
            .outgoing
            .send(Envelope::control(Control::SessionNew(SessionNew {
                session_id,
                provider_id,
                role_id,
            })));
        handle
    }

    /// Attaches to a session agentd already hosts (found via
    /// [`Self::session_list`] -- either resumed from its own log at startup,
    /// or left running from a session Horizon created earlier this
    /// connection) rather than creating a new one: sends `session_load`
    /// instead of `session_new`, so agentd replays the session's committed
    /// events onto the handle this returns instead of starting it fresh.
    /// The one production call site is [`reconnect_all_sessions`].
    pub(crate) fn attach_session(&self, session_id: AgentSessionId) -> contract::SessionHandle {
        let handle = self.register_session_routing(session_id);
        let _ = self
            .outgoing
            .send(Envelope::control(Control::SessionLoad(SessionLoad {
                session_id,
            })));
        handle
    }

    /// The plumbing [`Self::start_session`]/[`Self::attach_session`] share:
    /// registers this session id's event route (so `dispatch_incoming` can
    /// find it) and spawns the small draining thread that forwards the
    /// resulting `SessionHandle`'s commands as `command` envelopes (commands
    /// arrive from the UI thread, which isn't async). Doesn't itself send
    /// anything session-starting -- that's the one line that differs
    /// between the two callers.
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

    /// Asks agentd for every session it currently hosts (`docs/agent-
    /// runtime-split-design.md` step 4's "`hello` -> `session_list` ->
    /// `session_load` for every session"), blocking the calling thread for
    /// up to [`SESSION_LIST_TIMEOUT`]. Callers that must not block the UI
    /// thread (the `Reload Agent Runtime` command) call this from a
    /// background thread, the same way `Self::connect` itself blocks only
    /// its own dedicated thread.
    pub(crate) fn session_list(&self) -> Vec<SessionSummary> {
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
            .recv_timeout(SESSION_LIST_TIMEOUT)
            .unwrap_or_default()
    }

    /// Asks agentd to drain: flush and exit (`main::run`'s `Control::Drain`
    /// handling in `horizon-agentd`). Best-effort and fire-and-forget --
    /// the caller (`reload_agent_runtime`) doesn't wait for a reply, just
    /// for the old process to actually be gone (observed indirectly, by the
    /// next connect attempt succeeding against a fresh process).
    pub(crate) fn drain(&self) {
        let _ = self.outgoing.send(Envelope::control(Control::Drain));
    }

    /// A connection with no live socket behind it: every outgoing envelope
    /// is silently dropped (the paired receiver is discarded immediately).
    /// For tests that only need to prove *dispatch* -- does `start_session`
    /// produce the right shape -- without spawning a real `horizon-agentd`
    /// process.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let (outgoing, _receiver) = tokio::sync::mpsc::unbounded_channel();
        Self {
            outgoing,
            session_events: Arc::new(Mutex::new(HashMap::new())),
            pending_session_list: Arc::new(Mutex::new(None)),
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

/// Wires the Horizon side of the restored skipped-lines status feature
/// (`docs/agent-runtime-split-design.md`'s step-3 notes, "Skipped-lines
/// status reporting is omitted"): folds every `Control::SkippedLines`
/// summary agentd sends over `skipped_lines` into `agent_state_status`, the
/// same free-floating status the status bar (`app::status_bar`) already
/// renders -- no new UI surface needed. Called once per connection
/// (`agent::agentd_client::connect_agentd_at_startup` and
/// [`reload_agent_runtime`]'s reconnect), mirroring
/// [`wire_host_tool_responder`]'s channel-to-signal bridge shape.
pub(crate) fn wire_skipped_lines_status(
    skipped_lines: Receiver<String>,
    agent_state_status: RwSignal<Option<String>>,
) {
    let summaries = create_signal_from_channel(skipped_lines);
    create_effect(move |_| {
        if let Some(summary) = summaries.get() {
            agent_state_status.set(Some(format!("Agent event log: {summary}")));
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
async fn run_connection(socket_path: &Path, outcome_tx: std::sync::mpsc::Sender<ConnectOutcome>) {
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
    let pending_session_list = Arc::new(Mutex::new(None));
    let (host_tool_tx, host_tool_rx) = unbounded::<HostToolRequestEnvelope>();
    let (skipped_lines_tx, skipped_lines_rx) = unbounded::<String>();

    let connection = AgentdConnection {
        outgoing: outgoing_tx,
        session_events: session_events.clone(),
        pending_session_list: pending_session_list.clone(),
    };
    if outcome_tx
        .send(Ok((connection, host_tool_rx, skipped_lines_rx)))
        .is_err()
    {
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
                Ok(Some(envelope)) => dispatch_incoming(
                    envelope,
                    &session_events,
                    &pending_session_list,
                    &host_tool_tx,
                    &skipped_lines_tx,
                ),
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
/// session map), a `host_tool_request` to the responder effect's channel, an
/// ephemeral tool-call-progress tick to the same session-events route an
/// ordinary event takes (see below), and a startup skipped-lines summary to
/// its own dedicated channel. Horizon never expects a `Command` or most
/// other `Control` variants from agentd -- silently ignored rather than
/// treated as an error, matching `horizon-agentd`'s own tolerance for
/// out-of-place messages.
fn dispatch_incoming(
    envelope: Envelope,
    session_events: &Arc<Mutex<HashMap<AgentSessionId, Sender<ProviderEvent>>>>,
    pending_session_list: &Arc<Mutex<Option<Sender<Vec<SessionSummary>>>>>,
    host_tool_tx: &Sender<HostToolRequestEnvelope>,
    skipped_lines_tx: &Sender<String>,
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
        // Restores the step-3 trim (`docs/agent-runtime-split-design.md`):
        // folded through `ProviderEvent::tool_call_progress` -> the exact
        // same `apply_tool_call_progress_to_frame` path a persisted event
        // would take in `fold_agent_session_events`/`State::
        // extend_provider_events` -- this session-events route doesn't care
        // whether the `ProviderEvent` it carries came from an `Event`
        // envelope or a `Control::ToolCallProgress` one.
        EnvelopeBody::Control(Control::ToolCallProgress(progress)) => {
            let Some(session_id) = envelope.session_id else {
                return;
            };
            if let Some(sender) = session_events.lock().unwrap().get(&session_id) {
                let _ = sender.send(ProviderEvent::tool_call_progress(progress));
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
        EnvelopeBody::Control(Control::SessionListResult(summaries)) => {
            if let Some(reply_tx) = pending_session_list.lock().unwrap().take() {
                let _ = reply_tx.send(summaries);
            }
        }
        EnvelopeBody::Control(Control::SkippedLines(summary)) => {
            let _ = skipped_lines_tx.send(summary);
        }
        _ => {}
    }
}

/// The connection dropped (or a malformed message closed it) -- per step
/// 3's explicit scope ("if the connection drops, surface an error on
/// affected sessions; no *automatic* reconnect" -- step 4 adds a *manual*
/// one, `Reload Agent Runtime`), pushes a synthetic `Event::Error` into
/// every currently-registered session's event stream so it folds through
/// the ordinary path and shows up in that session's transcript, rather than
/// the pane silently going quiet.
fn mark_connection_lost(
    session_events: &Arc<Mutex<HashMap<AgentSessionId, Sender<ProviderEvent>>>>,
) {
    let senders: Vec<Sender<ProviderEvent>> =
        session_events.lock().unwrap().values().cloned().collect();
    let event = Event::Error(AgentError {
        message: "Lost connection to horizon-agentd -- use \"Reload Agent Runtime\" to \
                  reconnect (see docs/agent-runtime-split-design.md)."
            .to_string(),
    });
    for sender in senders {
        let _ = sender.send(ProviderEvent::from(event.clone()));
    }
}

// --- step 4: reconnect and the `Reload Agent Runtime` command ---------------

/// `docs/agent-runtime-split-design.md` step 4's "on connect: `hello` ->
/// `session_list` -> `session_load` for every session". The one production
/// call site is `app::state::AppState::new` (right after a successful
/// `agentd_client::connect_agentd_at_startup`) -- a fresh Horizon process
/// has no panes yet, so every session `session_list` reports surfaces as a
/// newly-registered detached session ("survival made visible"). Blocks the
/// calling thread on `AgentdConnection::session_list`'s round trip,
/// acceptable at startup for the same reason `AgentdConnection::connect`
/// itself is (see that method's doc comment); [`reload_agent_runtime`] does
/// the equivalent work off the UI thread instead, since it can run at any
/// point during a session.
pub(crate) fn reconnect_all_sessions(
    connection: &AgentdConnection,
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
) {
    attach_sessions(
        connection,
        connection.session_list(),
        workspace,
        frames,
        sessions,
    );
}

/// The per-summary half of [`reconnect_all_sessions`], factored out so
/// [`reload_agent_runtime`] can fetch `session_list` on its own background
/// thread (a blocking round trip) and run only this -- which touches floem
/// signals and so must run on the UI thread -- once it has the answer.
pub(crate) fn attach_sessions(
    connection: &AgentdConnection,
    summaries: Vec<SessionSummary>,
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
) {
    for summary in summaries {
        let session_id: SessionId = summary.session_id.into();
        // A no-op if the workspace already has a pane referencing this
        // session ("sessions Horizon has panes for reattach seamlessly");
        // otherwise registers it as a fresh detached session ("sessions
        // Horizon didn't know about surface as detached sessions").
        workspace.update(|ws| ws.register_detached_session(PaneKind::Agent, session_id));

        let handle = connection.attach_session(summary.session_id);
        fold_agent_session_events(session_id, handle, frames, sessions);
    }
}

/// Folds one session's (re)connected event stream into `Frames`/`Registry` --
/// the tail `app::runtime::agent::spawn_agent_session` and [`attach_sessions`]
/// share, whether the handle came from a brand-new `session_new` or a
/// `session_load` replay: either way, the events arriving on it already went
/// through agentd's own `process_agent_provider_event` pipeline, so this
/// side only has to fold and publish -- the same `LiveState::
/// extend_provider_events` + `Frames::update_agent_frame` step every agentd-
/// routed session has used since step 3, just shared explicitly now that
/// there are two callers instead of one.
pub(crate) fn fold_agent_session_events(
    session_id: SessionId,
    handle: contract::SessionHandle,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
) {
    let events = create_signal_from_channel(handle.events());
    sessions.update(|registry| {
        registry.insert_agent(session_id, handle);
    });

    let runtime_state = LiveState::with_disabled_persistence();
    create_effect(move |_| {
        if let Some(event) = events.get() {
            let frame = runtime_state.extend_provider_events(std::iter::once(event));
            frames.update(|frames| frames.update_agent_frame(session_id, frame));
        }
    });
}

/// How long [`wait_for_drain`] polls for the old `horizon-agentd` to
/// actually stop accepting connections before giving up and reporting a
/// drain timeout -- generous for a same-host process that flushes a socket
/// write and calls `std::process::exit` (should be near-instant), bounded so
/// a genuinely wedged old process produces a loud, specific failure instead
/// of `reload_agent_runtime` silently reconnecting to it (defeating the
/// whole point of "reload") or hanging indefinitely.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Poll interval for [`wait_for_drain`] -- fine-grained relative to
/// [`DRAIN_TIMEOUT`] since a same-host Unix socket connect attempt is cheap.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Blocks (this is [`reload_agent_runtime`]'s own background thread, not the
/// UI thread) until nothing answers a connection attempt on `socket_path`
/// -- i.e. the drained old `horizon-agentd` has actually exited, whether or
/// not it got around to unlinking the socket file itself (`Control::Drain`'s
/// handler calls `std::process::exit` directly, skipping the normal-exit
/// unlink path in `main::run` -- a stale file with nothing listening still
/// fails to connect, which is all this needs to observe) -- or [`DRAIN_TIMEOUT`]
/// elapses, in which case it's reported as a failure rather than silently
/// falling through to a spawn-or-connect attempt that might reattach to a
/// still-alive old process instead of the rebuilt binary. Synchronous
/// (`std::os::unix::net`, not `tokio`): this thread has no async runtime of
/// its own (see [`reload_agent_runtime`]'s doc comment).
fn wait_for_drain(socket_path: &Path) -> Result<(), String> {
    let deadline = Instant::now() + DRAIN_TIMEOUT;
    loop {
        if StdUnixStream::connect(socket_path).is_err() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "horizon-agentd did not drain within {:.1}s",
                DRAIN_TIMEOUT.as_secs_f64()
            ));
        }
        thread::sleep(DRAIN_POLL_INTERVAL);
    }
}

/// One stage of `reload_agent_runtime`'s progress, formatted by
/// [`reload_stage_status`] into the short strings that land in
/// `agent_state_status` (the status bar) while a reload is in flight --
/// separated from the sending/timing logic so the message text and its
/// ordering are directly unit-testable without spinning up floem's signal
/// plumbing or a real `horizon-agentd` process (see this module's tests).
enum ReloadStage {
    Draining,
    Spawning,
    Replaying(usize),
    Reconnected(Duration),
}

/// Formats `stage` into the short status-bar text `reload_agent_runtime`
/// pushes through `agent_state_status` at each point in the reload sequence
/// -- kept under its own name/`…` ellipsis style so "still in progress" vs.
/// "done" (`Reconnected`, no trailing ellipsis) reads unambiguously.
fn reload_stage_status(stage: ReloadStage) -> String {
    match stage {
        ReloadStage::Draining => "agent runtime: draining…".to_string(),
        ReloadStage::Spawning => "agent runtime: spawning…".to_string(),
        ReloadStage::Replaying(count) => format!(
            "agent runtime: replaying {count} session{}…",
            if count == 1 { "" } else { "s" }
        ),
        ReloadStage::Reconnected(elapsed) => {
            format!("agent runtime: reconnected ({:.1}s)", elapsed.as_secs_f64())
        }
    }
}

/// The result [`reload_agent_runtime`]'s background thread hands back to its
/// `create_effect` callback -- everything needed to finish reconnecting on
/// the UI thread (`Connected`), or the error string to surface
/// (`Failed`, e.g. a contract-version mismatch's "reload required" text, or
/// [`wait_for_drain`]'s timeout message).
#[derive(Clone)]
enum ReloadOutcome {
    Connected {
        connection: AgentdConnection,
        host_tool_requests: Receiver<HostToolRequestEnvelope>,
        skipped_lines: Receiver<String>,
        summaries: Vec<SessionSummary>,
        elapsed: Duration,
    },
    Failed(String),
}

/// The whole `Reload Agent Runtime` command (`app::commands::CommandId::
/// ReloadAgentRuntime`, dispatched from `app::command_actions`): drain the
/// current connection (if any), wait for it to actually exit
/// ([`wait_for_drain`]), spawn-or-connect the (possibly just-rebuilt)
/// binary, then run step 4's reconnect sequence against it -- "drain ->
/// agentd flushes and exits -> Horizon spawns the rebuilt binary ->
/// reconnect -> session_load" per the design. `agentd_connection` is set to
/// `None` immediately (so no session tries to route through the dying
/// connection while this is in flight) and staged progress -- draining,
/// spawning, replaying N sessions, reconnected -- is reported through
/// `agent_state_status`, the same free-floating status signal `app::
/// status_bar` already renders, so a reload that used to look like it did
/// nothing until it either finished or failed now visibly moves through
/// each phase. Every failure path (drain timeout, spawn failure, handshake
/// failure) funnels through the same `ReloadOutcome::Failed` ->
/// [`reload_failure_status`] mapping, so none of them are silent either.
///
/// The blocking work (`wait_for_drain`, `AgentdConnection::connect`,
/// `session_list`'s round trip) all runs on a background thread; only the
/// `create_effect` callbacks that receive progress/results touch floem
/// signals, so this never stalls the UI thread the way blocking at Horizon
/// startup is allowed to.
pub(crate) fn reload_agent_runtime(
    current: Option<AgentdConnection>,
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    agentd_connection: RwSignal<Option<AgentdConnection>>,
    agent_state_status: RwSignal<Option<String>>,
) {
    if let Some(connection) = &current {
        connection.drain();
    }
    agentd_connection.set(None);
    agent_state_status.set(Some(reload_stage_status(ReloadStage::Draining)));

    let (progress_tx, progress_rx) = unbounded::<String>();
    let (outcome_tx, outcome_rx) = unbounded::<ReloadOutcome>();
    thread::spawn(move || {
        let started = Instant::now();
        let socket_path = horizon_agent::socket::default_socket_path();

        if let Err(error) = wait_for_drain(&socket_path) {
            let _ = outcome_tx.send(ReloadOutcome::Failed(error));
            return;
        }

        let _ = progress_tx.send(reload_stage_status(ReloadStage::Spawning));
        let outcome = match AgentdConnection::connect(&socket_path) {
            Ok((connection, host_tool_requests, skipped_lines)) => {
                let summaries = connection.session_list();
                let _ =
                    progress_tx.send(reload_stage_status(ReloadStage::Replaying(summaries.len())));
                ReloadOutcome::Connected {
                    connection,
                    host_tool_requests,
                    skipped_lines,
                    summaries,
                    elapsed: started.elapsed(),
                }
            }
            Err(error) => ReloadOutcome::Failed(error),
        };
        let _ = outcome_tx.send(outcome);
    });

    let progress_signal = create_signal_from_channel(progress_rx);
    create_effect(move |_| {
        if let Some(message) = progress_signal.get() {
            agent_state_status.set(Some(message));
        }
    });

    let outcome_signal = create_signal_from_channel(outcome_rx);
    create_effect(move |_| {
        if let Some(outcome) = outcome_signal.get() {
            match outcome {
                ReloadOutcome::Connected {
                    connection,
                    host_tool_requests,
                    skipped_lines,
                    summaries,
                    elapsed,
                } => {
                    wire_host_tool_responder(connection.clone(), host_tool_requests, workspace);
                    wire_skipped_lines_status(skipped_lines, agent_state_status);
                    attach_sessions(&connection, summaries, workspace, frames, sessions);
                    agentd_connection.set(Some(connection));
                    agent_state_status
                        .set(Some(reload_stage_status(ReloadStage::Reconnected(elapsed))));
                }
                ReloadOutcome::Failed(error) => {
                    agent_state_status.set(Some(reload_failure_status(&error)));
                }
            }
        }
    });
}

/// Maps a failed reconnect attempt's error string to the
/// `agent_state_status` message `reload_agent_runtime` shows -- a contract-
/// version mismatch (the error text already contains "reload required",
/// verbatim, from `agent::agentd_client::handshake_over`) is called out with
/// the design's own wording ("agent runtime reload required") rather than a
/// generic failure message, since the fix is specific (rebuild, don't just
/// retry) and must never be silent.
fn reload_failure_status(error: &str) -> String {
    if error.contains("reload required") {
        format!("agent runtime reload required: {error}")
    } else {
        format!("Agent runtime reload failed: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_failure_status_calls_out_a_version_mismatch_as_reload_required() {
        let status = reload_failure_status(
            "horizon-agentd contract version mismatch: horizon speaks v1, agentd speaks v2 -- \
             reload required",
        );
        assert!(
            status.starts_with("agent runtime reload required"),
            "status was: {status}"
        );
    }

    #[test]
    fn reload_failure_status_reports_other_failures_generically() {
        let status = reload_failure_status(
            "timed out waiting for horizon-agentd to accept connections on /tmp/x.sock",
        );
        assert!(
            status.starts_with("Agent runtime reload failed"),
            "status was: {status}"
        );
        assert!(!status.to_ascii_lowercase().contains("reload required"));
    }

    /// A drain timeout's error text must flow through the same generic
    /// failure mapping as any other reconnect failure -- proven directly
    /// against [`wait_for_drain`]'s own message shape rather than the
    /// timing behavior itself (see `wait_for_drain_...` below for that),
    /// since this is the piece that decides what the status bar shows.
    #[test]
    fn reload_failure_status_reports_a_drain_timeout_generically() {
        let status = reload_failure_status("horizon-agentd did not drain within 2.0s");
        assert!(
            status.starts_with("Agent runtime reload failed"),
            "status was: {status}"
        );
    }

    /// [`wait_for_drain`] must return immediately (well under its own
    /// timeout) when nothing is listening on the socket path at all -- the
    /// common case (a stale/nonexistent path, or a cleanly drained process
    /// that already exited) -- rather than always waiting out the full
    /// budget.
    #[test]
    fn wait_for_drain_returns_immediately_when_nothing_is_listening() {
        let path = std::env::temp_dir().join(format!(
            "horizon-agentd-runtime-test-no-such-socket-{}.sock",
            uuid::Uuid::new_v4()
        ));
        let started = Instant::now();
        wait_for_drain(&path).expect("nothing listening should be reported as drained");
        assert!(
            started.elapsed() < DRAIN_TIMEOUT / 2,
            "wait_for_drain should not wait out its timeout when nothing is listening"
        );
    }

    /// [`wait_for_drain`] must time out (rather than hang forever) against a
    /// socket that keeps accepting connections the whole time -- modeling a
    /// wedged old `horizon-agentd` that never actually drains.
    #[test]
    fn wait_for_drain_times_out_against_a_socket_that_keeps_accepting() {
        let path = std::env::temp_dir().join(format!(
            "horizon-agentd-runtime-test-live-socket-{}.sock",
            uuid::Uuid::new_v4()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind test listener");
        // Accept (and immediately drop) connections in the background so
        // `wait_for_drain`'s repeated connect attempts keep succeeding for
        // its whole polling window, the way a still-alive process would.
        let accepting = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let accepting_in_thread = accepting.clone();
        let acceptor = thread::spawn(move || {
            listener.set_nonblocking(true).expect("set nonblocking");
            while accepting_in_thread.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = listener.accept();
                thread::sleep(Duration::from_millis(5));
            }
        });

        let error = wait_for_drain(&path).expect_err("a socket that never drains must time out");
        assert!(
            error.contains("did not drain"),
            "error message was: {error}"
        );

        accepting.store(false, std::sync::atomic::Ordering::SeqCst);
        acceptor.join().expect("acceptor thread should exit");
        let _ = std::fs::remove_file(&path);
    }

    /// The four staged messages `reload_agent_runtime` pushes through
    /// `agent_state_status` must be short, use the same "agent runtime: "
    /// prefix, and -- since `agent_state_status` has no history, just a
    /// latest-value label (`app::status_bar`) -- arrive in the same order
    /// the reload sequence produces them: draining, spawning, replaying,
    /// reconnected. Unit-tested at this message-formatting/ordering level
    /// rather than end-to-end through floem's signal plumbing against a
    /// real spawned `horizon-agentd`, matching the precedent already
    /// accepted for this function's own spawn/reconnect orchestration (see
    /// `docs/agent-runtime-split-design.md`'s step 4 testing-scope notes).
    #[test]
    fn reload_stage_status_messages_are_short_and_in_expected_order() {
        let stages = vec![
            ReloadStage::Draining,
            ReloadStage::Spawning,
            ReloadStage::Replaying(2),
            ReloadStage::Reconnected(Duration::from_millis(1500)),
        ];
        let messages: Vec<String> = stages.into_iter().map(reload_stage_status).collect();
        assert_eq!(
            messages,
            vec![
                "agent runtime: draining…".to_string(),
                "agent runtime: spawning…".to_string(),
                "agent runtime: replaying 2 sessions…".to_string(),
                "agent runtime: reconnected (1.5s)".to_string(),
            ]
        );
        for message in &messages {
            assert!(
                message.len() < 60,
                "status messages should stay short for the status bar, got: {message}"
            );
        }
    }

    #[test]
    fn reload_stage_status_singularizes_a_single_session() {
        assert_eq!(
            reload_stage_status(ReloadStage::Replaying(1)),
            "agent runtime: replaying 1 session…"
        );
    }
}
