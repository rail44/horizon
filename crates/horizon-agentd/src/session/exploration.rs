//! `horizon-agent`'s `task` seam, implemented against this
//! daemon's own session hosting.

use std::path::PathBuf;
use std::sync::Arc;

use horizon_agent::contract::{Command, ProviderId, SessionId, TaskProgress};
use horizon_agent::wire::AgentWireEvent;

use super::events::send_session_event;
use super::spawn::spawn_session_thread;
use super::state::{lock_unpoisoned, AgentdState};

/// `horizon-agent`'s `task` seam (`docs/agent-explore-design.md`),
/// implemented against this daemon's own session hosting: spawn a peer
/// session, subscribe to its events, terminate it. One is built per
/// requesting session in [`super::run::run_session`] and installed on its
/// `ToolSessionState`, carrying that session's provider and resolved
/// workspace root so an exploration always runs where its requester does.
///
/// **Peer, not child** (decision 2): the exploration is spawned with no
/// spawn source and `isolate: false`, so it shares the requester's exact
/// working tree -- including an isolated requester's worktree, whose
/// uncommitted state is precisely the view mid-task exploration needs --
/// and records no derivation edge. The derivation tree stays pure code
/// genealogy (`docs/session-relationship-design.md`: only isolation creates
/// an edge).
pub(super) struct AgentdExplorationHost {
    pub(super) state: Arc<AgentdState>,
    /// The session this host was installed on -- every task it launches is
    /// reported back to *this* session's attached client.
    pub(super) requester_id: SessionId,
    /// The requesting session's provider, so an exploration is answered by
    /// the same model family the requester is talking to.
    pub(super) provider_id: ProviderId,
    /// The requesting session's own resolved root -- post-isolation, so an
    /// isolated requester's exploration reads that worktree and not the
    /// daemon's cwd.
    pub(super) workspace_root: Option<PathBuf>,
}

impl horizon_agent::tools::ExplorationHost for AgentdExplorationHost {
    fn start(
        &self,
        request: horizon_agent::tools::ExplorationRequest,
    ) -> Result<horizon_agent::tools::StartedExploration, String> {
        // A named provider is validated before anything is spawned: an id
        // the registry does not know would otherwise fail inside the
        // session thread, after the caller already got a session id.
        let provider_id = match &request.provider {
            Some(name) => {
                let id = horizon_agent::registry::named_rig_provider_id(name);
                if !lock_unpoisoned(&self.state.providers).contains(&id) {
                    return Err(format!("no provider `{name}` is configured"));
                }
                id
            }
            None => self.provider_id.clone(),
        };
        let session_id = SessionId::new();
        // Subscribe first, spawn second -- the ordering requirement
        // `super::subscription` documents: the subscription has to exist
        // before the session's thread can emit anything.
        let subscription = self.state.subscribe_to_session(session_id);
        spawn_session_thread(
            self.state.clone(),
            session_id,
            provider_id,
            Some(request.role.clone()),
            self.workspace_root.clone(),
            None,
            false,
            None,
            Vec::new(),
        );
        // Ordered ahead of the prompt on the same channel, so the session's
        // first turn already runs the pinned model.
        if let (Some(provider), Some(model)) = (&request.provider, &request.model) {
            self.state.send_command(
                session_id,
                Command::SetSessionModel {
                    provider: provider.clone(),
                    model: model.clone(),
                },
            );
        }
        if !self.state.send_command(
            session_id,
            Command::UserMessage {
                text: request.prompt,
            },
        ) {
            self.state.unsubscribe_from_session(session_id);
            return Err("the task session ended before it could be asked".to_string());
        }
        Ok(horizon_agent::tools::StartedExploration {
            session_id: subscription.session_id,
            events: subscription.events,
        })
    }

    fn terminate(&self, session_id: SessionId) {
        self.state.unsubscribe_from_session(session_id);
        self.state.send_command(session_id, Command::Shutdown);
    }

    fn forward_progress(&self, _child: SessionId, progress: TaskProgress) {
        // Ephemeral by design: silently dropped when no client is attached
        // right now, and never persisted (see `AgentWireEvent::TaskProgress`).
        // The payload already carries the child's id.
        send_session_event(
            &self.state,
            self.requester_id,
            AgentWireEvent::TaskProgress(progress),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::host_tools::AgentdHostTools;
    use crate::session::test_support::test_state;
    use crossbeam_channel::unbounded;
    use horizon_agent::config::AgentToolsConfig;
    use horizon_agent::contract::{Event, ToolCallId, ToolCallRequest};
    use horizon_agent::live::LiveState;
    use horizon_agent::tools::{
        execute_agent_tool, register_session_runtime, unregister_session_runtime, Execution,
        RecallContext, ToolCompletion, ToolSessionState,
    };
    use std::time::Duration;

    /// A daemon whose only provider entry is named `solo` and has no API
    /// key, so a session started on it answers from the deterministic
    /// fallback responder instead of calling anything.
    fn named_provider_state() -> Arc<crate::session::AgentdState> {
        use horizon_agent::config::{
            AgentConfig, AgentPersistenceConfig, NamedProviderConfig, ProviderKind, ProvidersTable,
            RigAgentConfig,
        };
        use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
        use horizon_agent::registry::ProviderRegistry;

        let agent_config = AgentConfig {
            auxiliary: None,
            rig: RigAgentConfig {
                api_key_present: false,
                model: "m-solo".to_string(),
                ..Default::default()
            },
            providers: ProvidersTable {
                entries: vec![NamedProviderConfig {
                    name: "solo".to_string(),
                    kind: ProviderKind::OpenAiCompatible,
                    base_url: None,
                    api_key_env: "HORIZON_TEST_KEY_NEVER_SET".to_string(),
                    api_key_present: false,
                    default_model: Some("m-solo".to_string()),
                }],
                default_name: "solo".to_string(),
            },
            moa: horizon_agent::config::MoaTable::default(),
            persistence: AgentPersistenceConfig {
                event_log_path: std::path::PathBuf::from("/tmp/horizon-moa-host-test-events.jsonl"),
                duckdb_path: None,
            },
            tools: AgentToolsConfig::default(),
        };
        Arc::new(crate::session::AgentdState::new(
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
        ))
    }

    /// A proposer names the `[[providers]]` entry it runs on. An entry the
    /// registry does not know fails before any session is spawned, so the
    /// caller never gets a session id it then has to clean up.
    #[test]
    fn starting_on_an_unknown_provider_fails_without_spawning() {
        let state = test_state();
        let host = AgentdExplorationHost {
            state: state.clone(),
            requester_id: SessionId::new(),
            provider_id: ProviderId("builtin.agent.mock".to_string()),
            workspace_root: None,
        };
        let started = horizon_agent::tools::ExplorationHost::start(
            &host,
            horizon_agent::tools::ExplorationRequest::for_proposer(
                "anything".to_string(),
                "not-configured".to_string(),
                "m".to_string(),
            ),
        );
        let Err(error) = started else {
            panic!("an unconfigured provider must not start a session");
        };
        assert!(error.contains("not-configured"), "{error}");
        assert!(state.sessions.lock().unwrap().is_empty());
    }

    /// A named provider routes the spawned session to that entry's
    /// registry id, and the prompt still reaches it.
    #[test]
    fn starting_on_a_named_provider_routes_the_session_to_that_entry() {
        let state = named_provider_state();
        let host = AgentdExplorationHost {
            state: state.clone(),
            requester_id: SessionId::new(),
            provider_id: ProviderId("builtin.agent.mock".to_string()),
            workspace_root: None,
        };
        let started = horizon_agent::tools::ExplorationHost::start(
            &host,
            horizon_agent::tools::ExplorationRequest::for_proposer(
                "which provider answered?".to_string(),
                "solo".to_string(),
                "m-solo".to_string(),
            ),
        )
        .expect("the configured entry starts");

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut initialization = None;
        let mut answer = None;
        while std::time::Instant::now() < deadline && answer.is_none() {
            let Ok(event) = started.events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            if let Event::MessageCommitted(message) = event {
                match message.role {
                    horizon_agent::contract::MessageRole::Assistant if initialization.is_none() => {
                        initialization = Some(message.text)
                    }
                    horizon_agent::contract::MessageRole::Assistant => answer = Some(message.text),
                    _ => {}
                }
            }
        }
        let initialization = initialization.expect("the session announces its provider");
        assert!(
            initialization.contains("builtin.agent.rig.solo"),
            "{initialization}"
        );
        let answer = answer.expect("the prompt reached the session");
        assert!(answer.contains("which provider answered?"), "{answer}");

        horizon_agent::tools::ExplorationHost::terminate(&host, started.session_id);
    }

    /// A real explore-role session whose read leaves the workspace root.
    /// Nobody is watching it, so the call must resolve as an error result
    /// the model can read rather than an approval prompt: the turn carries
    /// on and the session still delivers a report.
    ///
    /// End to end on the deterministic fallback provider (no API key, so no
    /// network): the `fs.read path:` line drives a genuine `fs.read`
    /// through the tool pipeline, and the rig turn loop folds whatever
    /// result comes back.
    #[test]
    fn an_unattended_out_of_root_read_is_refused_and_the_session_still_reports() {
        let workspace = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "horizon-unattended-explore-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let outside = workspace.parent().unwrap().join(format!(
            "horizon-unattended-outside-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("elsewhere.txt");
        std::fs::write(&outside_file, "not yours\n").unwrap();

        let state = crate::session::test_support::state_with_rig_config(false, "m-fallback");
        let host = AgentdExplorationHost {
            state: state.clone(),
            requester_id: SessionId::new(),
            provider_id: lock_unpoisoned(&state.providers).default_provider_id(),
            workspace_root: Some(workspace.clone()),
        };
        let started = horizon_agent::tools::ExplorationHost::start(
            &host,
            horizon_agent::tools::ExplorationRequest::for_prompt(format!(
                "fs.read path: {}",
                outside_file.display()
            )),
        )
        .expect("the explore session starts");

        let mut collected = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            let Ok(event) = started.events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            let done = matches!(event, Event::TurnEnded(_));
            collected.push(event);
            if done {
                break;
            }
        }

        assert!(
            collected
                .iter()
                .any(|event| matches!(event, Event::ToolCallRequested(request) if request.tool_id == "fs.read")),
            "the session must have attempted the read: {collected:?}"
        );
        assert!(
            !collected
                .iter()
                .any(|event| matches!(event, Event::ApprovalRequested(_))),
            "nobody could answer a prompt here: {collected:?}"
        );
        let refusal = collected
            .iter()
            .find_map(|event| match event {
                Event::ToolCallFinished(result) => Some(result.clone()),
                _ => None,
            })
            .expect("the read resolves with a result of its own");
        assert!(refusal.is_error());
        assert!(
            refusal.output["message"]
                .as_str()
                .unwrap()
                .contains(&workspace.display().to_string()),
            "the refusal must name the root the session may read: {:?}",
            refusal.output
        );
        assert!(
            collected
                .iter()
                .any(|event| matches!(event, Event::TurnEnded(reason) if *reason == horizon_agent::contract::TurnEndReason::Completed)),
            "the turn must finish rather than park: {collected:?}"
        );
        let report = collected
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::MessageCommitted(message)
                    if message.role == horizon_agent::contract::MessageRole::Assistant =>
                {
                    Some(message.text.clone())
                }
                _ => None,
            })
            .expect("the session still delivers a final message");
        assert!(!report.trim().is_empty(), "{report}");

        horizon_agent::tools::ExplorationHost::terminate(&host, started.session_id);
        std::fs::remove_dir_all(workspace).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    fn call(
        state: &Arc<crate::session::AgentdState>,
        tool_state: &ToolSessionState,
        requester_id: SessionId,
        call_id: &str,
        tool_id: &str,
        input: serde_json::Value,
    ) -> serde_json::Value {
        let execution = execute_agent_tool(
            &AgentdHostTools {
                state: state.clone(),
            },
            tool_state,
            requester_id,
            &LiveState::with_disabled_persistence(),
            &ToolCallRequest {
                call_id: ToolCallId(call_id.to_string()),
                tool_id: tool_id.to_string(),
                input: input.into(),
                occurrence_id: horizon_agent::contract::OccurrenceId(
                    (ToolCallId(call_id.to_string())).0.clone(),
                ),
            },
        );
        let Ok(Execution::Applied(horizon_agent::tools::ToolUpdate::Finished { events, .. })) =
            execution
        else {
            panic!("`{tool_id}` resolves synchronously, got {execution:?}")
        };
        events
            .into_iter()
            .find_map(|event| match event {
                Event::ToolCallFinished(result) => Some(result.output.0),
                _ => None,
            })
            .expect("a ToolCallFinished event")
    }

    /// The whole `task` seam against the *real* daemon implementation
    /// rather than a stub: a `task` call spawns a genuine peer session
    /// here, its user message reaches that session's provider, its events
    /// come back through the [`crate::session::subscription`] seam, the
    /// session is shut down as soon as its own turn ends, and the report is
    /// then fetchable with `task_output`.
    ///
    /// The launch itself is asynchronous since 2026-07-28
    /// (`docs/agent-async-task-design.md`): the call returns a `started`
    /// receipt at once and nothing lands on `async_results` at all.
    /// Hermetic -- the child runs on the mock provider, so no network and
    /// no event log are involved.
    #[test]
    fn task_spawns_a_real_peer_session_and_terminates_it_when_it_finishes() {
        let state = test_state();
        let requester_id = SessionId::new();
        let (results_tx, results_rx) = unbounded::<ToolCompletion>();
        let host: Arc<dyn horizon_agent::tools::ExplorationHost> =
            Arc::new(AgentdExplorationHost {
                state: state.clone(),
                requester_id,
                provider_id: ProviderId("builtin.agent.mock".to_string()),
                workspace_root: None,
            });
        let tool_state = horizon_agent::tools::ToolSessionBuilder::for_current_dir(
            AgentToolsConfig::default(),
            RecallContext::default(),
        )
        .with_exploration_host(Some(host))
        .build();
        let live_state = LiveState::with_disabled_persistence();
        register_session_runtime(
            requester_id,
            tool_state.clone(),
            live_state.clone(),
            results_tx,
        );

        let launched = call(
            &state,
            &tool_state,
            requester_id,
            "task-e2e",
            "task",
            serde_json::json!({
                "description": "find the emit site",
                "prompt": "where is the emit site?",
            }),
        );
        assert_eq!(
            launched["status"],
            serde_json::json!("started"),
            "the launch must not block the requester's turn: {launched}"
        );
        let child_id = SessionId::from_uuid(
            launched["session_id"]
                .as_str()
                .expect("the spawned session id")
                .parse()
                .expect("a uuid"),
        );
        assert_ne!(child_id, requester_id);

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while state.sessions.lock().unwrap().contains_key(&child_id)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !state.sessions.lock().unwrap().contains_key(&child_id),
            "the task session must be terminated as soon as its own turn ends"
        );
        assert!(
            !state.has_subscriber(child_id),
            "its event subscription must be released with it"
        );
        assert!(
            results_rx.try_recv().is_err(),
            "an asynchronous launch delivers nothing on the tool-completion channel"
        );

        let fetched = call(
            &state,
            &tool_state,
            requester_id,
            "task-output-e2e",
            "task_output",
            serde_json::json!({ "session_id": child_id.as_uuid().to_string() }),
        );
        assert_eq!(
            fetched["status"],
            serde_json::json!("finished"),
            "{fetched}"
        );
        let report = fetched["report"].as_str().expect("a report");
        assert!(
            report.contains("where is the emit site?"),
            "the task must have answered the forwarded prompt, got: {report}"
        );

        unregister_session_runtime(requester_id);
    }
}
