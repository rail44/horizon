//! One connection's view onto the process-lifetime session state: the
//! handlers behind the hub's session-scoped control calls.

use std::sync::Arc;
use std::time::Duration;

use horizon_agent::contract::{Command, Event, SessionId};
use horizon_agent::persistence::event_log::WriterHandle;
use horizon_agent::wire::{
    AgentWireEvent, HostToolRequest, HostToolResponse, SessionNew, SessionSummary,
};
use tokio::sync::mpsc::UnboundedSender;

use super::events::send_session_event;
use super::spawn::spawn_session_thread;
use super::state::{lock_unpoisoned, SessiondState};

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
    pub(crate) fn new(state: Arc<SessiondState>) -> Self {
        Self { state }
    }

    /// Installs the current connection's host-tool bridge (the local half
    /// behind `HubHello::host_tools`) — the connection-global counterpart
    /// of the per-attachment subscribers [`Self::subscribe_agent`] installs.
    pub(crate) fn connect_host_tools(&self, outgoing: UnboundedSender<HostToolRequest>) {
        *self.state.host_tools_outgoing.lock().unwrap() = Some(outgoing);
    }

    /// Clears the connection-global host-tool bridge on disconnect, so a
    /// session thread's `execute_auto` fails fast instead of enqueueing
    /// into a bridge whose pump already died with the connection. The
    /// per-session subscribers are deliberately *not* swept here: each
    /// attachment's bridge dies with its own pump, and
    /// [`send_session_event`] already drops an entry lazily on its first
    /// failed send (a fresh attach replaces it anyway).
    pub(crate) fn disconnect(&self) {
        *self.state.host_tools_outgoing.lock().unwrap() = None;
    }

    /// Subscribes an attachment to `session_id`'s wire events, replacing
    /// any previous attachment's subscription (one client connection at a
    /// time; a re-attach supersedes). Returns the local receiving half the
    /// hub pumps into the attachment's remote channel.
    pub(crate) fn subscribe_agent(
        &self,
        session_id: SessionId,
    ) -> tokio::sync::mpsc::UnboundedReceiver<AgentWireEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        lock_unpoisoned(&self.state.agent_subscribers).insert(session_id, tx);
        rx
    }

    /// Pushes a session-scoped wire event to the session's current
    /// subscriber, if any — the hub's own send path (replay, model
    /// re-announcement), same semantics as every session thread's sends.
    pub(crate) fn send_session_event(&self, session_id: SessionId, event: AgentWireEvent) {
        send_session_event(&self.state, session_id, event);
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
            None,
            Vec::new(),
        );
    }

    /// Routes a `Command` envelope scoped to `session_id` to that session's
    /// thread. A miss (unknown session id -- stale/mistargeted envelope) is
    /// logged and dropped rather than panicking.
    pub(crate) fn route_command(&self, session_id: SessionId, command: Command) {
        if !self.state.send_command(session_id, command) {
            eprintln!("horizon-sessiond: command for unknown session {session_id:?}");
        }
    }

    /// Routes an incoming `Control::HostToolResponse` back to whichever
    /// session thread's `host_tools::SessiondHostTools::execute_auto` call is blocked
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

    /// Every session a client may see. Exploration sessions
    /// (`docs/agent-explore-design.md` decision 3: "invisible to the UI")
    /// are withheld: they are never attached to a pane, they live only as
    /// long as the `agent.explore` call waiting on them, and offering one
    /// in the session manager's attach list would invite a user into a
    /// read-only session that is about to be terminated under them. They
    /// remain fully first-class in the event log and DuckDB projection,
    /// which is where their cost is actually measured.
    pub(crate) fn session_list(&self) -> Vec<SessionSummary> {
        self.state
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, entry)| {
                !entry
                    .role_id
                    .as_ref()
                    .is_some_and(horizon_agent::roles::is_exploration)
            })
            .map(|(session_id, entry)| SessionSummary {
                session_id: *session_id,
                provider_id: entry.provider_id.clone(),
                role_id: entry.role_id.clone(),
                parent_session_id: entry.parent_session_id,
                workspace_root: entry.workspace_root.clone(),
            })
            .collect()
    }

    /// This session's resolved model id, if any -- see [`super::state::SessionEntry::model`]'s
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::state::SessionEntry;
    use crate::session::test_support::{judge_test_state, state_with_rig_config};
    use crossbeam_channel::{unbounded, Sender};
    use horizon_agent::contract::ProviderId;
    use horizon_agent::roles::RoleId;

    /// Decision 3, "invisible to the UI": a live exploration session is
    /// hosted exactly like any other, but is withheld from the client's
    /// session list so it can never be offered as something to attach to.
    #[test]
    fn a_live_exploration_session_is_withheld_from_the_client_session_list() {
        let state = judge_test_state();
        let provider_id = ProviderId("builtin.agent.mock".to_string());
        let explore_id = SessionId::new();
        let ordinary_id = SessionId::new();

        spawn_session_thread(
            state.clone(),
            explore_id,
            provider_id.clone(),
            Some(RoleId(horizon_agent::roles::EXPLORE_ROLE_ID.to_string())),
            None,
            None,
            false,
            None,
            Vec::new(),
        );
        spawn_session_thread(
            state.clone(),
            ordinary_id,
            provider_id,
            None,
            None,
            None,
            false,
            None,
            Vec::new(),
        );

        assert!(
            state.sessions.lock().unwrap().contains_key(&explore_id),
            "the exploration session is still a first-class hosted session"
        );
        let listed: Vec<SessionId> = Connection::new(state)
            .session_list()
            .into_iter()
            .map(|summary| summary.session_id)
            .collect();
        assert!(!listed.contains(&explore_id), "{listed:?}");
        assert!(listed.contains(&ordinary_id), "{listed:?}");
    }

    /// [`Connection::session_model`] answers from whatever
    /// [`resolve_and_announce_session_model`] stored on the session's
    /// `SessionEntry` -- the read side of the same "attach re-announces it"
    /// path `Control::SessionLoad`'s handler uses.
    #[test]
    fn connection_session_model_reads_the_stored_value_for_a_known_session_only() {
        let state = state_with_rig_config(true, "test-model");
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

    /// [`Connection::session_list`] must report the authoritative,
    /// post-isolation `workspace_root` from the session's own `SessionEntry`
    /// -- the wire-level counterpart of the state-level assertion above,
    /// and the coordinator's requested regression guard: the workspace
    /// model on the Horizon side reads exactly this field to correct its
    /// own pre-spawn value (`WorkspaceShell::spawn_agent_resume`/
    /// `spawn_workspace_restore`).
    #[test]
    fn session_list_reports_the_entrys_workspace_root_and_parent() {
        let state = state_with_rig_config(true, "test-model");
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
