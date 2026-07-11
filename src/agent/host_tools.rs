//! Horizon's implementation of the agent crate's [`HostTools`] seam
//! (`horizon_agent::tools::HostTools`): the one tool (`workspace.snapshot`)
//! that has to run on Horizon's side because it reads [`Workspace`], a type
//! `horizon-agent` can't depend on — see
//! `docs/agent-runtime-split-design.md`'s "Tools execute in the child"
//! guardrail, which calls tools like this one "host tools".
//!
//! [`workspace_snapshot`] itself is real production code -- it's what
//! `agent::agentd_runtime::answer_host_tool_request` calls to answer a
//! `host_tool_request` arriving from `horizon-agentd` (the wire-based
//! seam, step 3-4). [`WorkspaceHostTools`], the in-process `HostTools` impl
//! wrapping it, has no production caller left since step 4 retired
//! Horizon's own in-process session loop -- kept `#[cfg(test)]` as a
//! concrete `HostTools` this module's own tests (and nothing else) exercise
//! the seam through.

#[cfg(test)]
use crate::agent::tools::HostTools;
#[cfg(test)]
use crate::workspace::Workspace;

/// Wraps a `&Workspace` so it can be passed wherever the agent crate wants
/// a `&dyn HostTools` — see the module doc for why this is test-only now.
#[cfg(test)]
pub(crate) struct WorkspaceHostTools<'a>(pub(crate) &'a Workspace);

#[cfg(test)]
impl HostTools for WorkspaceHostTools<'_> {
    fn execute_auto(&self, tool_id: &str, _input: &serde_json::Value) -> Option<serde_json::Value> {
        match tool_id {
            "workspace.snapshot" => Some(workspace_snapshot(self.0)),
            _ => None,
        }
    }
}

// The snapshot payload itself moved to horizon-workspace::snapshot
// (shared with shell-gpui); kept as a thin alias for existing callers.
pub(crate) use horizon_workspace::snapshot::workspace_snapshot;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{Command, Event, ToolCallId, ToolCallRequest};
    use crate::agent::tools::{execute_agent_tool, process_agent_provider_event, Execution};
    use serde_json::json;

    fn dummy_tool_state() -> crate::agent::tools::ToolSessionState {
        crate::agent::tools::ToolSessionState::for_current_dir(
            crate::agent::config::AgentToolsConfig::default(),
            crate::agent::tools::RecallContext::default(),
        )
    }

    #[test]
    fn workspace_snapshot_includes_core_workspace_state() {
        let workspace = Workspace::mvp();
        let snapshot = workspace_snapshot(&workspace);

        assert_eq!(snapshot["tab_count"], 1);
        assert_eq!(snapshot["active_title"], "Terminal #1");
        assert_eq!(snapshot["tabs"][0]["title"], "Terminal #1");
    }

    #[test]
    fn execute_workspace_snapshot_returns_tool_result_events() {
        let workspace = Workspace::mvp();
        let tool_state = dummy_tool_state();
        let request = ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "workspace.snapshot".to_string(),
            input: json!({}),
        };

        let Execution::Auto(events) =
            execute_agent_tool(&WorkspaceHostTools(&workspace), &tool_state, &request)
        else {
            panic!("workspace snapshot should auto execute");
        };

        assert!(events.iter().any(|event| {
            matches!(
                event,
                Event::ToolCallFinished(result) if result.call_id == request.call_id
            )
        }));
    }

    #[test]
    fn processing_workspace_snapshot_returns_result_command_to_provider() {
        let workspace = Workspace::mvp();
        let tool_state = dummy_tool_state();
        let call_id = ToolCallId("call-1".to_string());
        let processing = process_agent_provider_event(
            &WorkspaceHostTools(&workspace),
            &tool_state,
            Event::ToolCallRequested(ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "workspace.snapshot".to_string(),
                input: json!({}),
            }),
        );

        assert!(processing.horizon_events.iter().any(|provider_event| {
            matches!(
                &provider_event.event,
                Event::ToolCallFinished(result) if result.call_id == call_id
            )
        }));
        assert!(processing.provider_commands.iter().any(|command| {
            matches!(
                command,
                Command::ToolCallResult(result) if result.call_id == call_id
            )
        }));
    }
}
