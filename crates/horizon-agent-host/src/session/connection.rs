//! One connection's view onto the process-lifetime session state: the
//! handlers behind the hub's session-scoped control calls.

use std::sync::Arc;
use std::time::Duration;

use horizon_agent::config::ReasoningLevel;
use horizon_agent::contract::Event;
use horizon_agent::contract::{Command, SessionId};
use horizon_agent::hosting::{ModelSelection, ReasoningSelection, SessionNew, SessionSummary};
use horizon_agent::persistence::event_log::{Appender, PersistedSessionContext};

use super::spawn::spawn_session_thread;
use super::state::{AgentdState, lock_unpoisoned};
use crate::worktree::WorktreeInfo;

/// A resumed session may still be waiting for its DuckDB projection.
/// Timeout is an explicit failure, never a successful empty history.
const REPLAY_TIMEOUT: Duration = Duration::from_secs(120);

/// One connection's view onto the process-lifetime [`AgentdState`] — thin by
/// design (step 4): every map that used to live here moved to `AgentdState`
/// so sessions survive a reconnect, leaving `Connection` as just the `Arc`
/// handle plus the methods that make sense scoped to "the current
/// connection" (installing/clearing `outgoing`).
#[derive(Clone)]
pub struct Connection {
    state: Arc<AgentdState>,
}

impl Connection {
    pub fn new(state: Arc<AgentdState>) -> Self {
        Self { state }
    }

    /// Sends a client's answer to an approval `requester` showed for one of
    /// its `task` children (`docs/agent-subagent-design.md` decision 4) to
    /// that child -- only while the approval is still pending there, which
    /// is also what ties the answer to a child of this requester.
    pub(crate) fn answer_task_approval(
        &self,
        requester: SessionId,
        occurrence: &str,
        command: horizon_agent::contract::Command,
    ) -> Result<(), String> {
        let child = lock_unpoisoned(&self.state.agent_subscribers)
            .get(&requester)
            .and_then(|stream| stream.task_approval(occurrence))
            .ok_or("the task's approval is no longer pending")?;
        if self.state.send_command(child, command) {
            Ok(())
        } else {
            Err("the task session has ended".to_string())
        }
    }

    /// The roles a session can start with, as `(id, title)`.
    pub(crate) fn modes(&self) -> Vec<(String, String)> {
        (self.state.packages().user_launchable().into_iter())
            .map(|role| (role.id.to_string(), role.title.to_string()))
            .collect()
    }

    /// The provider a new session starts on when none is named.
    pub(crate) fn default_provider_id(
        &self,
    ) -> Result<horizon_agent::contract::ProviderId, String> {
        self.state.default_provider_id()
    }

    /// The provider and model a new session starts on.
    pub(crate) fn default_model(&self) -> Option<(String, String)> {
        let config = lock_unpoisoned(&self.state.agent_config);
        if let Some(pair) = &config.providers.default_model {
            return Some((pair.provider.clone(), pair.model.clone()));
        }
        drop(config);
        let provider = self.state.default_provider_id().ok()?;
        let model = lock_unpoisoned(&self.state.agent_config).rig.model.clone();
        Some((provider.0, model))
    }

    /// The model catalog as it is now, and as it changes.
    pub(crate) fn model_catalog(
        &self,
    ) -> tokio::sync::watch::Receiver<Arc<Vec<super::state::ModelGroup>>> {
        self.state.model_catalog()
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

    #[cfg(test)]
    pub(crate) fn subscribe_agent(
        &self,
        session_id: SessionId,
    ) -> tokio::sync::mpsc::Receiver<horizon_agent::hosting::AgentWireEvent> {
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
        if let Some(role) = &new.role_id
            && self.state.packages().resolve_role(role).is_none()
        {
            return Err(format!("Unknown role `{}`", role.0));
        }
        spawn_session_thread(
            self.state.clone(),
            new.session_id,
            new.provider_id,
            new.role_id,
            new.workspace_root,
            new.spawn_source_session_id,
            None,
            Vec::new(),
        );
        Ok(())
    }

    /// Starts a session whose history is `source`'s branch up to the event
    /// `point` picks from it (`None`: none of it), in `workspace_root`. Its
    /// first record follows that event's record, so its own branch reaches
    /// into `source`'s.
    pub(crate) fn handle_session_fork(
        &self,
        source: SessionId,
        workspace_root: std::path::PathBuf,
        point: impl FnOnce(&[horizon_agent::contract::Event]) -> Result<Option<usize>, String>,
    ) -> Result<SessionId, ForkError> {
        let _lifecycle = lock_unpoisoned(&self.state.lifecycle);
        let writer = (self.state.writer()).ok_or(ForkError::Failed(
            "Session persistence is unavailable".into(),
        ))?;
        if let Some(failure) = writer.failure() {
            return Err(ForkError::Failed(format!(
                "Cannot start session: event log failed: {failure}"
            )));
        }
        writer
            .flush()
            .map_err(|error| ForkError::Failed(error.to_string()))?;
        let records = self
            .state
            .flushed_session_records(source)
            .map_err(ForkError::Failed)?;
        let own = || {
            records
                .iter()
                .rev()
                .filter(|record| record.session_id == source)
        };
        let provider_id = own()
            .find_map(|record| record.provider_id.clone())
            .ok_or_else(|| ForkError::Position(format!("Unknown agent session {source:?}")))?;
        let role_id = own().find_map(|record| record.role_id.clone());
        if !lock_unpoisoned(&self.state.providers).contains(&provider_id) {
            return Err(ForkError::Failed(format!(
                "Unknown agent provider {}",
                provider_id.0
            )));
        }
        if let Some(role) = &role_id
            && self.state.packages().resolve_role(role).is_none()
        {
            return Err(ForkError::Failed(format!("Unknown role `{}`", role.0)));
        }
        let events: Vec<_> = records.iter().map(|record| record.event.clone()).collect();
        let last = point(&events).map_err(ForkError::Position)?;
        let (mut history, mut parent) = match last {
            Some(last) => (
                events[..=last].to_vec(),
                Some(records[last].event_id.clone()),
            ),
            None => (Vec::new(), None),
        };
        let source_dir = lock_unpoisoned(&self.state.sessions)
            .get(&source)
            .and_then(|entry| {
                (entry
                    .worktree
                    .as_ref()
                    .map(|worktree| worktree.path.clone()))
                .or_else(|| entry.workspace_root.clone())
            })
            .or_else(|| {
                own().find_map(|record| record.session_context.as_ref()?.workspace_root.clone())
            });
        let session_id = SessionId::new();
        let worktree = match source_dir.as_ref().filter(|_| self.state.checkpoints()) {
            Some(dir) => {
                // The files at the position: the checkpoint taken in the
                // settled stretch it ends, or, for the whole of a session
                // between turns, the files as they are.
                let checkpoint = last.and_then(|last| {
                    settled_stretch(&events, last)
                        .rev()
                        .find_map(|at| {
                            let record = &records[at];
                            crate::checkpoint::find(
                                dir,
                                record.session_id.as_uuid(),
                                &record.event_id,
                            )
                        })
                        .or_else(|| {
                            let record = &records[last];
                            (last + 1 == records.len())
                                .then(|| {
                                    crate::checkpoint::record(
                                        dir,
                                        record.session_id.as_uuid(),
                                        &record.event_id,
                                    )
                                })
                                .and_then(|recorded| recorded.ok().flatten())
                        })
                });
                fork_worktree(dir, session_id, checkpoint).map_err(ForkError::Failed)?
            }
            None => None,
        };
        let Some(worktree) = worktree else {
            super::spawn::spawn_session_thread_with_context(
                self.state.clone(),
                session_id,
                provider_id,
                role_id,
                Some(workspace_root),
                Some(source),
                None,
                history,
                None,
                parent,
            );
            return Ok(session_id);
        };
        // The fork's first record says which worktree it owns, so a restart
        // restores that one.
        let context = PersistedSessionContext {
            workspace_root: Some(worktree.path.clone()),
            isolated_worktree: true,
            parent_session_id: Some(source),
            filesystem_grants: Vec::new(),
            spawn_source_session_id: Some(source),
        };
        let activated = Event::EnvironmentActivated(worktree.identity());
        let mut appender = Appender::new(
            writer.clone(),
            session_id,
            Some(provider_id.clone()),
            role_id.clone(),
        )
        .with_session_context(context.clone())
        .with_parent(parent);
        appender
            .commit_provider_events(vec![activated.clone().into()])
            .map_err(|error| ForkError::Failed(error.to_string()))?;
        history.push(activated);
        parent = appender.tip();
        super::spawn::spawn_session_thread_with_context(
            self.state.clone(),
            session_id,
            provider_id,
            role_id,
            Some(worktree.path.clone()),
            Some(source),
            Some(worktree),
            history,
            Some(context),
            parent,
        );
        Ok(session_id)
    }

    /// Delegates to [`AgentdState::wait_until_resume_ready`] -- see `main`'s
    /// bind-first startup fix: `Control::SessionList`/`Control::SessionLoad`
    /// must block on this before answering, so a client that connects while
    /// `resume_persisted_sessions` is still running doesn't see an
    /// incomplete (or, right after bind, empty) session list.
    pub(crate) fn state(&self) -> &Arc<AgentdState> {
        &self.state
    }

    /// The extension that serves the client request `method`, if any.
    pub(crate) fn extension_serving(
        &self,
        method: &str,
    ) -> Option<Arc<dyn crate::extension::Extension>> {
        self.state.extensions().serving(method)
    }

    pub(crate) async fn wait_until_resume_ready(&self) {
        self.state.wait_until_resume_ready().await;
    }

    /// Every session a client may see. Exploration sessions
    /// (`docs/agent-explore-design.md` decision 3: "invisible to the UI")
    /// are withheld: they are never attached to a pane, they live only as
    /// long as the `task` call waiting on them, and offering one
    /// in the session manager's attach list would invite a user into a
    /// read-only session that is about to be terminated under them. They
    /// remain fully first-class in the event log and DuckDB projection,
    /// which is where their cost is actually measured.
    pub fn session_list(&self) -> Vec<SessionSummary> {
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

    /// One session's summary, exploration sessions included (an attachment
    /// to one needs its facts even though the list withholds it).
    pub(crate) fn session_summary(&self, session_id: SessionId) -> Option<SessionSummary> {
        lock_unpoisoned(&self.state.sessions)
            .get(&session_id)
            .map(|entry| SessionSummary {
                session_id,
                provider_id: entry.provider_id.clone(),
                role_id: entry.role_id.clone(),
                parent_session_id: entry.parent_session_id,
                workspace_root: entry.workspace_root.clone(),
            })
    }

    /// The session's applied model id and selection, as the last
    /// announcement left them.
    pub(crate) fn session_model_state(
        &self,
        session_id: SessionId,
    ) -> (
        Option<String>,
        Option<ModelSelection>,
        Option<ReasoningSelection>,
    ) {
        lock_unpoisoned(&self.state.sessions)
            .get(&session_id)
            .map(|entry| {
                (
                    entry.model.clone(),
                    entry.selection.clone(),
                    entry.reasoning.clone(),
                )
            })
            .unwrap_or_default()
    }

    /// Queue a reasoning switch. The session applies it at its next turn
    /// boundary and announces what it applied; a level its model does not
    /// take is refused here, where the caller can be told.
    pub(crate) fn set_session_reasoning(
        &self,
        session_id: SessionId,
        level: ReasoningLevel,
    ) -> Result<(), String> {
        let sessions = lock_unpoisoned(&self.state.sessions);
        let entry = sessions
            .get(&session_id)
            .ok_or_else(|| format!("Unknown session {session_id:?}."))?;
        if let Some(reasoning) = &entry.reasoning
            && !reasoning.choices.contains(&level)
        {
            return Err(format!(
                "The session's model does not take effort {:?}.",
                level.as_str()
            ));
        }
        entry
            .inbound
            .send(Command::SetSessionReasoning { level })
            .map_err(|_| "Session is no longer accepting commands.".to_string())
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

/// Why a fork was not started.
#[derive(Debug)]
pub(crate) enum ForkError {
    /// The source or the position in it cannot be forked.
    Position(String),
    /// The fork could not be started.
    Failed(String),
}

/// A worktree of its own for the fork `session_id` of a session working in
/// `dir`, holding the files of `checkpoint` (`crate::checkpoint`): branched
/// at the commit the checkpoint was taken on, with what was not committed
/// there not committed here either. Without a checkpoint it is branched at
/// `dir`'s `HEAD`. `None` when `dir` is not in a git repository.
fn fork_worktree(
    dir: &std::path::Path,
    session_id: SessionId,
    checkpoint: Option<String>,
) -> Result<Option<WorktreeInfo>, String> {
    if crate::worktree::project_root(dir).is_none() {
        return Ok(None);
    }
    let base = match &checkpoint {
        Some(checkpoint) => crate::worktree::resolve_commit(dir, &format!("{checkpoint}^"))
            .unwrap_or_else(|_| checkpoint.clone()),
        None => "HEAD".to_string(),
    };
    let worktree = crate::worktree::create_isolated_worktree_at(dir, session_id.as_uuid(), &base)?;
    if let Some(checkpoint) = &checkpoint {
        crate::checkpoint::restore(&worktree.path, checkpoint)?;
    }
    Ok(Some(worktree))
}

/// The positions settled one after another up to `last` (itself settled):
/// the event indexes after which a session sits between turns, from the
/// first of that stretch to `last`.
fn settled_stretch(
    events: &[horizon_agent::contract::Event],
    last: usize,
) -> std::ops::RangeInclusive<usize> {
    let mut settled = horizon_agent::persistence::event_log::Settled::new();
    let mut start = 0;
    for (index, event) in events[..=last].iter().enumerate() {
        settled.apply(event);
        if !settled.is_settled() {
            start = index + 1;
        }
    }
    start.min(last)..=last
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::state::SessionEntry;
    use crate::session::test_support::{state_with_rig_config, test_state};
    use crossbeam_channel::unbounded;
    use horizon_agent::config::{NamedProviderConfig, ProvidersTable, Vendor};
    use horizon_agent::contract::Command;
    use horizon_agent::contract::ProviderId;
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
                reasoning: None,
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: None,
                spawn_source_session_id: None,
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
    /// resolved `workspace_root` from the session's own `SessionEntry`
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
                reasoning: None,
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: Some(parent_id),
                spawn_source_session_id: None,
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
        let entries = vec![
            NamedProviderConfig {
                name: "openai".to_string(),
                vendor: Vendor::openai(),
                base_url: None,
                api_key_env: "OPENAI_API_KEY".to_string(),
                api_key_present: true,
            },
            NamedProviderConfig {
                name: "claude".to_string(),
                vendor: Vendor::resolve("anthropic").unwrap(),
                base_url: None,
                api_key_env: "ANTHROPIC_API_KEY".to_string(),
                api_key_present: false,
            },
        ];
        let agent_config = horizon_agent::config::AgentConfig {
            auxiliary: None,
            decision: None,
            rig: horizon_agent::config::RigAgentConfig {
                api_key_present: true,
                model: "test-model".to_string(),
                ..Default::default()
            },
            providers: ProvidersTable {
                entries: entries.clone(),
                default_model: Some(horizon_agent::config::ModelPair {
                    provider: "openai".to_string(),
                    model: "m-fast".to_string(),
                    effort: None,
                }),
            },
            persistence: horizon_agent::config::AgentPersistenceConfig {
                sessions_dir: std::path::PathBuf::from("/tmp/horizon-connection-test-sessions"),
                legacy_event_log: None,
                duckdb_path: None,
            },
            tools: horizon_agent::config::AgentToolsConfig::default(),
        };
        let state = Arc::new(AgentdState::new(
            ProviderRegistry::builtin_with_config(agent_config.clone()),
            agent_config,
            None,
            None,
            Vec::new(),
            Vec::new(),
        ));
        (state, entries)
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
                reasoning: None,
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: None,
                spawn_source_session_id: None,
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
        assert!(
            connection
                .set_session_model(session_id, "openai".into(), "new-model".into())
                .is_err()
        );
        assert_eq!(connection.session_model(session_id), before);
        assert!(events.try_recv().is_err());
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
    /// A provider without its key is not asked for models, and is not in
    /// the catalog.
    #[tokio::test]
    async fn the_model_catalog_leaves_out_a_provider_without_its_key() {
        let (state, _entries) = two_provider_state();
        state
            .agent_config
            .lock()
            .unwrap()
            .providers
            .entries
            .retain(|entry| !entry.api_key_present);
        state.refresh_model_catalog().await;
        assert!(state.model_catalog().borrow().is_empty());
    }
}
