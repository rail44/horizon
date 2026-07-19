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
//! [`SessiondHostTools::execute_auto`]): the session thread genuinely blocks
//! on a channel recv while Horizon answers over the wire, which would
//! deadlock a single-threaded async runtime but is harmless on its own
//! dedicated thread.
//!
//! **Sessions are scoped to the process, not the connection (step 4).**
//! `SessiondState::sessions`/`pending_host_tool_requests`/`outgoing` are
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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};

use horizon_agent::config::{AgentConfig, AgentToolsConfig};
use horizon_agent::contract::{
    self, ApprovalRequest, Command, Error as AgentError, Event, Initialization, ProviderEvent,
    ProviderId, ProviderRegistry, SessionId, SessionState, ToolCallId, ToolCallResult,
    TurnEndReason,
};
use horizon_agent::frame::{agent_frame_from_events, AgentFrame, AgentFrameItem};
use horizon_agent::live::LiveState;
use horizon_agent::persistence::event_log::{Appender, Record, WriterHandle};
use horizon_agent::persistence::projection::duckdb::{DuckdbStoreHandle, SharedDuckdbStore};
use horizon_agent::roles::RoleId;
use horizon_agent::skills::SkillRegistry;
use horizon_agent::tools::{
    cancelled_tool_call_result, process_agent_provider_event, register_session_runtime,
    resolve_approval, should_fold_completion, unregister_session_runtime, ApprovalDecision,
    ApprovalOutcome, BashCompletion, HostTools, RecallContext, ToolSessionState,
};
use horizon_agent::wire::{
    Control, Envelope, EnvelopeBody, HostToolRequest, HostToolResponse, SessionNew, SessionSummary,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Notify;

use crate::worktree::{self, WorktreeInfo};

/// How long a session thread waits for Horizon to answer a `host_tool_*`
/// round trip before giving up. Generous but finite: a client that never
/// answers must not hang a session forever.
const HOST_TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// How long [`Connection::replay_events`] waits for a live session's own
/// thread to answer a replay request. **Not** purely a local channel hop:
/// a just-resumed session's thread does real work before it ever reaches
/// the loop that drains the `replay` channel, including blocking on
/// [`SessiondState::wait_for_duckdb_store`] -- which is deliberately *not*
/// ordered against [`SessiondState::mark_resume_ready`] (`Control::
/// SessionList`/`SessionLoad`'s own readiness gate), so a client can see a
/// resumed session as "listed" before its thread has gotten anywhere near
/// this channel. Under real contention (many sessiond processes competing
/// for CPU/disk, e.g. the full workspace test suite running in parallel)
/// that DuckDB rebuild-or-open wait can genuinely take several seconds,
/// and a timeout here has no way to distinguish "thread not there yet"
/// from "session truly has no history" -- it silently falls back to an
/// empty `Vec` either way (see the call site). A production `session_load`
/// racing this hard would misreport a real session as empty, so this is
/// sized generously to make that misfire vanishingly rare while still
/// bounding a genuinely wedged session thread. (Originally 5s -- too tight
/// under load, see `docs/tasks/backlog.md` #27. This crate's e2e tests
/// independently hit a comparable real-PTY stall past 60s under a
/// deliberately extreme concurrent `cargo build --release` loop during that
/// fix's own validation -- see `TERMINAL_UPDATE_TIMEOUT`'s doc comment in
/// `tests/e2e.rs` -- so this is sized with the same margin.)
const REPLAY_TIMEOUT: Duration = Duration::from_secs(120);

/// The connection-swappable outgoing envelope queue every session thread
/// sends through — see the module doc's "sessions are scoped to the
/// process" note.
type SharedOutgoing = Mutex<Option<UnboundedSender<Envelope>>>;

/// Process-lifetime state, built once in `main` and shared (via `Arc`) by
/// every connection `horizon-sessiond` ever serves, and by every session
/// thread regardless of which (if any) connection is currently live.
pub(crate) struct SessiondState {
    pub(crate) providers: ProviderRegistry,
    pub(crate) agent_config: AgentConfig,
    /// `None` until [`Self::set_writer`] runs (or forever, if the event log
    /// couldn't be opened -- sessions still run, just without persistence,
    /// the same graceful degrade the deleted in-process agent runtime had).
    /// A `Mutex` rather than a plain field
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
    /// Shared, multi-reader-blocking handle onto the live DuckDB projection
    /// (see [`SharedDuckdbStore`]'s doc comment) -- the *same* instance
    /// `main` also hands to the rig provider, so both consumers observe the
    /// event-log writer thread's one rebuild-or-open decision. `run_session`
    /// blocks on [`Self::wait_for_duckdb_store`] (never `main`'s accept loop
    /// or the readiness gate above) to populate a spawned session's
    /// `RecallContext`.
    duckdb_cell: SharedDuckdbStore,
    /// Horizon's single config file's host-resolved path (`main`'s
    /// `horizon_config::resolved_path()` call), injected into every
    /// spawned session's `ToolSessionState` (see `run_session`) for the
    /// `config.read`/`config.write` agent tools -- see
    /// `horizon_agent::tools::state::ToolSessionState::config_path`'s doc
    /// comment for the full seam. `None` means the same thing it means for
    /// `horizon_config::resolved_path`: no `HOME`/`XDG_CONFIG_HOME` to fall
    /// back to.
    config_path: Option<PathBuf>,
}

impl SessiondState {
    pub(crate) fn new(
        providers: ProviderRegistry,
        agent_config: AgentConfig,
        writer: Option<WriterHandle>,
        duckdb_cell: SharedDuckdbStore,
        config_path: Option<PathBuf>,
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
            duckdb_cell,
            config_path,
        }
    }

    pub(crate) fn writer(&self) -> Option<WriterHandle> {
        self.writer.lock().unwrap().clone()
    }

    pub(crate) fn set_writer(&self, writer: Option<WriterHandle>) {
        *self.writer.lock().unwrap() = writer;
    }

    /// Blocks the calling (dedicated session, per the module doc) OS
    /// thread until the event-log writer thread's own DuckDB rebuild-or-
    /// open decision has landed, then returns the shared handle (`None` if
    /// no DuckDB path was configured, or the rebuild/open failed). Never
    /// called from `main`'s accept loop or from anything gated on
    /// [`Self::wait_until_resume_ready`] -- this is a wholly separate wait,
    /// scoped to one session's own construction.
    pub(crate) fn wait_for_duckdb_store(&self) -> Option<DuckdbStoreHandle> {
        self.duckdb_cell.wait()
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

    /// `(directory, source_is_owned_worktree)` for `session_id`'s current
    /// state, if it's a session this process still hosts live -- the
    /// spawn-source lookup [`worktree::resolve_isolation_source`] needs.
    /// `None` for an unknown/foreign id (a terminal isn't tracked in this
    /// map at all -- deferred, see `worktree`'s module doc -- or the source
    /// session has already ended), which the caller treats as "no source",
    /// per that function's own doc comment.
    pub(crate) fn session_directory(&self, session_id: SessionId) -> Option<(PathBuf, bool)> {
        let sessions = self.sessions.lock().unwrap();
        let entry = sessions.get(&session_id)?;
        match &entry.worktree {
            Some(worktree) => Some((worktree.path.clone(), true)),
            None => entry.workspace_root.clone().map(|root| (root, false)),
        }
    }

    /// Records the outcome of a successful [`worktree::create_isolated_worktree`]
    /// call on `session_id`'s own `SessionEntry`: decision 2's derivation
    /// edge (`parent_session_id`, only when there actually was a spawn
    /// source -- an isolated-but-sourceless spawn is still a valid lineage
    /// root that merely owns a worktree) plus the resolved directory a
    /// later child spawned *from this session* would see via
    /// [`Self::session_directory`]. A no-op if `session_id` somehow isn't
    /// in `sessions` any more (the session ended before its own worktree
    /// creation finished) -- nothing left to record onto.
    pub(crate) fn record_isolated_worktree(
        &self,
        session_id: SessionId,
        parent_session_id: Option<SessionId>,
        worktree: WorktreeInfo,
    ) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(entry) = sessions.get_mut(&session_id) {
            entry.workspace_root = Some(worktree.path.clone());
            entry.parent_session_id = parent_session_id;
            entry.worktree = Some(worktree);
        }
    }
}

struct SessionEntry {
    provider_id: ProviderId,
    /// Mirrors `provider_id` -- surfaced in `session_list` summaries
    /// ([`Connection::session_list`]) the same way.
    role_id: Option<RoleId>,
    /// This session's resolved model id, computed once at spawn time (see
    /// [`spawn_session_thread`]) via [`ProviderRegistry::resolved_model`] --
    /// the same role-adjusted resolution `run_session`'s own
    /// `providers.start_session` call performs, just without waiting on it.
    /// Retained for the whole session lifetime so a later `session_load`
    /// (`Connection::session_model`) can re-announce it to a (re)attaching
    /// client -- see `docs/agent-output-ui-amendment.md`'s dated model-chip
    /// addendum.
    model: Option<String>,
    inbound: Sender<Command>,
    /// Answers a `session_load` for this session: the session's own thread
    /// receives a one-shot reply channel here and sends back everything its
    /// `LiveState::events()` has accumulated — see
    /// [`Connection::replay_events`].
    replay: Sender<Sender<Vec<Event>>>,
    /// The session this one derives from -- `Some` only when this session
    /// was actually spawned isolated (see [`SessiondState::
    /// record_isolated_worktree`]); `docs/session-relationship-design.md`
    /// decision 2's "the edge exists only via isolation". Surfaced
    /// additively as `SessionSummary.parent_session_id` by
    /// [`Connection::session_list`].
    parent_session_id: Option<SessionId>,
    /// The directory this session's file tools are actually confined to --
    /// its own worktree path if `worktree.is_some()`, else whatever
    /// `SessionNew.workspace_root` carried. Read by [`SessiondState::
    /// session_directory`] so a *child* spawned from this session knows
    /// where to branch its own worktree from.
    workspace_root: Option<PathBuf>,
    /// This session's own isolated worktree, if [`worktree::
    /// create_isolated_worktree`] succeeded for it -- `None` for an
    /// ordinary shared-directory session. Removed (if clean) when the
    /// session ends, see [`spawn_session_thread`]'s thread body.
    worktree: Option<WorktreeInfo>,
}

/// Resolves this session's model (pure and synchronous -- see
/// `Provider::resolved_model`'s doc comment) and, if resolvable, announces
/// it live to whichever client is connected right now, if any. Pulled out
/// of [`spawn_session_thread`] as its own function purely so this
/// resolve-then-maybe-send step is unit-testable without spinning up a
/// whole session thread -- same reason [`tool_session_state_for`] was.
///
/// A fresh `Control::SessionNew` caller is already listening
/// (`SessiondHandle::start_session` registers the session's route before
/// sending `SessionNew`), so it sees this immediately; a resumed session
/// spawned at daemon startup usually has no connection yet
/// ([`send_envelope`] silently drops it then) -- [`Connection::session_model`]
/// re-announces the same value for that case, from `Control::SessionLoad`'s
/// handler. See `docs/agent-output-ui-amendment.md`'s dated model-chip
/// addendum.
fn resolve_and_announce_session_model(
    state: &Arc<SessiondState>,
    session_id: SessionId,
    provider_id: &ProviderId,
    role_id: Option<&RoleId>,
) -> Option<String> {
    let model = state.providers.resolved_model(provider_id, role_id);
    if let Some(model) = &model {
        send_envelope(
            &state.outgoing,
            Envelope {
                v: horizon_agent::wire::CONTRACT_VERSION,
                session_id: Some(session_id),
                body: EnvelopeBody::Control(Control::SessionModel(model.clone())),
            },
        );
    }
    model
}

/// Spawns the dedicated thread for one session — the shared spawn path for
/// both a fresh `Control::SessionNew` ([`Connection::handle_session_new`])
/// and a session resumed from the persisted log at startup
/// ([`resume_persisted_sessions`]); `history` is empty for the former,
/// already-committed events for the latter. `workspace_root` is `Some` only
/// from a fresh `SessionNew` that carried one — resumed sessions don't
/// persist it (out of scope here), so they always pass `None` and fall back
/// to `run_session`'s process-cwd default, same as before this field
/// existed. `spawn_source_session_id`/`isolate` are resumed sessions'
/// `None`/`false` too, for the same reason: lineage lives in this process's
/// in-memory `SessiondState` (see `SessionEntry::parent_session_id`'s doc
/// comment), not the event log, so it doesn't survive a `horizon-sessiond`
/// restart either.
#[allow(clippy::too_many_arguments)]
fn spawn_session_thread(
    state: Arc<SessiondState>,
    session_id: SessionId,
    provider_id: ProviderId,
    role_id: Option<RoleId>,
    workspace_root: Option<PathBuf>,
    spawn_source_session_id: Option<SessionId>,
    isolate: bool,
    history: Vec<Event>,
) {
    let (inbound_tx, inbound_rx) = unbounded::<Command>();
    let (replay_tx, replay_rx) = unbounded::<Sender<Vec<Event>>>();
    let model =
        resolve_and_announce_session_model(&state, session_id, &provider_id, role_id.as_ref());
    state.sessions.lock().unwrap().insert(
        session_id,
        SessionEntry {
            provider_id: provider_id.clone(),
            role_id: role_id.clone(),
            model,
            inbound: inbound_tx,
            replay: replay_tx,
            parent_session_id: None,
            workspace_root: workspace_root.clone(),
            worktree: None,
        },
    );

    let thread_state = state.clone();
    thread::spawn(move || {
        run_session(
            session_id,
            provider_id,
            role_id,
            workspace_root,
            spawn_source_session_id,
            isolate,
            &thread_state,
            inbound_rx,
            replay_rx,
            history,
        );
        // Decision 5: a session that owned an isolated worktree gets it
        // cleaned up (if clean) exactly when its own thread ends -- which
        // only happens on a genuine `Command::Shutdown`/provider exit (the
        // daemon-side "terminate" signal), never on a mere close/detach
        // (those leave the thread, and this session, running -- see the
        // module doc's "sessions are scoped to the process" note).
        let entry = thread_state.sessions.lock().unwrap().remove(&session_id);
        if let Some(worktree) = entry.and_then(|entry| entry.worktree) {
            if !worktree::remove_worktree_if_clean(&worktree) {
                eprintln!(
                    "horizon-sessiond: kept worktree {} for {session_id:?} (not clean)",
                    worktree.path.display()
                );
            }
        }
    });
}

/// `docs/agent-runtime-split-design.md` step 4, "sessiond start": reads the
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
/// comes back as a ghost" before this filter existed. How many sessions hit
/// this skip is counted and reported as one combined summary line after the
/// loop, not printed per session -- a real archived log can carry dozens of
/// long-dead sessions, which used to bury the "resumed session" lines for
/// the ones that actually matter.
pub(crate) fn resume_persisted_sessions(state: &Arc<SessiondState>, records: Vec<Record>) {
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

    // Counted rather than printed per session (see the loop below): a real
    // archived log can carry dozens of long-dead sessions, and a line per
    // one drowned out the genuinely interesting "resumed session" lines
    // right next to it.
    let mut skipped_terminated = 0usize;

    for (session_id, mut session_records) in by_session {
        session_records.sort_by_key(|record| record.sequence);
        let provider_id = session_records
            .iter()
            .rev()
            .find_map(|record| record.provider_id.clone())
            .unwrap_or_else(|| state.providers.default_provider_id());
        // Mirrors `provider_id` just above: every record `Appender` writes
        // for a session carries the same `role_id` (see
        // `event_log::Appender::new`), so the last one found scanning from
        // the tail is the session's role for its whole lifetime.
        let role_id = session_records
            .iter()
            .rev()
            .find_map(|record| record.role_id.clone());
        let mut events: Vec<Event> = session_records
            .into_iter()
            .map(|record| record.event)
            .collect();

        let frame = agent_frame_from_events(&events);
        if session_is_dead(&frame) {
            skipped_terminated += 1;
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

            let mut appender = Appender::new(
                writer.clone(),
                session_id,
                Some(provider_id.clone()),
                role_id.clone(),
            );
            match appender
                .append_provider_events(closing.iter().cloned().map(ProviderEvent::from).collect())
            {
                Ok(()) => events.extend(closing),
                Err(error) => eprintln!(
                    "horizon-sessiond: failed to commit interrupted turn as cancelled for \
                     {session_id:?}: {error}"
                ),
            }
        }

        eprintln!(
            "horizon-sessiond: resumed session {session_id:?} ({} event(s))",
            events.len()
        );
        spawn_session_thread(
            state.clone(),
            session_id,
            provider_id,
            role_id,
            None,
            None,
            false,
            events,
        );
    }

    if skipped_terminated > 0 {
        eprintln!(
            "horizon-sessiond: skipped resume of {skipped_terminated} already-terminated \
             session(s)"
        );
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
            AgentFrameItem::ToolCallRequested(request)
                if !outstanding.contains(&request.call_id) =>
            {
                outstanding.push(request.call_id.clone());
            }
            AgentFrameItem::ToolCallFinished(result) => {
                outstanding.retain(|call_id| call_id != &result.call_id);
            }
            _ => {}
        }
    }
    outstanding
}

/// One connection's view onto the process-lifetime [`SessiondState`] — thin by
/// design (step 4): every map that used to live here moved to `SessiondState`
/// so sessions survive a reconnect, leaving `Connection` as just the `Arc`
/// handle plus the methods that make sense scoped to "the current
/// connection" (installing/clearing `outgoing`).
#[derive(Clone)]
pub(crate) struct Connection {
    state: Arc<SessiondState>,
}

impl Connection {
    /// Installs `outgoing` as the shared target every session thread sends
    /// through (see the module doc) — this is what makes a freshly accepted
    /// connection immediately start receiving events from sessions that
    /// were already running (resumed at startup, or left over from a prior
    /// connection on this same process).
    pub(crate) fn new(outgoing: UnboundedSender<Envelope>, state: Arc<SessiondState>) -> Self {
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
    /// the same call the deleted in-process agent runtime used to make
    /// before every agent session moved here.
    pub(crate) fn handle_session_new(&self, new: SessionNew) {
        spawn_session_thread(
            self.state.clone(),
            new.session_id,
            new.provider_id,
            new.role_id,
            new.workspace_root,
            new.spawn_source_session_id,
            new.isolate,
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
            None => eprintln!("horizon-sessiond: command for unknown session {session_id:?}"),
        }
    }

    /// Routes an incoming `Control::HostToolResponse` back to whichever
    /// session thread's [`SessiondHostTools::execute_auto`] call is blocked
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

    /// Delegates to [`SessiondState::wait_until_resume_ready`] -- see `main`'s
    /// bind-first startup fix: `Control::SessionList`/`Control::SessionLoad`
    /// must block on this before answering, so a client that connects while
    /// `resume_persisted_sessions` is still running doesn't see an
    /// incomplete (or, right after bind, empty) session list.
    pub(crate) async fn wait_until_resume_ready(&self) {
        self.state.wait_until_resume_ready().await;
    }

    /// Delegates to [`SessiondState::skipped_lines_summary`] -- see `main::
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
                role_id: entry.role_id.clone(),
                parent_session_id: entry.parent_session_id,
                workspace_root: entry.workspace_root.clone(),
            })
            .collect()
    }

    /// This session's resolved model id, if any -- see [`SessionEntry::model`]'s
    /// doc comment. `None` for an unknown `session_id` too (a stale/racing
    /// `session_load`), same "nothing to report" shape [`Self::session_list`]
    /// uses for a missing entry.
    pub(crate) fn session_model(&self, session_id: SessionId) -> Option<String> {
        self.state
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .and_then(|entry| entry.model.clone())
    }

    /// Delegates to [`SessiondState::writer`] -- `main`'s `Control::Drain`
    /// handling uses this to flush the event log's writer channel to disk
    /// before the process exits. An `append` returning only means a record
    /// was *enqueued*; the writer's background thread is what actually
    /// writes and flushes it (see `WriterHandle::open`'s "Ordering
    /// guarantee" doc comment), and forwarding an event to this connection
    /// over the wire happens after that same enqueue, not after it's
    /// durable. Without this, a client that drains right after observing a
    /// session's latest event over the wire could still race the writer and
    /// lose it -- unlike a `kill -9`, a graceful drain has no excuse to ever
    /// do that.
    pub(crate) fn writer(&self) -> Option<WriterHandle> {
        self.state.writer()
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
/// accepted by the channel, for the one caller ([`SessiondHostTools::
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
struct SessiondHostTools {
    session_id: SessionId,
    state: Arc<SessiondState>,
}

impl HostTools for SessiondHostTools {
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

/// Builds a session's file-tool confinement root (`tools::state::
/// ToolSessionState::workspace_root`): an explicit `workspace_root` --
/// carried by a fresh `wire::SessionNew`, when the caller supplied one --
/// takes precedence over `ToolSessionState::for_current_dir`'s default of
/// this process's own cwd (the only behavior before this field existed, and
/// still the only behavior for a resumed session, which never has one --
/// see [`spawn_session_thread`]'s doc comment). Pulled out of
/// [`run_session`] as its own function purely so this Some/None dispatch is
/// unit-testable without spinning up a whole session thread.
fn tool_session_state_for(
    workspace_root: Option<PathBuf>,
    tools: AgentToolsConfig,
    recall: RecallContext,
) -> ToolSessionState {
    match workspace_root {
        Some(root) => ToolSessionState::for_root(root, tools, recall),
        None => ToolSessionState::for_current_dir(tools, recall),
    }
}

/// Resolves and creates this session's isolated worktree (`docs/
/// session-relationship-design.md` decisions 2-3), returning the directory
/// its file tools should actually be confined to, plus whether isolation
/// actually succeeded -- the latter is what `ToolSessionState::
/// with_isolated_worktree` needs (`docs/agent-approval-design.md`'s tier 1:
/// the per-call trust predicate's isolation input must reflect the real
/// outcome, never merely the request). Runs on the session's own dedicated
/// thread, before `tool_session_state_for` -- a few tens of milliseconds of
/// blocking `git` subprocess calls at session-start time, the same shape
/// `state.wait_for_duckdb_store()` just above already accepts for this
/// thread. Degrades gracefully on any failure (no git repo found, no
/// commits yet, ...): falls back to `workspace_root` (today's
/// shared-directory behavior) and records no lineage edge, since isolation
/// didn't actually happen -- matching decision 2's "the edge exists only
/// via isolation" for the *actual* outcome, not merely the request. A
/// `contract::Event::Error` is also emitted so the failure is visible in
/// the session's own transcript rather than only sessiond's stderr.
fn resolve_and_create_isolated_worktree(
    state: &Arc<SessiondState>,
    session_id: SessionId,
    spawn_source_session_id: Option<SessionId>,
    workspace_root: Option<PathBuf>,
) -> (Option<PathBuf>, bool) {
    let fallback_dir = workspace_root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    let parent_info =
        spawn_source_session_id.and_then(|source_id| state.session_directory(source_id));
    let (source_dir, source_is_owned_worktree) =
        worktree::resolve_isolation_source(parent_info, fallback_dir);

    match worktree::create_isolated_worktree(
        &source_dir,
        source_is_owned_worktree,
        session_id.as_uuid(),
    ) {
        Ok(info) => {
            let root = info.path.clone();
            state.record_isolated_worktree(session_id, spawn_source_session_id, info);
            (Some(root), true)
        }
        Err(error) => {
            eprintln!(
                "horizon-sessiond: failed to create isolated worktree for {session_id:?}: {error}"
            );
            send_envelope(
                &state.outgoing,
                Envelope::event(
                    session_id,
                    Event::Error(AgentError {
                        message: format!(
                            "failed to create an isolated worktree ({error}); continuing without \
                             isolation"
                        ),
                    }),
                ),
            );
            (workspace_root, false)
        }
    }
}

/// The session's whole lifetime, from `Initialize` through to the
/// provider's channel closing. Runs entirely synchronously on its own
/// dedicated thread -- see the module doc for why. Faithfully mirrors the
/// deleted in-process agent runtime's shape, minus the floem signals/
/// effects it used to fold through: register the tool/live state (seeded with
/// `history`, see [`resume_persisted_sessions`]), send `Initialize`, then
/// fold every provider event / bash completion / inbound command / replay
/// request as it arrives, forwarding the resulting (non-ephemeral) events to
/// Horizon over the wire exactly as `LiveState::extend_provider_events`
/// folded them in-process.
#[allow(clippy::too_many_arguments)]
fn run_session(
    session_id: SessionId,
    provider_id: ProviderId,
    role_id: Option<RoleId>,
    workspace_root: Option<PathBuf>,
    spawn_source_session_id: Option<SessionId>,
    isolate: bool,
    state: &Arc<SessiondState>,
    inbound_rx: Receiver<Command>,
    replay_rx: Receiver<Sender<Vec<Event>>>,
    history: Vec<Event>,
) {
    let Some(handle) = state
        .providers
        .start_session(&provider_id, session_id, role_id.clone())
    else {
        // `ProviderRegistry::start_session` returns `None` for either an
        // unknown `provider_id` or an unresolvable `role_id` (see its own
        // doc comment on why role validation is centralized there) -- this
        // is `roles`'s "never silently degrade to role-less" requirement's
        // one production enforcement point, so the message distinguishes
        // which one actually failed rather than defaulting to a generic
        // "unknown provider" that would be misleading for a bad role.
        let message = match &role_id {
            Some(role_id) if horizon_agent::roles::resolve(role_id).is_none() => {
                format!("Unknown role `{}`.", role_id.0)
            }
            _ => format!("Unknown provider `{}`.", provider_id.0),
        };
        send_envelope(
            &state.outgoing,
            Envelope::event(session_id, Event::Error(AgentError { message })),
        );
        return;
    };

    // Blocks this session's own dedicated thread (never `main`'s accept
    // loop, and never the readiness gate `session_list`/`session_new`
    // block on) until the event-log writer thread's own DuckDB
    // rebuild-or-open decision has landed -- see `SessiondState::
    // wait_for_duckdb_store`'s doc comment.
    let recall = RecallContext {
        session_id: Some(session_id),
        store: state.wait_for_duckdb_store(),
    };
    // Discovered from this process's own cwd, same as `providers::rig::
    // session::session_extra_sections` independently does for the prompt's
    // skills section -- both are cheap, session-start-once listings of the
    // same on-disk state, so there's no shared value worth threading
    // between these two otherwise-unconnected session-start sites (this
    // thread's `ToolSessionState`/tool execution vs. the rig provider's own
    // dedicated thread and its system prompt).
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
    // Skill discovery above always uses the process cwd regardless of
    // `workspace_root` -- see the comment just above `cwd`'s definition on
    // why that's an intentionally separate, unthreaded value.
    let (workspace_root, isolated) = if isolate {
        resolve_and_create_isolated_worktree(
            state,
            session_id,
            spawn_source_session_id,
            workspace_root,
        )
    } else {
        (workspace_root, false)
    };
    let tool_state = tool_session_state_for(workspace_root, state.agent_config.tools, recall)
        .with_isolated_worktree(isolated)
        .with_skills(SkillRegistry::discover(&cwd))
        .with_config_path(state.config_path.clone());
    let live_state = match state.writer() {
        Some(writer) => LiveState::with_event_log_and_history(
            session_id,
            Some(provider_id.clone()),
            role_id.clone(),
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

    let host = SessiondHostTools {
        session_id,
        state: state.clone(),
    };

    let commands_tx = handle.sender();
    let _ = commands_tx.send(Command::Initialize(Initialization {
        session_id,
        provider_id: provider_id.clone(),
        role_id,
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

/// One provider event through the same processing pipeline the deleted
/// in-process agent runtime's effect used to run
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
/// `docs/agent-runtime-split-design.md` recorded as trimmed for sessiond mode.
/// `process_agent_provider_event` never mixes progress and real events in
/// one `Processing` (a progress tick always comes back alone), so splitting
/// `horizon_events` into the two forwarding shapes below is exhaustive in
/// practice, not just by construction.
fn handle_provider_event(
    host: &dyn HostTools,
    state: &Arc<SessiondState>,
    tool_state: &ToolSessionState,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    provider_event: ProviderEvent,
) {
    let processing = process_agent_provider_event(host, tool_state, session_id, provider_event);
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
/// result has now arrived on its own channel -- the same shape the deleted
/// in-process agent runtime's `fold_bash_completion` used to have,
/// forwarding the same events over the wire instead of updating a local
/// `Frames` signal, except the trailing `StateChanged` is no longer
/// unconditional (see below).
///
/// `bash` is the only tool whose completion arrives asynchronously like
/// this -- `fs.write`/`fs.edit`/`config.write` all resolve synchronously
/// inside `agent::tools::approval::resolve_synchronous_tool` (folded
/// straight into `dispatch_inbound_command`'s `resolve_and_forward`) -- so
/// this is the one place a completion can land after *other* tool-call
/// approvals from the same turn are still outstanding.
fn fold_bash_completion(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    completion: BashCompletion,
) {
    match completion {
        BashCompletion::Finished(result) => {
            fold_finished_bash_result(state, live_state, commands_tx, session_id, result)
        }
        BashCompletion::RetryWithoutSandbox { call_id, reason } => {
            fold_bash_retry_without_sandbox(state, live_state, session_id, call_id, reason)
        }
    }
}

/// The ordinary case: a bash call actually finished (successfully or not).
/// Unchanged behavior from before [`BashCompletion`] grew a second variant.
fn fold_finished_bash_result(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    result: ToolCallResult,
) {
    let frame = live_state.frame();
    if !should_fold_completion(&frame, &result.call_id) {
        return;
    }

    // Honest trailing state: a second approval-gated call from the same
    // turn (another `bash` approved earlier, or a sibling fs/config
    // request still awaiting a decision) can still be outstanding when
    // this one finishes -- reporting `WaitingForUser` then is exactly the
    // backlog #34 bug (status line blanks, stop button vanishes, while a
    // decision is still actionable). `actionable_pending_approval_call_ids`
    // (not the plain `pending_approval_call_ids`) is the right reader here
    // for the same reason it's the required one on every dispatch path
    // (see its doc comment): it excludes a *ghost* request whose own turn
    // already ended, which no live daemon-side gate can ever answer, so a
    // ghost alone must never hold the reported state at `WaitingForApproval`
    // forever. `result.call_id` itself is still in that list at this point
    // -- only a *folded* `ToolCallFinished` clears an id, and this call's
    // hasn't been folded yet -- so it's excluded explicitly rather than
    // re-reading the frame after folding.
    //
    // If nothing else is outstanding, `WaitingForUser` is still not
    // necessarily the last word: `commands_tx.send` below hands the result
    // to the provider, which may still be mid-turn (more model output
    // pending) and will emit its own `StateChanged` once it resumes --
    // exactly as it already could race this state before this fix. That's
    // the session loop's own turn-level state to own; this function only
    // has to stop lying about the one thing it *does* know here: whether an
    // approval is still actionable.
    let approval_still_pending = frame
        .actionable_pending_approval_call_ids()
        .into_iter()
        .any(|id| id != result.call_id);
    let trailing_state = if approval_still_pending {
        SessionState::WaitingForApproval
    } else {
        SessionState::WaitingForUser
    };

    let events = vec![
        Event::ToolCallFinished(result.clone()),
        Event::StateChanged(trailing_state),
    ];
    let _ = live_state.extend_provider_events(events.clone().into_iter().map(Into::into));
    for event in events {
        send_envelope(&state.outgoing, Envelope::event(session_id, event));
    }

    let _ = commands_tx.send(Command::ToolCallResult(result));
}

/// A sandboxed tier-1 attempt looked denied by the sandbox itself
/// (`horizon_sandbox::is_likely_sandbox_denied`) -- surface the normal
/// approval flow for a retry of the same call without the sandbox
/// (`docs/agent-approval-design.md`'s "Denial UX"), instead of reporting a
/// raw failure straight to the provider. Reissues a fresh `ToolCallRequested`
/// for the same `call_id` right before the `ApprovalRequested`: `AgentFrame::
/// has_tool_call_started`/`has_tool_call_finished` are scoped to items
/// *since the latest* `ToolCallRequested` occurrence for a call_id (see
/// their doc comments -- the same mechanism already supports a provider
/// reusing a call_id for a genuinely new call), so this moves that scope
/// boundary past the first (sandboxed) attempt's own `ToolCallStarted`;
/// without it, the user's eventual Approve of this retry would be
/// misclassified as `AlreadyResolved` by `tools::approval::try_execute`.
/// Not forwarded to the provider: the original call is still open from its
/// point of view, exactly as if it hadn't been auto-approved yet at all.
fn fold_bash_retry_without_sandbox(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    session_id: SessionId,
    call_id: ToolCallId,
    reason: String,
) {
    let frame = live_state.frame();
    if !should_fold_completion(&frame, &call_id) {
        return;
    }
    let Some(original_request) = frame.tool_call_request(&call_id).cloned() else {
        // Should be unreachable (this call_id was necessarily requested to
        // have gotten this far) -- nothing sane to reissue against.
        return;
    };

    let events = vec![
        Event::ToolCallRequested(original_request),
        Event::ApprovalRequested(ApprovalRequest { call_id, reason }),
        Event::StateChanged(SessionState::WaitingForApproval),
    ];
    let _ = live_state.extend_provider_events(events.clone().into_iter().map(Into::into));
    for event in events {
        send_envelope(&state.outgoing, Envelope::event(session_id, event));
    }
}

/// A `Command` envelope arriving from Horizon for this session.
/// `ApproveToolCall`/`DenyToolCall` are resolved right here (decision 2:
/// "Approval decisions stay in Horizon... resolved in sessiond") via
/// `tools::approval::resolve_approval`; everything else forwards straight
/// to the provider, unchanged. (An earlier in-process shell shared this
/// helper from its own click handler; that path retired with the
/// runtime split.)
fn dispatch_inbound_command(
    state: &Arc<SessiondState>,
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
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    call_id: ToolCallId,
    decision: ApprovalDecision,
) {
    // `resolve_approval` moves `call_id`; keep a copy so the
    // `AlreadyResolved` arm below can still name it in its log line.
    let logged_call_id = call_id.clone();
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
        // The pending -> resolved transition already happened for this
        // call_id (started or finished) -- see `ApprovalOutcome::
        // AlreadyResolved`'s doc comment. This is the guard that stops a
        // burst of duplicate `Approve`/`Deny` commands (the 2026-07
        // repeated-approval OOM incident) from re-executing anything: every
        // one after the first lands here and is dropped, logged rather than
        // silently swallowed so a runaway burst like that incident's is
        // visible in sessiond's own stderr.
        ApprovalOutcome::AlreadyResolved => {
            eprintln!(
                "horizon-sessiond: dropped duplicate approve/deny for session {session_id:?}, \
                 call {logged_call_id:?} (already resolved)"
            );
        }
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

    /// A `SessionNew.workspace_root` of `Some(dir)` must confine the
    /// session's file tools to `dir`, not this process's cwd.
    #[test]
    fn tool_session_state_for_uses_the_given_directory_when_some() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .expect("canonicalize temp dir");
        let state = tool_session_state_for(
            Some(dir.clone()),
            AgentToolsConfig::default(),
            RecallContext::default(),
        );
        assert_eq!(state.workspace_root(), Some(dir.as_path()));
    }

    /// `None` (today's only value Horizon actually sends -- see
    /// `wire::SessionNew::workspace_root`'s doc comment) must keep behaving
    /// exactly as before this field existed: confined to this process's own
    /// cwd.
    #[test]
    fn tool_session_state_for_falls_back_to_the_process_cwd_when_none() {
        let expected_root = std::env::current_dir()
            .and_then(|dir| dir.canonicalize())
            .expect("canonicalize process cwd");
        let state =
            tool_session_state_for(None, AgentToolsConfig::default(), RecallContext::default());
        assert_eq!(state.workspace_root(), Some(expected_root.as_path()));
    }

    fn drain_events(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(envelope) = rx.try_recv() {
            if let EnvelopeBody::Event(event) = envelope.body {
                events.push(event);
            }
        }
        events
    }

    /// Regression test for backlog #34: `SessionState::WaitingForUser`
    /// reported while a tool-call approval is still pending. Two `bash`
    /// calls are approval-gated in the same turn; only the first has been
    /// approved (its `ToolRunning`/`ToolCallStarted` pair already folded,
    /// mirroring `agent::tools::approval::resolve_bash`'s `Started` outcome)
    /// when its async completion reaches `fold_bash_completion`. The second
    /// call's `ApprovalRequested` is still unresolved at that point, so the
    /// trailing state this emits must be `WaitingForApproval`, not
    /// `WaitingForUser` -- exactly the dishonest-state bug the backlog item
    /// describes (status line blanks, stop button vanishes, while a
    /// decision is still actionable). Once the second call is also approved
    /// and finishes, the state must finally settle on `WaitingForUser`.
    #[test]
    fn fold_bash_completion_reports_waiting_for_approval_while_a_sibling_approval_is_pending() {
        use horizon_agent::contract::{ApprovalRequest, ToolCallResult};

        let agent_config = AgentConfig::from_env_and_provider(None, None);
        let state = Arc::new(SessiondState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
        ));
        let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();
        *state.outgoing.lock().unwrap() = Some(outgoing_tx);

        let live_state = LiveState::with_disabled_persistence();
        let session_id = SessionId::new();
        let call_a = ToolCallId("bash-a".to_string());
        let call_b = ToolCallId("bash-b".to_string());

        live_state.extend_provider_events(
            vec![
                Event::StateChanged(SessionState::WaitingForApproval),
                Event::ApprovalRequested(ApprovalRequest {
                    call_id: call_a.clone(),
                    reason: "bash".to_string(),
                }),
                Event::ApprovalRequested(ApprovalRequest {
                    call_id: call_b.clone(),
                    reason: "bash".to_string(),
                }),
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_a.clone()),
            ]
            .into_iter()
            .map(Into::into),
        );

        let (commands_tx, commands_rx) = unbounded::<Command>();

        fold_bash_completion(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            BashCompletion::Finished(ToolCallResult::new(
                call_a.clone(),
                serde_json::json!({ "exit_code": 0 }),
            )),
        );

        let forwarded = drain_events(&mut outgoing_rx);
        assert_eq!(
            forwarded.last(),
            Some(&Event::StateChanged(SessionState::WaitingForApproval)),
            "call_b's approval is still outstanding, so the reported state must \
             stay WaitingForApproval, got: {forwarded:?}"
        );
        assert!(matches!(
            commands_rx.try_recv(),
            Ok(Command::ToolCallResult(result)) if result.call_id == call_a
        ));

        // Approving `call_b` folds its own running pair the same way
        // `call_a`'s did, then its completion arrives too.
        live_state.extend_provider_events(
            vec![
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_b.clone()),
            ]
            .into_iter()
            .map(Into::into),
        );

        fold_bash_completion(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            BashCompletion::Finished(ToolCallResult::new(
                call_b.clone(),
                serde_json::json!({ "exit_code": 0 }),
            )),
        );

        let forwarded = drain_events(&mut outgoing_rx);
        assert_eq!(
            forwarded.last(),
            Some(&Event::StateChanged(SessionState::WaitingForUser)),
            "every approval is resolved now, so the reported state must finally \
             be WaitingForUser, got: {forwarded:?}"
        );
    }

    /// The denial-retry flow's session-loop half
    /// (`docs/agent-approval-design.md`'s "Denial UX"): a `BashCompletion::
    /// RetryWithoutSandbox` must fold a fresh `ToolCallRequested` +
    /// `ApprovalRequested` + `WaitingForApproval` for the same call_id --
    /// never a `ToolCallResult` to the provider, since the original call is
    /// still open from its point of view.
    #[test]
    fn fold_bash_completion_turns_a_sandbox_denial_into_a_fresh_approval_request() {
        let agent_config = AgentConfig::from_env_and_provider(None, None);
        let state = Arc::new(SessiondState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
        ));
        let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();
        *state.outgoing.lock().unwrap() = Some(outgoing_tx);

        let live_state = LiveState::with_disabled_persistence();
        let session_id = SessionId::new();
        let call_id = ToolCallId("bash-denied".to_string());

        live_state.extend_provider_events(
            vec![
                Event::ToolCallRequested(horizon_agent::contract::ToolCallRequest {
                    call_id: call_id.clone(),
                    tool_id: "bash".to_string(),
                    input: serde_json::json!({ "command": "echo hi" }),
                }),
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_id.clone()),
            ]
            .into_iter()
            .map(Into::into),
        );

        let (commands_tx, commands_rx) = unbounded::<Command>();

        fold_bash_completion(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            BashCompletion::RetryWithoutSandbox {
                call_id: call_id.clone(),
                reason: "looked sandbox-denied".to_string(),
            },
        );

        let forwarded = drain_events(&mut outgoing_rx);
        assert!(
            forwarded.iter().any(|event| matches!(
                event,
                Event::ToolCallRequested(request) if request.call_id == call_id
            )),
            "expected a reissued ToolCallRequested for the same call_id: {forwarded:?}"
        );
        assert!(
            forwarded.iter().any(|event| matches!(
                event,
                Event::ApprovalRequested(request) if request.call_id == call_id
            )),
            "expected a fresh ApprovalRequested: {forwarded:?}"
        );
        assert_eq!(
            forwarded.last(),
            Some(&Event::StateChanged(SessionState::WaitingForApproval))
        );
        assert!(
            commands_rx.try_recv().is_err(),
            "the original call is still open from the provider's point of view -- \
             nothing should be forwarded to it yet"
        );

        let frame = live_state.frame();
        assert!(
            !frame.has_tool_call_started(&call_id),
            "the reissue must move the started/finished scope boundary past the \
             first (sandboxed) attempt's own ToolCallStarted"
        );
    }

    /// Builds a hermetic [`SessiondState`] with an explicit, env-independent
    /// `RigAgentConfig` (never `AgentConfig::from_env_and_provider`'s real
    /// env vars -- a developer's own `OPENAI_API_KEY` must never leak into
    /// this test's expectations) and an installed `outgoing` channel to
    /// observe what gets sent.
    fn state_with_rig_config(
        openai_enabled: bool,
        model: &str,
    ) -> (
        Arc<SessiondState>,
        tokio::sync::mpsc::UnboundedReceiver<Envelope>,
    ) {
        let mut agent_config = AgentConfig::from_env_and_provider(None, None);
        agent_config.rig.openai_enabled = openai_enabled;
        agent_config.rig.model = model.to_string();
        let state = Arc::new(SessiondState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
        ));
        let (outgoing_tx, outgoing_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();
        *state.outgoing.lock().unwrap() = Some(outgoing_tx);
        (state, outgoing_rx)
    }

    /// A resolvable model (rig provider, `openai_enabled: true`) is both
    /// returned (for `SessionEntry::model`) and announced live as a
    /// session-scoped `Control::SessionModel`, matching how `role_id`
    /// already travels -- see `docs/agent-output-ui-amendment.md`'s dated
    /// model-chip addendum.
    #[test]
    fn resolve_and_announce_session_model_sends_and_returns_the_resolved_model() {
        let (state, mut outgoing_rx) = state_with_rig_config(true, "test-model");
        let session_id = SessionId::new();
        let provider_id = ProviderId("builtin.agent.rig".to_string());

        let model = resolve_and_announce_session_model(&state, session_id, &provider_id, None);

        assert_eq!(model.as_deref(), Some("test-model"));
        let sent = outgoing_rx
            .try_recv()
            .expect("a SessionModel control should have been sent");
        assert_eq!(sent.session_id, Some(session_id));
        assert!(
            matches!(
                &sent.body,
                EnvelopeBody::Control(Control::SessionModel(model)) if model == "test-model"
            ),
            "expected a session-scoped SessionModel control, got: {:?}",
            sent.body
        );
    }

    /// Deterministic fallback mode (no `OPENAI_API_KEY`, mirrored here via
    /// `openai_enabled: false`) never calls a real provider, so there is no
    /// honest model to report -- nothing must be sent, mirroring
    /// `Control::SkippedLines`'s "omitted entirely" convention.
    #[test]
    fn resolve_and_announce_session_model_sends_nothing_in_deterministic_fallback_mode() {
        let (state, mut outgoing_rx) = state_with_rig_config(false, "test-model");
        let session_id = SessionId::new();
        let provider_id = ProviderId("builtin.agent.rig".to_string());

        let model = resolve_and_announce_session_model(&state, session_id, &provider_id, None);

        assert_eq!(model, None);
        assert!(
            outgoing_rx.try_recv().is_err(),
            "nothing should be sent when there is no resolvable model"
        );
    }

    /// [`Connection::session_model`] answers from whatever
    /// [`resolve_and_announce_session_model`] stored on the session's
    /// `SessionEntry` -- the read side of the same "attach re-announces it"
    /// path `Control::SessionLoad`'s handler uses.
    #[test]
    fn connection_session_model_reads_the_stored_value_for_a_known_session_only() {
        let (state, _outgoing_rx) = state_with_rig_config(true, "test-model");
        let session_id = SessionId::new();
        let (inbound_tx, _inbound_rx) = unbounded::<Command>();
        let (replay_tx, _replay_rx) = unbounded::<Sender<Vec<Event>>>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: Some("stored-model".to_string()),
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: None,
                workspace_root: None,
                worktree: None,
            },
        );

        let connection = Connection {
            state: state.clone(),
        };
        assert_eq!(
            connection.session_model(session_id).as_deref(),
            Some("stored-model")
        );
        assert_eq!(connection.session_model(SessionId::new()), None);
    }

    /// An id sessiond has never hosted (or has already ended) reports no
    /// directory -- the "no source" case [`worktree::resolve_isolation_source`]
    /// treats as a lineage root, falling back to the spawn's own
    /// `workspace_root`.
    #[test]
    fn session_directory_is_none_for_an_unknown_session() {
        let (state, _outgoing_rx) = state_with_rig_config(true, "test-model");
        assert_eq!(state.session_directory(SessionId::new()), None);
    }

    /// A plain (non-isolated) session reports its own `workspace_root` and
    /// `false` (not an owned worktree) -- what a *child* spawned from it
    /// would branch fresh-from-origin against.
    #[test]
    fn session_directory_reports_the_plain_workspace_root_when_not_isolated() {
        let (state, _outgoing_rx) = state_with_rig_config(true, "test-model");
        let session_id = SessionId::new();
        let (inbound_tx, _inbound_rx) = unbounded::<Command>();
        let (replay_tx, _replay_rx) = unbounded::<Sender<Vec<Event>>>();
        let root = std::path::PathBuf::from("/tmp/plain-root");
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: None,
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: None,
                workspace_root: Some(root.clone()),
                worktree: None,
            },
        );

        assert_eq!(state.session_directory(session_id), Some((root, false)));
    }

    /// [`SessiondState::record_isolated_worktree`] updates the session's own
    /// entry so a later [`SessiondState::session_directory`] lookup (from a
    /// grandchild spawn) reports the worktree path and `true` (owned) --
    /// the multi-level chaining decision 3 asks for.
    #[test]
    fn record_isolated_worktree_makes_the_session_report_as_an_owned_worktree() {
        let (state, _outgoing_rx) = state_with_rig_config(true, "test-model");
        let session_id = SessionId::new();
        let parent_id = SessionId::new();
        let (inbound_tx, _inbound_rx) = unbounded::<Command>();
        let (replay_tx, _replay_rx) = unbounded::<Sender<Vec<Event>>>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: None,
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: None,
                workspace_root: Some(std::path::PathBuf::from("/tmp/pre-isolation")),
                worktree: None,
            },
        );

        let info = WorktreeInfo {
            repo_root: std::path::PathBuf::from("/tmp/repo"),
            path: std::path::PathBuf::from("/tmp/repo/.horizon/worktrees/abcd1234"),
            branch: "horizon/abcd1234".to_string(),
        };
        state.record_isolated_worktree(session_id, Some(parent_id), info.clone());

        assert_eq!(
            state.session_directory(session_id),
            Some((info.path.clone(), true))
        );
        let sessions = state.sessions.lock().unwrap();
        let entry = &sessions[&session_id];
        assert_eq!(entry.parent_session_id, Some(parent_id));
        assert_eq!(entry.worktree, Some(info));
    }

    /// [`Connection::session_list`] must report the authoritative,
    /// post-isolation `workspace_root` from the session's own `SessionEntry`
    /// -- the wire-level counterpart of the state-level assertion above,
    /// and the coordinator's requested regression guard: the workspace
    /// model on the Horizon side reads exactly this field to correct its
    /// own pre-spawn value (`WorkspaceShell::spawn_agent_resume`/
    /// `spawn_workspace_restore`).
    #[test]
    fn session_list_reports_the_entrys_workspace_root_and_parent() {
        let (state, _outgoing_rx) = state_with_rig_config(true, "test-model");
        let session_id = SessionId::new();
        let parent_id = SessionId::new();
        let (inbound_tx, _inbound_rx) = unbounded::<Command>();
        let (replay_tx, _replay_rx) = unbounded::<Sender<Vec<Event>>>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: None,
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: Some(parent_id),
                workspace_root: Some(std::path::PathBuf::from(
                    "/tmp/repo/.horizon/worktrees/abcd1234",
                )),
                worktree: None,
            },
        );

        let connection = Connection { state };
        let summaries = connection.session_list();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, session_id);
        assert_eq!(summaries[0].parent_session_id, Some(parent_id));
        assert_eq!(
            summaries[0].workspace_root,
            Some(std::path::PathBuf::from(
                "/tmp/repo/.horizon/worktrees/abcd1234"
            ))
        );
    }
}
