//! `horizon-agent`'s `task` seam, implemented against this
//! daemon's own session hosting.

use std::path::PathBuf;
use std::sync::Arc;

use horizon_agent::contract::{Command, ProviderId, SessionId};
use horizon_agent::roles::RoleId;

use super::spawn::spawn_session_thread;
use super::state::SessiondState;

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
pub(super) struct SessiondExplorationHost {
    pub(super) state: Arc<SessiondState>,
    /// The requesting session's provider, so an exploration is answered by
    /// the same model family the requester is talking to.
    pub(super) provider_id: ProviderId,
    /// The requesting session's own resolved root -- post-isolation, so an
    /// isolated requester's exploration reads that worktree and not the
    /// daemon's cwd.
    pub(super) workspace_root: Option<PathBuf>,
}

impl horizon_agent::tools::ExplorationHost for SessiondExplorationHost {
    fn start(&self, prompt: String) -> Result<horizon_agent::tools::StartedExploration, String> {
        let session_id = SessionId::new();
        // Subscribe first, spawn second -- the ordering requirement
        // `super::subscription` documents: the subscription has to exist
        // before the session's thread can emit anything.
        let subscription = self.state.subscribe_to_session(session_id);
        spawn_session_thread(
            self.state.clone(),
            session_id,
            self.provider_id.clone(),
            Some(RoleId(horizon_agent::roles::EXPLORE_ROLE_ID.to_string())),
            self.workspace_root.clone(),
            None,
            false,
            None,
            Vec::new(),
        );
        if !self
            .state
            .send_command(session_id, Command::UserMessage { text: prompt })
        {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::host_tools::SessiondHostTools;
    use crate::session::test_support::judge_test_state;
    use crossbeam_channel::unbounded;
    use horizon_agent::config::AgentToolsConfig;
    use horizon_agent::contract::{Event, ToolCallId, ToolCallRequest};
    use horizon_agent::live::LiveState;
    use horizon_agent::tools::{
        execute_agent_tool, register_session_runtime, unregister_session_runtime, Execution,
        RecallContext, ToolCompletion, ToolSessionState,
    };
    use std::time::Duration;

    fn call(
        state: &Arc<crate::session::SessiondState>,
        tool_state: &ToolSessionState,
        requester_id: SessionId,
        call_id: &str,
        tool_id: &str,
        input: serde_json::Value,
    ) -> serde_json::Value {
        let execution = execute_agent_tool(
            &SessiondHostTools {
                state: state.clone(),
            },
            tool_state,
            requester_id,
            &ToolCallRequest {
                call_id: ToolCallId(call_id.to_string()),
                tool_id: tool_id.to_string(),
                input: input.into(),

                occurrence_id: None,
            },
        );
        let Execution::Auto(events) = execution else {
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
        let state = judge_test_state();
        let requester_id = SessionId::new();
        let (results_tx, results_rx) = unbounded::<ToolCompletion>();
        let host: Arc<dyn horizon_agent::tools::ExplorationHost> =
            Arc::new(SessiondExplorationHost {
                state: state.clone(),
                provider_id: ProviderId("builtin.agent.mock".to_string()),
                workspace_root: None,
            });
        let tool_state = ToolSessionState::for_current_dir(
            AgentToolsConfig::default(),
            RecallContext::default(),
        )
        .with_exploration_host(Some(host));
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
