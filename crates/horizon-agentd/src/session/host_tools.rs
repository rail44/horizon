//! The agent (child) side of the host-tool channel: the round trip that
//! blocks a session thread while Horizon answers a `host_tool_request`.

use std::sync::Arc;
use std::time::Duration;

use horizon_agent::contract::{self, Command, SessionId};
use horizon_agent::hosting::HostToolRequest;
use horizon_agent::tools::HostTools;

use super::state::AgentdState;

/// How long a session thread waits for Horizon to answer a `host_tool_*`
/// round trip before giving up. Generous but finite: a client that never
/// answers must not hang a session forever.
pub(crate) const HOST_TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// Sends a host-tool request through the current connection's
/// `_horizon/host_tool` bridge, if a connection is live. Returns whether
/// the send was actually accepted, for the one caller
/// ([`AgentdHostTools::execute_auto`]) that needs to fail fast rather
/// than wait out its full timeout when nothing is listening.
fn send_host_tool_request(state: &AgentdState, request: HostToolRequest) -> bool {
    match state.host_tools_outgoing.lock().unwrap().as_ref() {
        Some(tx) => tx.send(request).is_ok(),
        None => false,
    }
}

/// The agent (child) side of the host-tool channel (guardrail 4 in
/// `docs/agent-runtime-split-design.md`): sends a `host_tool_request` over
/// the current connection (if any -- see [`send_host_tool_request`];
/// connection-global, exactly as the JSONL envelope's receiver treated it) and blocks this
/// session's dedicated thread on the matching `host_tool_response`. Only
/// `workspace.snapshot` is ever routed here today (the same tool id
/// Horizon's own `agent::host_tools::WorkspaceHostTools` answers
/// in-process) -- everything else falls through to `None`, letting
/// `execute_agent_tool` try the crate's own `tools::fs` auto tools next.
pub(super) struct AgentdHostTools {
    pub(super) state: Arc<AgentdState>,
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

        let request = HostToolRequest {
            request_id: contract::RequestId(request_id.clone()),
            tool_id: tool_id.to_string(),
            input: input.clone().into(),
        };
        if !send_host_tool_request(&self.state, request) {
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
        response.map(|response| response.output.0)
    }

    /// `worktree.enter`: the activation request the board's `implement`
    /// action sends, with the base pinned to the session root's current
    /// `HEAD` commit. `SessionEnvironment::activate` repeats the ownership
    /// check when the provider reaches its turn boundary.
    fn enter_worktree(&self, session: SessionId) -> Result<String, String> {
        let (root, owns_worktree) = self
            .state
            .session_directory(session)
            .ok_or("Session has no repository root")?;
        if owns_worktree {
            return Err(format!(
                "This session already owns a worktree at {}",
                root.display()
            ));
        }
        let base = crate::worktree::resolve_commit(&root, "HEAD")?;
        if !self
            .state
            .send_command(session, Command::ActivateWorktree { base: base.clone() })
        {
            return Err("Session stopped before environment activation".into());
        }
        Ok(base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::state::SessionEntry;
    use horizon_agent::config::AgentToolsConfig;
    use horizon_agent::contract::{Event, OccurrenceId, ProviderId, ToolCallId, ToolCallRequest};
    use horizon_agent::live::LiveState;
    use horizon_agent::tools::{
        execute_agent_tool, Execution, RecallContext, ToolSessionBuilder, ToolUpdate,
    };
    use std::path::Path;

    /// Runs one `worktree.enter` call through the tool executor, returning
    /// the recorded result's outcome flag and JSON body.
    fn enter(state: &Arc<AgentdState>, session: SessionId) -> (bool, serde_json::Value) {
        let tool_state = ToolSessionBuilder::for_current_dir(
            AgentToolsConfig::default(),
            RecallContext::default(),
        )
        .build();
        let execution = execute_agent_tool(
            &AgentdHostTools {
                state: state.clone(),
            },
            &tool_state,
            session,
            &LiveState::with_disabled_persistence(),
            &ToolCallRequest {
                call_id: ToolCallId("enter-1".into()),
                occurrence_id: OccurrenceId::new(),
                tool_id: "worktree.enter".into(),
                input: serde_json::json!({}).into(),
            },
        );
        let Ok(Execution::Applied(ToolUpdate::Finished { events, result })) = execution else {
            panic!("worktree.enter resolves synchronously, got {execution:?}");
        };
        assert!(events
            .iter()
            .any(|event| matches!(event, Event::ToolCallStarted(_))));
        (result.is_error(), result.output.0)
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(dir).args(args);
        for (key, _) in std::env::vars() {
            if key.starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command.env("GIT_CONFIG_GLOBAL", "/dev/null");
        command.env("GIT_CONFIG_SYSTEM", "/dev/null");
        let output = command.output().unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn repository() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
        git(repo.path(), &["add", "README.md"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
        );
        repo
    }

    fn insert_session(
        state: &AgentdState,
        root: &Path,
        worktree: Option<crate::worktree::WorktreeInfo>,
    ) -> (SessionId, crossbeam_channel::Receiver<Command>) {
        let session = SessionId::new();
        let (inbound, inbound_rx) = crossbeam_channel::unbounded();
        let (replay, _) = crossbeam_channel::unbounded();
        state.sessions.lock().unwrap().insert(
            session,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.mock".into()),
                role_id: None,
                model: None,
                selection: None,
                inbound,
                replay,
                parent_session_id: None,
                spawn_source_session_id: None,
                workspace_root: Some(root.to_path_buf()),
                worktree,
            },
        );
        (session, inbound_rx)
    }

    #[test]
    fn enter_worktree_requests_activation_at_the_resolved_head_commit() {
        let repo = repository();
        let state = super::super::test_support::test_state();
        let (session, inbound) = insert_session(&state, repo.path(), None);

        let (failed, output) = enter(&state, session);

        assert!(!failed, "{output}");
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        assert_eq!(head.len(), 40, "{head}");
        assert_eq!(output["requested"], true);
        assert_eq!(output["base"], head.as_str());
        assert_eq!(
            inbound.try_recv().unwrap(),
            Command::ActivateWorktree { base: head }
        );
        assert!(inbound.try_recv().is_err());
    }

    #[test]
    fn enter_worktree_refuses_a_session_that_already_owns_one() {
        let repo = repository();
        let state = super::super::test_support::test_state();
        let owned = crate::worktree::WorktreeInfo {
            repo_root: repo.path().to_path_buf(),
            path: repo.path().join(".horizon/worktrees/owned"),
            branch: "horizon/owned".into(),
            base: "0".repeat(40),
        };
        let (session, inbound) = insert_session(&state, &owned.path.clone(), Some(owned));

        let (failed, output) = enter(&state, session);

        assert!(failed, "{output}");
        assert!(
            output["message"]
                .as_str()
                .unwrap()
                .contains("already owns a worktree"),
            "{output}"
        );
        assert!(inbound.try_recv().is_err());
    }
}
