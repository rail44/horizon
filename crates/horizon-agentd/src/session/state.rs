//! Process-lifetime session state: the shared [`SessiondState`] every
//! connection and every session thread works through, and the per-session
//! [`SessionEntry`] its registry holds.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crossbeam_channel::Sender;

use horizon_agent::config::AgentConfig;
use horizon_agent::contract::{Command, Event, ProviderId, ProviderRegistry, SessionId};
use horizon_agent::persistence::event_log::WriterHandle;
use horizon_agent::persistence::projection::duckdb::{DuckdbStoreHandle, SharedDuckdbStore};
use horizon_agent::roles::RoleId;
use horizon_agent::wire::{AgentWireEvent, HostToolRequest, HostToolResponse};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Notify;

use crate::worktree::WorktreeInfo;

pub(super) fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The per-session event subscribers (one per live agent attachment,
/// installed by the hub's `new_agent`/`attach_agent` and replaced by a
/// re-attach) every session thread sends through — the v10 shape of the
/// old connection-swappable outgoing envelope queue; see the module doc's
/// "sessions are scoped to the process" note. The sender is the local
/// (unbounded, sync-sendable) half of an attachment's event bridge; an
/// async pump owned by the hub drains it into the attachment's remote
/// channel. A send failing means that attachment's bridge is gone
/// (client detached or connection died), so the entry is dropped lazily.
pub(super) type AgentSubscribers = Mutex<HashMap<SessionId, UnboundedSender<AgentWireEvent>>>;

/// Process-lifetime state, built once in `main` and shared (via `Arc`) by
/// every connection `horizon-agentd` ever serves, and by every session
/// thread regardless of which (if any) connection is currently live.
pub(crate) struct SessiondState {
    /// The live provider registry, behind a `Mutex` so a `Reload Config`
    /// can rebuild it in place (see [`Self::reload_provider_config`])
    /// without a `Reload Agent Runtime`. Read at session-spawn time
    /// (see `run`/`spawn`/`resume`); a running session keeps its spawn-time
    /// config for its whole lifetime, so a swap takes effect for the next
    /// session, not a running turn.
    pub(crate) providers: Mutex<ProviderRegistry>,
    /// Same swap story as `providers`: `[provider]`'s `base_url` is read at
    /// spawn time for the enforcing judge (see `run_session`), so a live
    /// reload updates it for new sessions.
    pub(crate) agent_config: Mutex<AgentConfig>,
    /// `None` until [`Self::set_writer`] runs (or forever, if the event log
    /// couldn't be opened -- sessions still run, just without persistence,
    /// the same graceful degrade the deleted in-process agent runtime had).
    /// A `Mutex` rather than a plain field
    /// because `main` now binds the socket and starts accepting connections
    /// *before* the event log is opened (see the bind-first fix in `main`'s
    /// doc comment) -- this is filled in once that finishes, on whatever
    /// thread happens to be running by then.
    writer: Mutex<Option<WriterHandle>>,
    pub(super) sessions: Mutex<HashMap<SessionId, SessionEntry>>,
    pub(super) pending_host_tool_requests: Mutex<HashMap<String, Sender<HostToolResponse>>>,
    pub(super) agent_subscribers: AgentSubscribers,
    /// In-process observers of a session's `contract::Event` stream,
    /// installed alongside (never instead of) the client-facing
    /// [`AgentSubscribers`] above -- see [`super::subscription`], which owns
    /// this map's whole vocabulary.
    pub(super) session_subscriptions: super::subscription::SessionSubscriptions,
    /// The current connection's host-tool request bridge (the local half of
    /// `HubHello::host_tools`), installed by the hub's `hello` and cleared
    /// when the connection ends — connection-global, unlike the
    /// session-scoped subscribers above.
    pub(super) host_tools_outgoing: Mutex<Option<UnboundedSender<HostToolRequest>>>,
    /// Flips once (see [`Self::mark_resume_ready`]) after
    /// [`super::resume::resume_persisted_sessions`] finishes populating `sessions` from the
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
    pub(super) config_path: Option<PathBuf>,
    /// Validated `[[grants.project]]` entries from the same config load
    /// (`main`'s `horizon_config::project_grants` call) --
    /// `docs/containment-denial-narrow-grants-design.md`'s 2026-07-26
    /// decision. Threaded through here rather than re-read from
    /// `horizon_config::load()` at each session spawn for the same reason
    /// `config_path` is: one config load per process, and this crate's own
    /// tests must never observe the developer's real `~/.config/horizon/
    /// config.toml` (`horizon_config`'s `#[cfg(test)]` gate is that crate's
    /// own, and does not apply to test binaries here). Empty at every test
    /// construction site.
    pub(super) project_grants: Vec<horizon_config::ProjectGrant>,
}

impl SessiondState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        providers: ProviderRegistry,
        agent_config: AgentConfig,
        writer: Option<WriterHandle>,
        duckdb_cell: SharedDuckdbStore,
        config_path: Option<PathBuf>,
        project_grants: Vec<horizon_config::ProjectGrant>,
    ) -> Self {
        Self {
            providers: Mutex::new(providers),
            agent_config: Mutex::new(agent_config),
            writer: Mutex::new(writer),
            sessions: Mutex::new(HashMap::new()),
            pending_host_tool_requests: Mutex::new(HashMap::new()),
            agent_subscribers: Mutex::new(HashMap::new()),
            session_subscriptions: Mutex::new(HashMap::new()),
            host_tools_outgoing: Mutex::new(None),
            resume_ready: AtomicBool::new(false),
            resume_notify: Notify::new(),
            skipped_lines_summary: Mutex::new(None),
            duckdb_cell,
            config_path,
            project_grants,
        }
    }

    pub(crate) fn writer(&self) -> Option<WriterHandle> {
        self.writer.lock().unwrap().clone()
    }

    /// Re-reads `[provider]` from the config file at [`Self::config_path`] and
    /// rebuilds both the provider registry and the agent config in place, so
    /// a `Reload Config` can push a model/base-URL change to a running daemon
    /// without a `Reload Agent Runtime` (which exists for agent-code reloads
    /// -- see `docs/terminald-split-design.md` decision 2). A read/parse error
    /// leaves the previous registry untouched and is returned to the caller;
    /// a missing file resolves to built-in defaults, the same outcome
    /// `Reload Config`'s UI-side `horizon_config::reload` already yields for
    /// `[theme]`/`[keybindings]`. The swap takes effect for the *next* session
    /// -- a running session cloned its `RigAgentConfig` into its own thread
    /// at spawn and is unaffected.
    pub(crate) fn reload_provider_config(&self) -> Result<(), String> {
        let raw = horizon_config::reload_from_path(self.config_path.as_deref())?;
        let new_agent_config = AgentConfig::from_env_and_provider(
            raw.provider.model.clone(),
            raw.provider.base_url.clone(),
        );
        let new_providers = ProviderRegistry::builtin_with_config(
            new_agent_config.clone(),
            self.duckdb_cell.clone(),
        );
        *lock_unpoisoned(&self.agent_config) = new_agent_config;
        *lock_unpoisoned(&self.providers) = new_providers;
        Ok(())
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

    /// Called exactly once, after [`super::resume::resume_persisted_sessions`] returns --
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
    /// spawn-source lookup [`crate::worktree::resolve_isolation_source`] needs.
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

    /// Records the outcome of a successful [`crate::worktree::create_isolated_worktree`]
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

    /// Routes a `Command` to `session_id`'s thread, reporting whether there
    /// was a live session to route it to. [`super::connection::Connection::route_command`]
    /// turns a miss into a log line; [`super::exploration::SessiondExplorationHost::terminate`]
    /// deliberately ignores one -- a task session that already
    /// ended on its own needs no shutdown.
    pub(super) fn send_command(&self, session_id: SessionId, command: Command) -> bool {
        let sender = self
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|entry| entry.inbound.clone());
        match sender {
            Some(sender) => sender.send(command).is_ok(),
            None => false,
        }
    }
}

pub(super) struct SessionEntry {
    pub(super) provider_id: ProviderId,
    /// Mirrors `provider_id` -- surfaced in `session_list` summaries
    /// ([`super::connection::Connection::session_list`]) the same way.
    pub(super) role_id: Option<RoleId>,
    /// This session's resolved model id, computed once at spawn time (see
    /// [`super::spawn::spawn_session_thread`]) via [`ProviderRegistry::resolved_model`] --
    /// the same role-adjusted resolution `run_session`'s own
    /// `providers.start_session` call performs, just without waiting on it.
    /// Retained for the whole session lifetime so a later `session_load`
    /// (`Connection::session_model`) can re-announce it to a (re)attaching
    /// client -- see `docs/agent-output-ui-amendment.md`'s dated model-chip
    /// addendum.
    pub(super) model: Option<String>,
    pub(super) inbound: Sender<Command>,
    /// Answers a `session_load` for this session: the session's own thread
    /// receives a one-shot reply channel here and sends back everything its
    /// `LiveState::events()` has accumulated — see
    /// [`super::connection::Connection::replay_events`].
    pub(super) replay: Sender<Sender<Vec<Event>>>,
    /// The session this one derives from -- `Some` only when this session
    /// was actually spawned isolated (see [`SessiondState::
    /// record_isolated_worktree`]); `docs/session-relationship-design.md`
    /// decision 2's "the edge exists only via isolation". Surfaced
    /// additively as `SessionSummary.parent_session_id` by
    /// [`super::connection::Connection::session_list`].
    pub(super) parent_session_id: Option<SessionId>,
    /// The directory this session's file tools are actually confined to --
    /// its own worktree path if `worktree.is_some()`, else whatever
    /// `SessionNew.workspace_root` carried. Read by [`SessiondState::
    /// session_directory`] so a *child* spawned from this session knows
    /// where to branch its own worktree from.
    pub(super) workspace_root: Option<PathBuf>,
    /// This session's own isolated worktree, if [`crate::worktree::
    /// create_isolated_worktree`] succeeded for it -- `None` for an
    /// ordinary shared-directory session. Removed (if clean) when the
    /// session ends, see [`super::spawn::spawn_session_thread`]'s thread body.
    pub(super) worktree: Option<WorktreeInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::state_with_rig_config;
    use crossbeam_channel::unbounded;
    use horizon_agent::contract::Event;

    /// An id agentd has never hosted (or has already ended) reports no
    /// directory -- the "no source" case [`crate::worktree::resolve_isolation_source`]
    /// treats as a lineage root, falling back to the spawn's own
    /// `workspace_root`.
    #[test]
    fn session_directory_is_none_for_an_unknown_session() {
        let state = state_with_rig_config(true, "test-model");
        assert_eq!(state.session_directory(SessionId::new()), None);
    }

    /// A plain (non-isolated) session reports its own `workspace_root` and
    /// `false` (not an owned worktree) -- what a *child* spawned from it
    /// would branch fresh-from-origin against.
    #[test]
    fn session_directory_reports_the_plain_workspace_root_when_not_isolated() {
        let state = state_with_rig_config(true, "test-model");
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
}
