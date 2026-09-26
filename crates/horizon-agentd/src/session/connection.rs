//! One connection's view onto the process-lifetime session state: the
//! handlers behind the hub's session-scoped control calls.

use std::sync::Arc;
use std::time::Duration;

use horizon_agent::contract::SessionId;
use horizon_agent::persistence::event_log::WriterHandle;
use horizon_agent::wire::{
    HostToolRequest, HostToolResponse, ProviderSummary, SessionNew, SessionSummary,
};
use tokio::sync::mpsc::UnboundedSender;

use super::spawn::spawn_session_thread;
use super::state::{lock_unpoisoned, AgentdState};

/// A resumed session may still be waiting for its DuckDB projection.
/// Timeout is an explicit failure, never a successful empty history.
const REPLAY_TIMEOUT: Duration = Duration::from_secs(120);

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
    pub(crate) fn new(state: Arc<AgentdState>) -> Self {
        Self { state }
    }

    pub(crate) fn register_board(&self, root: std::path::PathBuf) -> Result<(), String> {
        let root =
            crate::worktree::project_root(&root).ok_or("Board root is not a Git repository")?;
        self.state.register_board(root);
        Ok(())
    }

    /// Every configured provider with its default model and availability —
    /// the model picker's data. Reads the agent config (the same table the
    /// registry was built from), so it reflects the loaded `[[providers]]`
    /// surface, or the built-in default when the file has none;
    /// entries run in the config file's order. `available` is the
    /// build-time-resolved key presence — the same rule the registry itself
    /// follows; a mid-session environment change is honored by a *switch*,
    /// not by this listing. Candidate models come from the provider's own
    /// `/models` listing ([`Self::list_provider_models`]), not from here.
    pub(crate) fn list_providers(&self) -> Vec<ProviderSummary> {
        let config = lock_unpoisoned(&self.state.agent_config);
        let table = &config.providers;
        let mut summaries: Vec<ProviderSummary> = table
            .entries
            .iter()
            .map(|entry| ProviderSummary {
                name: entry.name.clone(),
                base_url: entry.base_url.clone(),
                api_key_env: entry.api_key_env.clone(),
                default_model: entry.default_model.clone(),
                available: entry.api_key_present,
                default: entry.name == table.default_name,
            })
            .collect();
        // The `[[moa]]` entries ride the same shape as one more group, so
        // the picker and `set_session_model` need no MoA-specific wire
        // surface: its items are the entry names, answered by
        // `list_provider_models` like any other group's. Omitted entirely
        // when nothing is configured.
        if !config.moa.entries.is_empty() {
            // One flag for the group, as `ProviderSummary` carries: false
            // when no entry could run at all, which grays the group out the
            // way a key-less provider entry is grayed out. A group with some
            // usable entries stays selectable and an unusable entry inside
            // it is refused by `set_session_model`.
            let available = config
                .moa
                .entries
                .iter()
                .any(|entry| entry.aggregator.api_key_present);
            summaries.push(ProviderSummary {
                name: horizon_agent::config::MOA_PROVIDER_NAME.to_string(),
                base_url: None,
                api_key_env: String::new(),
                default_model: None,
                available,
                default: false,
            });
        }
        summaries
    }

    /// A provider's own live model-id listing for the picker's discovery
    /// (`SessionHub::list_provider_models`), fetched by the entry's own
    /// resolved base URL and key. An unknown provider, an unavailable entry,
    /// or an endpoint that answers nothing yields an empty list — discovery
    /// augments the picker, it never blocks a pick.
    ///
    /// The reserved `moa` group answers from the config instead: its items
    /// are the `[[moa]]` entry names, in file order, with no request made.
    pub(crate) async fn list_provider_models(&self, provider: &str) -> Vec<String> {
        let entry = {
            let config = lock_unpoisoned(&self.state.agent_config);
            if provider == horizon_agent::config::MOA_PROVIDER_NAME {
                return config
                    .moa
                    .entries
                    .iter()
                    .map(|entry| entry.name.clone())
                    .collect();
            }
            config.providers.entry(provider).cloned()
        };
        match entry {
            Some(entry) => entry.list_model_ids().await,
            None => Vec::new(),
        }
    }

    /// Validate using current config and enqueue the resolved snapshot. Model
    /// state and announcements change only after the provider applies it.
    pub(crate) fn set_session_model(
        &self,
        session_id: SessionId,
        provider: String,
        model: String,
    ) -> Result<(), String> {
        let command = super::model_selection::resolve(&self.state, &provider, &model)?;
        let sessions = lock_unpoisoned(&self.state.sessions);
        let entry = sessions
            .get(&session_id)
            .ok_or_else(|| format!("Unknown session {session_id:?}."))?;
        entry
            .inbound
            .send(command)
            .map_err(|_| "Session is no longer accepting commands.".to_string())?;
        Ok(())
    }

    pub(crate) fn ensure_board_organizer(
        &self,
        root: std::path::PathBuf,
    ) -> Result<SessionId, String> {
        self.register_board(root.clone())?;
        crate::board_flow::organizer_session(&self.state, &root)
    }

    /// Installs the current connection's host-tool bridge (the local half
    /// behind `HubHello::host_tools`) — the connection-global counterpart
    /// of the per-attachment subscriptions installed by [`Self::attach`].
    pub(crate) fn connect_host_tools(&self, outgoing: UnboundedSender<HostToolRequest>) {
        *self.state.host_tools_outgoing.lock().unwrap() = Some(outgoing);
    }

    /// Clears the connection-global host-tool bridge on disconnect, so a
    /// session thread's `execute_auto` fails fast instead of enqueueing
    /// into a bridge whose pump already died with the connection. The
    /// per-session subscribers are deliberately *not* swept here: each
    /// attachment's bridge dies with its own pump, and
    /// [`super::events::send_session_event`] already drops an entry lazily on its first
    /// failed send (a fresh attach replaces it anyway).
    pub(crate) fn disconnect(&self) {
        *self.state.host_tools_outgoing.lock().unwrap() = None;
    }

    #[cfg(test)]
    pub(crate) fn subscribe_agent(
        &self,
        session_id: SessionId,
    ) -> tokio::sync::mpsc::Receiver<horizon_agent::wire::AgentWireEvent> {
        super::attachment::subscribe(&self.state, session_id)
    }

    /// Spawns the session thread for a `Control::SessionNew`. Reuses the
    /// crate's existing spawn shape (`ProviderRegistry::start_session`) --
    /// the same call the deleted in-process agent runtime used to make
    /// before every agent session moved here.
    pub(crate) fn handle_session_new(&self, new: SessionNew) -> Result<(), String> {
        let _lifecycle = lock_unpoisoned(&self.state.lifecycle);
        if self.state.session_exists(new.session_id) {
            return Err("Session already exists; attach to it instead".into());
        }
        if let Some(failure) = self.state.writer().and_then(|writer| writer.failure()) {
            return Err(format!("Cannot start session: event log failed: {failure}"));
        }
        if !lock_unpoisoned(&self.state.providers).contains(&new.provider_id) {
            return Err(format!("Unknown agent provider {}", new.provider_id.0));
        }
        if let Some(role) = &new.role_id {
            if horizon_agent::roles::resolve(role).is_none() {
                return Err(format!("Unknown role `{}`", role.0));
            }
        }
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
        Ok(())
    }

    /// Routes an incoming `Control::HostToolResponse` back to whichever
    /// session thread's `host_tools::AgentdHostTools::execute_auto` call is blocked
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

    /// Delegates to [`AgentdState::skipped_lines_summary`] -- see the hub's
    /// `hello` (`crate::hub`), which waits for [`Self::wait_until_resume_ready`]
    /// first so this always reflects the finished startup read.
    pub(crate) fn skipped_lines_summary(&self) -> Option<String> {
        self.state.skipped_lines_summary()
    }

    /// Delegates to [`AgentdState::reload_provider_config`] -- the
    /// daemon-side half of a `Reload Config`: rebuild the provider
    /// registry/agent config from the config file without respawning the
    /// process. See that method's doc comment for the granularity.
    pub(crate) fn reload_provider_config(&self) -> Result<(), String> {
        self.state.reload_provider_config()
    }

    /// Every session a client may see. Exploration sessions
    /// (`docs/agent-explore-design.md` decision 3: "invisible to the UI")
    /// are withheld: they are never attached to a pane, they live only as
    /// long as the `task` call waiting on them, and offering one
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
    #[cfg(test)]
    fn session_model(&self, session_id: SessionId) -> Option<String> {
        self.state
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .and_then(|entry| entry.model.clone())
    }

    /// The hub's graceful-exit flush barrier, including non-session appends.
    pub(crate) fn writer(&self) -> Option<WriterHandle> {
        self.state.writer()
    }

    /// Capture and register on the session's own thread. Cancelling this wait
    /// drops the reply receiver; a late reply drops its lease automatically.
    pub(crate) async fn attach(
        &self,
        session_id: SessionId,
    ) -> Result<super::attachment::Bootstrap, String> {
        self.attach_with_timeout(session_id, REPLAY_TIMEOUT).await
    }

    pub(super) async fn attach_with_timeout(
        &self,
        session_id: SessionId,
        timeout: Duration,
    ) -> Result<super::attachment::Bootstrap, String> {
        let request = lock_unpoisoned(&self.state.sessions)
            .get(&session_id)
            .map(|entry| entry.replay.clone())
            .ok_or_else(|| format!("Unknown agent session {session_id:?}"))?;
        let (reply, response) = tokio::sync::oneshot::channel();
        request
            .send(reply)
            .map_err(|_| "Session ended before history could be restored".to_string())?;
        tokio::time::timeout(timeout, response)
            .await
            .map_err(|_| {
                "Timed out restoring session history; reopen the session to retry".to_string()
            })?
            .map_err(|_| "Session ended while restoring history".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::state::SessionEntry;
    use crate::session::test_support::{state_with_rig_config, test_state};
    use crossbeam_channel::unbounded;
    use horizon_agent::config::{NamedProviderConfig, ProviderKind, ProvidersTable};
    use horizon_agent::contract::Command;
    use horizon_agent::contract::ProviderId;
    use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
    use horizon_agent::registry::ProviderRegistry;
    use horizon_agent::roles::RoleId;
    use std::sync::Arc;

    /// Decision 3, "invisible to the UI": a live exploration session is
    /// hosted exactly like any other, but is withheld from the client's
    /// session list so it can never be offered as something to attach to.
    #[test]
    fn a_live_exploration_session_is_withheld_from_the_client_session_list() {
        let state = test_state();
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
        let (replay_tx, _replay_rx) = unbounded::<crate::session::attachment::AttachRequest>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: Some("stored-model".to_string()),
                selection: None,
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
        let (replay_tx, _replay_rx) = unbounded::<crate::session::attachment::AttachRequest>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: None,
                selection: None,
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
    /// A hermetic two-provider surface (never `from_env_and_provider`'s
    /// real env reads): one openai-compatible entry with its key present,
    /// one anthropic entry with its key unset — the two-provider
    /// completion condition's state-level shape.
    fn two_provider_state() -> (Arc<AgentdState>, Vec<NamedProviderConfig>) {
        two_provider_state_with_moa(horizon_agent::config::MoaTable::default())
    }

    fn two_provider_state_with_moa(
        moa: horizon_agent::config::MoaTable,
    ) -> (Arc<AgentdState>, Vec<NamedProviderConfig>) {
        let entries = vec![
            NamedProviderConfig {
                name: "openai".to_string(),
                kind: ProviderKind::OpenAiCompatible,
                base_url: None,
                api_key_env: "OPENAI_API_KEY".to_string(),
                api_key_present: true,
                default_model: Some("m-fast".to_string()),
            },
            NamedProviderConfig {
                name: "claude".to_string(),
                kind: ProviderKind::Anthropic,
                base_url: None,
                api_key_env: "ANTHROPIC_API_KEY".to_string(),
                api_key_present: false,
                default_model: Some("m-opus".to_string()),
            },
        ];
        let agent_config = horizon_agent::config::AgentConfig {
            auxiliary: None,
            rig: horizon_agent::config::RigAgentConfig {
                api_key_present: true,
                model: "test-model".to_string(),
                ..Default::default()
            },
            providers: ProvidersTable {
                entries: entries.clone(),
                default_name: "openai".to_string(),
            },
            moa,
            persistence: horizon_agent::config::AgentPersistenceConfig {
                event_log_path: std::path::PathBuf::from(
                    "/tmp/horizon-connection-test-events.jsonl",
                ),
                duckdb_path: None,
            },
            tools: horizon_agent::config::AgentToolsConfig::default(),
        };
        let state = Arc::new(AgentdState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
            Vec::new(),
            Vec::new(),
        ));
        (state, entries)
    }

    /// `list_providers` reports every configured provider — both of them,
    /// with the key-less one registered but unavailable (`available:
    /// false`, never dropped) — in file order, with the default flagged.
    #[test]
    fn list_providers_reports_both_entries_and_marks_the_unavailable_one() {
        let (state, _entries) = two_provider_state();
        let connection = Connection { state };
        let summaries = connection.list_providers();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].name, "openai");
        assert!(summaries[0].available);
        assert!(summaries[0].default);
        assert_eq!(summaries[0].default_model.as_deref(), Some("m-fast"));
        assert_eq!(summaries[1].name, "claude");
        assert!(!summaries[1].available);
        assert!(!summaries[1].default);
        // The key variable's NAME is reported (never a value), so a picker
        // can explain unavailability.
        assert_eq!(summaries[1].api_key_env, "ANTHROPIC_API_KEY");
    }

    /// Acceptance queues a validated snapshot without changing applied state.
    #[test]
    fn set_session_model_queues_a_snapshot_without_announcing_application() {
        let (state, _entries) = two_provider_state();
        let session_id = SessionId::new();
        let (inbound_tx, inbound_rx) = unbounded::<Command>();
        let (replay_tx, _replay_rx) = unbounded::<crate::session::attachment::AttachRequest>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: Some("test-model".to_string()),
                selection: None,
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: None,
                workspace_root: None,
                worktree: None,
            },
        );
        let connection = Connection { state };
        let mut events = connection.subscribe_agent(session_id);
        connection
            .set_session_model(session_id, "claude".to_string(), "m-opus".to_string())
            .unwrap();

        assert_eq!(
            connection.session_model(session_id).as_deref(),
            Some("test-model")
        );
        let Command::ApplySessionModel(selection) = inbound_rx.recv().unwrap() else {
            panic!("expected a resolved model switch");
        };
        assert_eq!(selection.model(), "m-opus");
        assert_eq!(selection.requested_provider(), "claude");
        assert_eq!(selection.requested_model(), "m-opus");
        assert!(events.try_recv().is_err(), "queueing is not application");
    }

    #[test]
    fn a_closed_session_channel_does_not_announce_an_unapplied_model_switch() {
        let (state, _) = two_provider_state();
        let session_id = SessionId::new();
        drop(state.install_test_session(session_id));
        let connection = Connection::new(state);
        let before = connection.session_model(session_id);
        let mut events = connection.subscribe_agent(session_id);
        assert!(connection
            .set_session_model(session_id, "openai".into(), "new-model".into())
            .is_err());
        assert_eq!(connection.session_model(session_id), before);
        assert!(events.try_recv().is_err());
    }

    fn moa_member(
        provider: &str,
        model: &str,
        api_key_present: bool,
    ) -> horizon_agent::config::MoaMember {
        horizon_agent::config::MoaMember {
            api_key_present,
            api_key_env: format!("{}_API_KEY", provider.to_uppercase()),
            ..horizon_agent::config::MoaMember::new(provider.to_string(), model.to_string())
        }
    }

    /// `mix` aggregates on the available `openai` entry; `stranded` on the
    /// key-less `claude` one.
    fn moa_table() -> horizon_agent::config::MoaTable {
        horizon_agent::config::MoaTable {
            entries: vec![
                horizon_agent::config::MoaEntry {
                    name: "mix".to_string(),
                    aggregator: moa_member("openai", "m-aggregate", true),
                    proposers: vec![moa_member("openai", "m-fast", true)],
                },
                horizon_agent::config::MoaEntry {
                    name: "stranded".to_string(),
                    aggregator: moa_member("claude", "m-opus", false),
                    proposers: vec![moa_member("openai", "m-fast", true)],
                },
            ],
        }
    }

    /// The configured `[[moa]]` entries are offered as one more group
    /// whose items are the entry names; a surface with none adds nothing.
    #[tokio::test]
    async fn list_providers_offers_the_moa_entries_as_their_own_group() {
        let (state, _entries) = two_provider_state_with_moa(moa_table());
        let connection = Connection { state };
        let summaries = connection.list_providers();
        assert_eq!(summaries.len(), 3);
        let group = summaries.last().unwrap();
        assert_eq!(group.name, "moa");
        assert!(!group.default);
        assert!(group.available, "its aggregator's key is present");
        assert_eq!(group.default_model, None);
        assert_eq!(
            connection.list_provider_models("moa").await,
            vec![
                "mix".to_string(),
                // Listed even though its aggregator has no key: the group
                // carries one availability flag, so an unusable entry is
                // refused on confirm rather than hidden.
                "stranded".to_string(),
            ]
        );

        let (state, _entries) = two_provider_state();
        let connection = Connection { state };
        assert_eq!(connection.list_providers().len(), 2);
        assert!(connection.list_provider_models("moa").await.is_empty());
    }

    /// MoA resolution captures the aggregator without prematurely announcing it.
    #[test]
    fn set_session_model_captures_the_moa_aggregator_before_application() {
        let (state, _entries) = two_provider_state_with_moa(moa_table());
        let session_id = SessionId::new();
        let (inbound_tx, inbound_rx) = unbounded::<Command>();
        let (replay_tx, _replay_rx) = unbounded::<crate::session::attachment::AttachRequest>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: Some("test-model".to_string()),
                selection: None,
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: None,
                workspace_root: None,
                worktree: None,
            },
        );
        let connection = Connection { state };
        connection
            .set_session_model(session_id, "moa".to_string(), "mix".to_string())
            .unwrap();
        assert_eq!(
            connection.session_model(session_id).as_deref(),
            Some("test-model")
        );
        let Command::ApplySessionModel(selection) = inbound_rx.recv().unwrap() else {
            panic!("expected a resolved MoA selection");
        };
        assert_eq!(selection.model(), "m-aggregate");
        assert_eq!(selection.requested_provider(), "moa");
        assert_eq!(selection.requested_model(), "mix");

        let error = connection
            .set_session_model(session_id, "moa".to_string(), "typo".to_string())
            .unwrap_err();
        assert!(error.contains("Unknown moa entry `typo`"), "{error}");

        // An entry whose aggregator's key variable is unset is refused with
        // the reason, not accepted into a session that would then answer
        // from the deterministic fallback responder.
        let error = connection
            .set_session_model(session_id, "moa".to_string(), "stranded".to_string())
            .unwrap_err();
        assert!(error.contains("unavailable"), "{error}");
        assert!(error.contains("CLAUDE_API_KEY"), "{error}");
        assert_eq!(
            connection.session_model(session_id).as_deref(),
            Some("test-model"),
            "the refused switch left the previous selection in place"
        );
    }

    /// The group is grayed out only when no entry could run at all; with a
    /// mix it stays selectable and the unusable entry is refused on confirm.
    #[test]
    fn the_moa_group_is_unavailable_only_when_no_entry_can_run() {
        let (state, _entries) = two_provider_state_with_moa(moa_table());
        let connection = Connection { state };
        assert!(connection.list_providers().last().unwrap().available);

        let stranded_only = horizon_agent::config::MoaTable {
            entries: vec![horizon_agent::config::MoaEntry {
                name: "stranded".to_string(),
                aggregator: moa_member("claude", "m-opus", false),
                proposers: vec![moa_member("openai", "m-fast", true)],
            }],
        };
        let (state, _entries) = two_provider_state_with_moa(stranded_only);
        let connection = Connection { state };
        let group = connection.list_providers().last().unwrap().clone();
        assert_eq!(group.name, "moa");
        assert!(!group.available);
    }

    #[test]
    fn set_session_model_rejects_an_unknown_provider_and_an_unknown_session() {
        let (state, _entries) = two_provider_state();
        let connection = Connection { state };
        let error = connection
            .set_session_model(SessionId::new(), "typo".to_string(), "m".to_string())
            .unwrap_err();
        assert!(error.contains("Unknown provider `typo`"), "{error}");

        // A known provider against an unknown session is the caller's bug
        // too — an error, not a silent no-op.
        let error = connection
            .set_session_model(SessionId::new(), "openai".to_string(), "m-fast".to_string())
            .unwrap_err();
        assert!(error.contains("Unknown session"), "{error}");
    }

    /// Discovery never blocks a pick: an unavailable entry (no key) answers
    /// an empty list without ever making a request, and an unknown provider
    /// is empty too (not an error).
    #[tokio::test]
    async fn list_provider_models_is_empty_for_an_unavailable_or_unknown_provider() {
        let (state, _entries) = two_provider_state();
        let connection = Connection { state };
        assert!(connection.list_provider_models("claude").await.is_empty());
        assert!(connection.list_provider_models("typo").await.is_empty());
    }
}
