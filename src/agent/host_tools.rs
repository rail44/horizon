//! Horizon's implementation of the agent crate's [`HostTools`] seam
//! (`horizon_agent::tools::HostTools`): the one tool (`workspace.snapshot`)
//! that has to run on Horizon's side because it reads [`Workspace`], a type
//! `horizon-agent` can't depend on — see
//! `docs/agent-runtime-split-design.md`'s "Tools execute in the child"
//! guardrail, which calls tools like this one "host tools".

use serde_json::json;

use crate::agent::tools::HostTools;
use crate::workspace::Workspace;

/// Wraps a `&Workspace` so it can be passed wherever the agent crate wants
/// a `&dyn HostTools` — see `app/runtime/agent.rs::spawn_agent_session`'s
/// `create_effect` for the call site.
pub(crate) struct WorkspaceHostTools<'a>(pub(crate) &'a Workspace);

impl HostTools for WorkspaceHostTools<'_> {
    fn execute_auto(&self, tool_id: &str, _input: &serde_json::Value) -> Option<serde_json::Value> {
        match tool_id {
            "workspace.snapshot" => Some(workspace_snapshot(self.0)),
            _ => None,
        }
    }
}

pub(crate) fn workspace_snapshot(workspace: &Workspace) -> serde_json::Value {
    json!({
        "tab_count": workspace.tab_count(),
        "detached_session_count": workspace.detached_session_count(),
        "active_title": workspace.active_title(),
        "active_visible_index": workspace.active_visible_index(),
        "tabs": workspace
            .tab_summaries()
            .into_iter()
            .map(|tab| json!({
                "index": tab.index,
                "title": tab.title,
                "active": tab.active,
                "pane_count": tab.pane_count,
                "active_session_id": tab.active_session_id.map(|id| format!("{id:?}")),
            }))
            .collect::<Vec<_>>(),
        "panes": workspace
            .pane_summaries()
            .into_iter()
            .map(|pane| json!({
                "tab_index": pane.tab_index,
                "pane_index": pane.pane_index,
                "title": pane.title,
                "kind": format!("{:?}", pane.kind).to_ascii_lowercase(),
                "active": pane.active,
                "tab_active": pane.tab_active,
            }))
            .collect::<Vec<_>>(),
        "sessions": workspace
            .session_summaries()
            .into_iter()
            .map(|session| json!({
                "id": format!("{:?}", session.id),
                "kind": format!("{:?}", session.kind).to_ascii_lowercase(),
                "display_number": session.display_number,
                "title": session.title,
                "attached": session.attached,
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{Command, Event, ToolCallId, ToolCallRequest};
    use crate::agent::tools::{execute_agent_tool, process_agent_provider_event, Execution};

    fn dummy_tool_state() -> crate::agent::tools::ToolSessionState {
        crate::agent::tools::ToolSessionState::for_current_dir(
            crate::agent::config::AgentToolsConfig::default(),
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
