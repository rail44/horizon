//! Session hosting: `docs/agent-runtime-split-design.md` steps 3-4. Each
//! `Control::SessionNew` (or a resumed session found in the event log at
//! startup, see [`resume_persisted_sessions`]) spawns a dedicated OS thread
//! that owns the real session loop (the same `providers`/`tools`/
//! `persistence` machinery Horizon used to run in-process), and command/event
//! envelopes are routed to/from that thread by session id.
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
//! **Sessions are scoped to the process, not the connection (step 4).**
//! `AgentdState::sessions`/`pending_host_tool_requests`/`outgoing` are
//! process-lifetime (built once in `main`, shared via `Arc`) rather than
//! recreated per accepted connection: a session's thread outlives any one
//! connection, and a fresh connection re-targets the *same* running
//! sessions rather than starting over. `outgoing` is the seam that makes
//! that possible — a swappable "current connection's writer channel" cell
//! (`Connection::new` installs it, `Connection::disconnect` clears it) that
//! every session thread sends through by reference, so a session spawned
//! before any connection existed (a resumed session at startup) and a
//! session spawned mid-connection are indistinguishable once they're
//! running: both just send through whatever `outgoing` currently points at,
//! silently dropping events when it's `None` (no client to see them).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};

use horizon_agent::config::AgentConfig;
use horizon_agent::contract::{
    self, Command, Error as AgentError, Event, Initialization, ProviderEvent, ProviderId,
    ProviderRegistry, SessionId, SessionState, ToolCallId, TurnEndReason,
};
use horizon_agent::frame::{agent_frame_from_events, AgentFrame, AgentFrameItem};
use horizon_agent::live::LiveState;
use horizon_agent::persistence::event_log::{Appender, Record, WriterHandle};
use horizon_agent::tools::{
    cancelled_tool_call_result, process_agent_provider_event, register_session_runtime,
    resolve_approval, should_fold_completion, unregister_session_runtime, ApprovalDecision,
    ApprovalOutcome, BashCompletion, HostTools, ToolSessionState,
};
use horizon_agent::wire::{
    Control, Envelope, EnvelopeBody, HostToolRequest, HostToolResponse, SessionNew, SessionSummary,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Notify;

/// How long a session thread waits for Horizon to answer a `host_tool_*`
/// round trip before giving up. Generous but finite: a client that never
/// answers must not hang a session forever.
const HOST_TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// How long [`Connection::replay_events`] waits for a live session's own
/// thread to answer a replay request. Purely a local channel hop (no I/O),
/// so this only ever protects against a session thread that's wedged.
const REPLAY_TIMEOUT: Duration = Duration::from_secs(5);

/// The connection-swappable outgoing envelope queue every session thread
/// sends through — see the module doc's "sessions are scoped to the
/// process" note.
type SharedOutgoing = Mutex<Option<UnboundedSender<Envelope>>>;

/// Process-lifetime state, built once in `main` and shared (via `Arc`) by
/// every connection `horizon-agentd` ever serves, and by every session
/// thread regardless of which (if any) connection is currently live.
pub(crate) struct AgentdState {
    pub(crate) providers: ProviderRegistry,
    pub(crate) agent_config: AgentConfig,
    /// `None` until [`Self::set_writer`] runs (or forever, if the event log
    /// couldn't be opened -- mirrors Horizon's own graceful degrade in
    /// `app::runtime::agent::open_agent_runtime_state_store`: sessions still
    /// run, just without persistence). A `Mutex` rather than a plain field
    /// because `main` now binds the socket and starts accepting connections
    /// *before* the event log is opened (see the bind-first fix in `main`'s
    /// doc comment) -- this is filled in once that finishes, on whatever
    /// thread happens to be running by then.
    writer: Mutex<Option<WriterHandle>>,
    sessions: Mutex<HashMap<SessionId, SessionEntry>>,
    pending_host_tool_requests: Mutex<HashMap<String, Sender<HostToolResponse>>>,
    outgoing: SharedOutgoing,
    /// Flips once (see [`Self::mark_resume_ready`]) after
    /// [`resume_persisted_sessions`] finishes populating `sessions` from the
    /// log. `session_list`/`session_load` must not answer while this is
    /// still false -- see [`Self::wait_until_resume_ready`] -- or a
    /// (re)connecting client would see a partial (or, right after bind,
    /// completely empty) view of sessions that genuinely exist. `hello`/
    /// `ping` never check this: they don't depend on session state at all,
    /// which is the whole point of binding first (see `main`).
    resume_ready: AtomicBool,
    resume_notify: Notify,
    /// This process's own startup event-log corruption diagnostics
    /// (`persistence::event_log::ReadReport::skipped_summary`), `None` until
    /// [`Self::set_skipped_lines_summary`] runs (or forever, if the startup
    /// read found nothing to skip) -- see [`Self::skipped_lines_summary`]
    /// and `main::run_session_hosting_loop`, which reports this once per
    /// connection restoring the step-3 trim recorded in
    /// `docs/agent-runtime-split-design.md`.
    skipped_lines_summary: Mutex<Option<String>>,
}

impl AgentdState {
    pub(crate) fn new(
        providers: ProviderRegistry,
        agent_config: AgentConfig,
        writer: Option<WriterHandle>,
    ) -> Self {
        Self {
            providers,
            agent_config,
            writer: Mutex::new(writer),
            sessions: Mutex::new(HashMap::new()),
            pending_host_tool_requests: Mutex::new(HashMap::new()),
            outgoing: Mutex::new(None),
            resume_ready: AtomicBool::new(false),
            resume_notify: Notify::new(),
            skipped_lines_summary: Mutex::new(None),
        }
    }

    pub(crate) fn writer(&self) -> Option<WriterHandle> {
        self.writer.lock().unwrap().clone()
    }

    pub(crate) fn set_writer(&self, writer: Option<WriterHandle>) {
        *self.writer.lock().unwrap() = writer;
    }

    /// Called once from [`crate::spawn_resume_task`], alongside
    /// [`Self::set_writer`] -- before [`Self::mark_resume_ready`], so a
    /// connection's readiness-gated summary send (see `main::
    /// run_session_hosting_loop`) always observes the final value.
    pub(crate) fn set_skipped_lines_summary(&self, summary: Option<String>) {
        *self.skipped_lines_summary.lock().unwrap() = summary;
    }

    pub(crate) fn skipped_lines_summary(&self) -> Option<String> {
        self.skipped_lines_summary.lock().unwrap().clone()
    }

    /// Called exactly once, after [`resume_persisted_sessions`] returns --
    /// see `main`'s startup sequencing.
    pub(crate) fn mark_resume_ready(&self) {
        self.resume_ready.store(true, Ordering::SeqCst);
        self.resume_notify.notify_waiters();
    }

    /// Blocks (async, so it only ever parks the calling connection's own
    /// task -- see `main::run_session_hosting_loop`) until
    /// [`Self::mark_resume_ready`] has run. Builds the `Notified` future
    /// before re-checking the flag, per `tokio::sync::Notify`'s documented
    /// pattern for "wait for a one-time event without a missed-wakeup race"
    /// -- otherwise a `mark_resume_ready` landing between the flag check and
    /// the `.await` would never be observed.
    pub(crate) async fn wait_until_resume_ready(&self) {
        loop {
            let notified = self.resume_notify.notified();
            if self.resume_ready.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

struct SessionEntry {
    provider_id: ProviderId,
    inbound: Sender<Command>,
    /// Answers a `session_load` for this session: the session's own thread
    /// receives a one-shot reply channel here and sends back everything its
    /// `LiveState::events()` has accumulated — see
    /// [`Connection::replay_events`].
    replay: Sender<Sender<Vec<Event>>>,
}

/// Spawns the dedicated thread for one session — the shared spawn path for
/// both a fresh `Control::SessionNew` ([`Connection::handle_session_new`])
/// and a session resumed from the persisted log at startup
/// ([`resume_persisted_sessions`]); `history` is empty for the former,
/// already-committed events for the latter.
fn spawn_session_thread(
    state: Arc<AgentdState>,
    session_id: SessionId,
    provider_id: ProviderId,
    history: Vec<Event>,
) {
    let (inbound_tx, inbound_rx) = unbounded::<Command>();
    let (replay_tx, replay_rx) = unbounded::<Sender<Vec<Event>>>();
    state.sessions.lock().unwrap().insert(
        session_id,
        SessionEntry {
            provider_id: provider_id.clone(),
            inbound: inbound_tx,
            replay: replay_tx,
        },
    );

    let thread_state = state.clone();
    thread::spawn(move || {
        run_session(
            session_id,
            provider_id,
            &thread_state,
            inbound_rx,
            replay_rx,
            history,
        );
        thread_state.sessions.lock().unwrap().remove(&session_id);
    });
}

/// `docs/agent-runtime-split-design.md` step 4, "agentd start": reads the
/// startup read's records and, for each session found (grouped here by
/// `session_id`), resumes it live: any turn still open at that session's
/// tail (`AgentFrame::is_turn_in_flight`, the same "is a turn in flight"
/// check the palette's `Cancel Agent Turn` enablement uses) is committed
/// durably as cancelled *before* the session goes live again, per "any turn
/// open at the log's tail is committed as cancelled" — then a fresh thread
/// is spawned exactly as `Control::SessionNew` would, seeded with the whole
/// history so its first frame is complete. A no-op when there's no writer
/// (persistence disabled for this run — nothing to resume from or write a
/// fixup to).
///
/// Sessions whose log already ends in a terminal state ([`session_is_dead`])
/// are skipped entirely rather than resumed: there is no live provider
/// process left behind a terminated/exited session, so reviving its thread
/// would just leave it parked forever, and doing this for *every* session
/// ever created makes startup cost (and thread count) grow without bound
/// with history -- exactly what was observed as "every historical session
/// comes back as a ghost" before this filter existed.
pub(crate) fn resume_persisted_sessions(state: &Arc<AgentdState>, records: Vec<Record>) {
    let Some(writer) = state.writer() else {
        return;
    };

    let mut by_session: HashMap<SessionId, Vec<Record>> = HashMap::new();
    for record in records {
        by_session
            .entry(record.session_id)
            .or_default()
            .push(record);
    }

    for (session_id, mut session_records) in by_session {
        session_records.sort_by_key(|record| record.sequence);
        let provider_id = session_records
            .iter()
            .rev()
            .find_map(|record| record.provider_id.clone())
            .unwrap_or_else(|| state.providers.default_provider_id());
        let mut events: Vec<Event> = session_records
            .into_iter()
            .map(|record| record.event)
            .collect();

        let frame = agent_frame_from_events(&events);
        if session_is_dead(&frame) {
            eprintln!("horizon-agentd: skipping resume of {session_id:?} (already terminated)");
            continue;
        }

        if frame.is_turn_in_flight() {
            // Mirrors what a live `Command::Cancel` does (`providers::rig::
            // session`, `providers::mock`): finish every still-outstanding
            // tool call as cancelled *before* the turn-end/state-change
            // pair, so e.g. a call parked in `WaitingForApproval` doesn't
            // keep reading as pending in the resumed frame -- there is no
            // live provider left to eventually answer it.
            let mut closing: Vec<Event> = outstanding_tool_call_ids(&frame)
                .into_iter()
                .map(|call_id| Event::ToolCallFinished(cancelled_tool_call_result(call_id)))
                .collect();
            closing.push(Event::TurnEnded(TurnEndReason::Cancelled));
            closing.push(Event::StateChanged(SessionState::WaitingForUser));

            let mut appender = Appender::new(writer.clone(), session_id, Some(provider_id.clone()));
            match appender
                .append_provider_events(closing.iter().cloned().map(ProviderEvent::from).collect())
            {
                Ok(()) => events.extend(closing),
                Err(error) => eprintln!(
                    "horizon-agentd: failed to commit interrupted turn as cancelled for \
                     {session_id:?}: {error}"
                ),
            }
        }

        eprintln!(
            "horizon-agentd: resumed session {session_id:?} ({} event(s))",
            events.len()
        );
        spawn_session_thread(state.clone(), session_id, provider_id, events);
    }
}

/// Whether `frame`'s folded state shows its session already dead: either
/// `SessionState::Terminated` (the state `rig`'s `Command::Shutdown` path
/// sends -- see `providers::rig::session`) or an `Event::Exited` item (the
/// mock provider's shutdown path, `providers::mock`, pairs this with
/// `Terminated`; checked independently here in case a future provider ever
/// sends one without the other). Used by [`resume_persisted_sessions`] to
/// decide which sessions are worth spawning a thread for at all.
fn session_is_dead(frame: &AgentFrame) -> bool {
    matches!(frame.state, Some(SessionState::Terminated))
        || frame
            .items
            .iter()
            .any(|item| matches!(item, AgentFrameItem::Exited(_)))
}

/// Every `ToolCallRequested` call id in `frame` that has no matching
/// `ToolCallFinished` yet — i.e. genuinely still outstanding, whether it was
/// waiting on approval, waiting on Horizon to run it, or already running.
/// Used by [`resume_persisted_sessions`] to decide which calls need a
/// synthetic cancelled result when their turn is committed as cancelled.
fn outstanding_tool_call_ids(frame: &AgentFrame) -> Vec<ToolCallId> {
    let mut outstanding = Vec::new();
    for item in &frame.items {
        match item {
            AgentFrameItem::ToolCallRequested(request) => {
                if !outstanding.contains(&request.call_id) {
                    outstanding.push(request.call_id.clone());
                }
            }
            AgentFrameItem::ToolCallFinished(result) => {
                outstanding.retain(|call_id| call_id != &result.call_id);
            }
            _ => {}
        }
    }
    outstanding
}

/// One connection's view onto the process-lifetime [`AgentdState`] — thin by
/// design (step 4): every map that used to live here moved to `AgentdState`
/// so sessions survive a reconnect, leaving `Connection` as just the `Arc`
/// handle plus the methods that make sense scoped to "the current
/// connection" (installing/clearing `outgoing`).
#[derive(Clone)]
pub(crate) struct Connection {
    state: Arc<AgentdState>,
}

impl Connection {
    /// Installs `outgoing` as the shared target every session thread sends
    /// through (see the module doc) — this is what makes a freshly accepted
    /// connection immediately start receiving events from sessions that
    /// were already running (resumed at startup, or left over from a prior
    /// connection on this same process).
    pub(crate) fn new(outgoing: UnboundedSender<Envelope>, state: Arc<AgentdState>) -> Self {
        *state.outgoing.lock().unwrap() = Some(outgoing);
        Self { state }
    }

    /// Clears the shared outgoing target on disconnect, so a session thread
    /// doesn't keep "successfully" enqueueing envelopes into a writer task
    /// that already gave up on a dead socket (see `main::
    /// run_session_hosting_loop`'s doc comment on the writer task's own
    /// lifetime).
    pub(crate) fn disconnect(&self) {
        *self.state.outgoing.lock().unwrap() = None;
    }

    /// Spawns the session thread for a `Control::SessionNew`. Reuses the
    /// crate's existing spawn shape (`ProviderRegistry::start_session`) --
    /// the same call `app::runtime::agent::spawn_agent_session` makes
    /// in-process.
    pub(crate) fn handle_session_new(&self, new: SessionNew) {
        spawn_session_thread(
            self.state.clone(),
            new.session_id,
            new.provider_id,
            Vec::new(),
        );
    }

    /// Routes a `Command` envelope scoped to `session_id` to that session's
    /// thread. A miss (unknown session id -- stale/mistargeted envelope) is
    /// logged and dropped rather than panicking.
    pub(crate) fn route_command(&self, session_id: SessionId, command: Command) {
        let sender = self
            .state
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
            .state
            .pending_host_tool_requests
            .lock()
            .unwrap()
            .remove(&response.request_id.0);
        if let Some(sender) = sender {
            let _ = sender.send(response);
        }
    }

    /// Delegates to [`AgentdState::wait_until_resume_ready`] -- see `main`'s
    /// bind-first startup fix: `Control::SessionList`/`Control::SessionLoad`
    /// must block on this before answering, so a client that connects while
    /// `resume_persisted_sessions` is still running doesn't see an
    /// incomplete (or, right after bind, empty) session list.
    pub(crate) async fn wait_until_resume_ready(&self) {
        self.state.wait_until_resume_ready().await;
    }

    /// Delegates to [`AgentdState::skipped_lines_summary`] -- see `main::
    /// run_session_hosting_loop`, which waits for [`Self::wait_until_resume_ready`]
    /// first so this always reflects the finished startup read.
    pub(crate) fn skipped_lines_summary(&self) -> Option<String> {
        self.state.skipped_lines_summary()
    }

    pub(crate) fn session_list(&self) -> Vec<SessionSummary> {
        self.state
            .sessions
            .lock()
            .unwrap()
            .iter()
            .map(|(session_id, entry)| SessionSummary {
                session_id: *session_id,
                provider_id: entry.provider_id.clone(),
            })
            .collect()
    }

    /// Handles `Control::SessionLoad`: asks `session_id`'s own thread (if
    /// live) to hand back everything its `LiveState::events()` has
    /// accumulated -- already-committed history plus anything folded in
    /// since -- so the caller (`main::run_session_hosting_loop`) can forward
    /// it to the requesting client as ordinary event envelopes. Per the
    /// design's "v1 bootstrap" note, this is exactly the events list, not a
    /// server-side frame snapshot (a later optimization). An unknown
    /// session id resolves to an empty list rather than an error -- nothing
    /// to replay.
    ///
    /// Runs the actual wait on a `spawn_blocking` thread rather than
    /// blocking this async call's caller directly, so a slow (or wedged)
    /// session thread can't stall this connection's envelope-reading loop
    /// for unrelated traffic.
    pub(crate) async fn replay_events(&self, session_id: SessionId) -> Vec<Event> {
        let replay_tx = self
            .state
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|entry| entry.replay.clone());
        let Some(replay_tx) = replay_tx else {
            return Vec::new();
        };

        tokio::task::spawn_blocking(move || {
            let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
            if replay_tx.send(reply_tx).is_err() {
                return Vec::new();
            }
            reply_rx.recv_timeout(REPLAY_TIMEOUT).unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }
}

/// Sends `envelope` through whichever connection currently owns `outgoing`,
/// silently dropping it if none does (no client to see it right now -- see
/// the module doc). Returns whether the send was actually attempted and
/// accepted by the channel, for the one caller ([`AgentdHostTools::
/// execute_auto`]) that needs to fail fast rather than wait out its full
/// timeout when nothing is listening.
fn send_envelope(outgoing: &SharedOutgoing, envelope: Envelope) -> bool {
    match outgoing.lock().unwrap().as_ref() {
        Some(tx) => tx.send(envelope).is_ok(),
        None => false,
    }
}

/// The agent (child) side of the host-tool channel (guardrail 4 in
/// `docs/agent-runtime-split-design.md`): sends a `host_tool_request` over
/// the current connection (if any -- see [`send_envelope`]) and blocks this
/// session's dedicated thread on the matching `host_tool_response`. Only
/// `workspace.snapshot` is ever routed here today (the same tool id
/// Horizon's own `agent::host_tools::WorkspaceHostTools` answers
/// in-process) -- everything else falls through to `None`, letting
/// `execute_agent_tool` try the crate's own `tools::fs` auto tools next.
struct AgentdHostTools {
    session_id: SessionId,
    state: Arc<AgentdState>,
}

impl HostTools for AgentdHostTools {
    fn execute_auto(&self, tool_id: &str, input: &serde_json::Value) -> Option<serde_json::Value> {
        if tool_id != "workspace.snapshot" {
            return None;
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        self.state
            .pending_host_tool_requests
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
        if !send_envelope(&self.state.outgoing, envelope) {
            self.state
                .pending_host_tool_requests
                .lock()
                .unwrap()
                .remove(&request_id);
            return None;
        }

        let response = reply_rx.recv_timeout(HOST_TOOL_TIMEOUT).ok();
        self.state
            .pending_host_tool_requests
            .lock()
            .unwrap()
            .remove(&request_id);
        response.map(|response| response.output)
    }
}

/// The session's whole lifetime, from `Initialize` through to the
/// provider's channel closing. Runs entirely synchronously on its own
/// dedicated thread -- see the module doc for why. Faithfully mirrors
/// `app::runtime::agent::spawn_agent_session`'s in-process shape, minus the
/// floem signals/effects: register the tool/live state (seeded with
/// `history`, see [`resume_persisted_sessions`]), send `Initialize`, then
/// fold every provider event / bash completion / inbound command / replay
/// request as it arrives, forwarding the resulting (non-ephemeral) events to
/// Horizon over the wire exactly as `LiveState::extend_provider_events`
/// folded them in-process.
fn run_session(
    session_id: SessionId,
    provider_id: ProviderId,
    state: &Arc<AgentdState>,
    inbound_rx: Receiver<Command>,
    replay_rx: Receiver<Sender<Vec<Event>>>,
    history: Vec<Event>,
) {
    let Some(handle) = state.providers.start_session(&provider_id, session_id) else {
        send_envelope(
            &state.outgoing,
            Envelope::event(
                session_id,
                Event::Error(AgentError {
                    message: format!("Unknown provider `{}`.", provider_id.0),
                }),
            ),
        );
        return;
    };

    let tool_state = ToolSessionState::for_current_dir(state.agent_config.tools);
    let live_state = match state.writer() {
        Some(writer) => LiveState::with_event_log_and_history(
            session_id,
            Some(provider_id.clone()),
            writer,
            history,
        ),
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
        state: state.clone(),
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
                    state,
                    &tool_state,
                    &live_state,
                    &commands_tx,
                    session_id,
                    provider_event,
                ),
                Err(_) => break,
            },
            recv(bash_results_rx) -> message => {
                if let Ok(completion) = message {
                    fold_bash_completion(state, &live_state, &commands_tx, session_id, completion);
                }
            },
            recv(inbound_rx) -> message => match message {
                Ok(command) => dispatch_inbound_command(
                    state,
                    &live_state,
                    &commands_tx,
                    session_id,
                    command,
                ),
                Err(_) => break,
            },
            recv(replay_rx) -> message => {
                if let Ok(reply_tx) = message {
                    let _ = reply_tx.send(live_state.events());
                }
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
/// forwarded to Horizon as event envelopes. Ephemeral tool-call progress
/// (`ProviderEvent::tool_call_progress`) is folded into the local frame (so
/// a later `resolve_approval`'s `frame.tool_call_request` lookup stays
/// correct) exactly like every other event, but forwarded as its own
/// `Control::ToolCallProgress` message rather than a `contract::Event` --
/// there's no `Event` variant for it (it's never part of conversation
/// history or the persisted log; see `ToolCallProgress`'s own doc comment),
/// so wrapping it in `Envelope::event` isn't an option. This restores the
/// streaming-tool-call-argument-preview feature the module's step 3 notes in
/// `docs/agent-runtime-split-design.md` recorded as trimmed for agentd mode.
/// `process_agent_provider_event` never mixes progress and real events in
/// one `Processing` (a progress tick always comes back alone), so splitting
/// `horizon_events` into the two forwarding shapes below is exhaustive in
/// practice, not just by construction.
fn handle_provider_event(
    host: &dyn HostTools,
    state: &Arc<AgentdState>,
    tool_state: &ToolSessionState,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    provider_event: ProviderEvent,
) {
    let processing = process_agent_provider_event(host, tool_state, provider_event);
    for command in processing.provider_commands {
        let _ = commands_tx.send(command);
    }

    let mut to_forward: Vec<Event> = Vec::new();
    let mut progress_envelopes: Vec<Envelope> = Vec::new();
    for event in &processing.horizon_events {
        match &event.tool_call_progress {
            Some(progress) => progress_envelopes.push(Envelope {
                v: horizon_agent::wire::CONTRACT_VERSION,
                session_id: Some(session_id),
                body: EnvelopeBody::Control(Control::ToolCallProgress(progress.clone())),
            }),
            None => to_forward.push(event.event.clone()),
        }
    }
    let _ = live_state.extend_provider_events(processing.horizon_events);
    for envelope in progress_envelopes {
        send_envelope(&state.outgoing, envelope);
    }
    for event in to_forward {
        send_envelope(&state.outgoing, Envelope::event(session_id, event));
    }
}

/// The async-execution analogue of [`handle_provider_event`]'s fold, for a
/// `bash` call approved earlier (`ApprovalOutcome::Started` below) whose
/// result has now arrived on its own channel -- mirrors `app/runtime/
/// agent.rs::fold_bash_completion` exactly, forwarding the same events over
/// the wire instead of updating a local `Frames` signal.
fn fold_bash_completion(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
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
        send_envelope(&state.outgoing, Envelope::event(session_id, event));
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
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    command: Command,
) {
    match command {
        Command::ApproveToolCall { call_id } => resolve_and_forward(
            state,
            live_state,
            commands_tx,
            session_id,
            call_id,
            ApprovalDecision::Approve,
        ),
        Command::DenyToolCall { call_id, reason } => resolve_and_forward(
            state,
            live_state,
            commands_tx,
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
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
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
                send_envelope(&state.outgoing, Envelope::event(session_id, event));
            }
            let _ = commands_tx.send(command);
        }
        ApprovalOutcome::Started { events, .. } => {
            for event in events {
                send_envelope(&state.outgoing, Envelope::event(session_id, event));
            }
        }
        ApprovalOutcome::Forward(command) => {
            let _ = commands_tx.send(command);
        }
        ApprovalOutcome::AlreadyResolved => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_agent::contract::Exit;

    /// A session whose log ends in `SessionState::Terminated` (the state
    /// `rig`'s `Command::Shutdown` path sends, with no accompanying
    /// `Event::Exited` -- see `providers::rig::session`) must be treated as
    /// dead.
    #[test]
    fn session_is_dead_when_the_frame_state_is_terminated() {
        let events = vec![
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::Terminated),
        ];
        let frame = agent_frame_from_events(&events);
        assert!(session_is_dead(&frame));
    }

    /// The mock provider's shutdown path sends `Event::Exited` right after
    /// `SessionState::Terminated`; either one alone must be enough to flag
    /// the session as dead, so this covers `Exited` being present without
    /// relying on the state check.
    #[test]
    fn session_is_dead_when_an_exited_event_is_present() {
        let events = vec![
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::Terminated),
            Event::Exited(Exit {
                reason: "shutdown".to_string(),
            }),
        ];
        let frame = agent_frame_from_events(&events);
        assert!(session_is_dead(&frame));
    }

    /// A session parked in an ordinary live state (here, waiting for the
    /// next user message) must not be flagged as dead -- this is the
    /// common case `resume_persisted_sessions` must keep resuming.
    #[test]
    fn session_is_not_dead_when_waiting_for_user() {
        let events = vec![
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::WaitingForUser),
        ];
        let frame = agent_frame_from_events(&events);
        assert!(!session_is_dead(&frame));
    }

    /// A session with a turn still genuinely in flight (e.g. parked on an
    /// approval, as a `kill -9` mid-turn would leave it) is not dead either
    /// -- `resume_persisted_sessions` handles that case by committing the
    /// interrupted turn as cancelled, not by refusing to resume it.
    #[test]
    fn session_is_not_dead_when_a_turn_is_in_flight() {
        let events = vec![
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::WaitingForApproval),
        ];
        let frame = agent_frame_from_events(&events);
        assert!(!session_is_dead(&frame));
    }
}
