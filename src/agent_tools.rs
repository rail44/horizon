use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::{
    horizon_events_for_provider_event, AgentCommand, AgentError, AgentEvent, AgentMessage,
    AgentMessageRole, AgentProviderEvent, AgentSessionState, AgentToolCallRequest,
    AgentToolCallResult, AgentToolPermission,
};
use crate::workspace::Workspace;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentToolDefinition {
    pub id: String,
    pub title: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub permission: AgentToolPermission,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentToolExecution {
    Auto(Vec<AgentEvent>),
    RequiresApproval,
    Denied(Vec<AgentEvent>),
    Unknown(Vec<AgentEvent>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentEventProcessing {
    pub horizon_events: Vec<AgentProviderEvent>,
    pub provider_commands: Vec<AgentCommand>,
}

pub fn agent_tool_definitions() -> Vec<AgentToolDefinition> {
    vec![
        AgentToolDefinition {
            id: "workspace.snapshot".to_string(),
            title: "Workspace Snapshot".to_string(),
            description: "Read tabs, panes, sessions, and active workspace state.".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            permission: AgentToolPermission::AutoAllowRead,
        },
        AgentToolDefinition {
            id: "mock.approval_required".to_string(),
            title: "Mock Approval Required".to_string(),
            description: "Test tool that exercises the approval flow.".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": true
            }),
            permission: AgentToolPermission::RequireApproval,
        },
    ]
}

pub fn permission_for_tool(tool_id: &str) -> Option<AgentToolPermission> {
    agent_tool_definitions()
        .into_iter()
        .find(|definition| definition.id == tool_id)
        .map(|definition| definition.permission)
}

pub fn execute_agent_tool(
    workspace: &Workspace,
    request: &AgentToolCallRequest,
) -> AgentToolExecution {
    match permission_for_tool(&request.tool_id) {
        Some(AgentToolPermission::AutoAllowRead | AgentToolPermission::AutoAllowUi) => {
            AgentToolExecution::Auto(execute_auto_tool(workspace, request))
        }
        Some(AgentToolPermission::RequireApproval) => AgentToolExecution::RequiresApproval,
        Some(AgentToolPermission::Deny) => {
            AgentToolExecution::Denied(vec![AgentEvent::Error(AgentError {
                message: format!("Tool `{}` is denied by Horizon policy.", request.tool_id),
            })])
        }
        None => AgentToolExecution::Unknown(vec![AgentEvent::Error(AgentError {
            message: format!("Unknown tool `{}`.", request.tool_id),
        })]),
    }
}

pub fn process_agent_provider_event(
    workspace: &Workspace,
    provider_event: impl Into<AgentProviderEvent>,
) -> AgentEventProcessing {
    let provider_event = provider_event.into();
    let event = provider_event.event.clone();
    let mut horizon_events = horizon_events_for_provider_event(&event)
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            if index == 0 {
                AgentProviderEvent {
                    event,
                    provider_payload: provider_event.provider_payload.clone(),
                }
            } else {
                event.into()
            }
        })
        .collect::<Vec<_>>();
    let mut provider_commands = Vec::new();

    if let AgentEvent::ToolCallRequested(request) = &event {
        match execute_agent_tool(workspace, request) {
            AgentToolExecution::Auto(events) => {
                for result_event in &events {
                    if let AgentEvent::ToolCallFinished(result) = result_event {
                        provider_commands.push(AgentCommand::ToolCallResult(result.clone()));
                    }
                }
                horizon_events.extend(events.into_iter().map(AgentProviderEvent::from));
            }
            AgentToolExecution::RequiresApproval => {}
            AgentToolExecution::Denied(events) | AgentToolExecution::Unknown(events) => {
                horizon_events.extend(events.into_iter().map(AgentProviderEvent::from));
            }
        }
    }

    AgentEventProcessing {
        horizon_events,
        provider_commands,
    }
}

fn execute_auto_tool(workspace: &Workspace, request: &AgentToolCallRequest) -> Vec<AgentEvent> {
    match request.tool_id.as_str() {
        "workspace.snapshot" => vec![
            AgentEvent::StateChanged(AgentSessionState::ToolRunning),
            AgentEvent::ToolCallStarted(request.call_id.clone()),
            AgentEvent::ToolCallFinished(AgentToolCallResult {
                call_id: request.call_id.clone(),
                output: workspace_snapshot(workspace),
            }),
            AgentEvent::StateChanged(AgentSessionState::WaitingForUser),
        ],
        _ => vec![AgentEvent::Error(AgentError {
            message: format!(
                "Tool `{}` cannot be executed automatically.",
                request.tool_id
            ),
        })],
    }
}

pub fn workspace_snapshot(workspace: &Workspace) -> serde_json::Value {
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

pub fn tool_result_message(result: &AgentToolCallResult) -> AgentEvent {
    AgentEvent::MessageCommitted(AgentMessage {
        role: AgentMessageRole::Assistant,
        text: format!("Tool result received for {}.", result.call_id.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentToolCallId;

    #[test]
    fn workspace_snapshot_tool_is_read_only_auto_allow() {
        assert_eq!(
            permission_for_tool("workspace.snapshot"),
            Some(AgentToolPermission::AutoAllowRead)
        );
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
        let request = AgentToolCallRequest {
            call_id: AgentToolCallId("call-1".to_string()),
            tool_id: "workspace.snapshot".to_string(),
            input: json!({}),
        };

        let AgentToolExecution::Auto(events) = execute_agent_tool(&workspace, &request) else {
            panic!("workspace snapshot should auto execute");
        };

        assert!(events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::ToolCallFinished(result) if result.call_id == request.call_id
            )
        }));
    }

    #[test]
    fn processing_workspace_snapshot_returns_result_command_to_provider() {
        let workspace = Workspace::mvp();
        let call_id = AgentToolCallId("call-1".to_string());
        let processing = process_agent_provider_event(
            &workspace,
            AgentEvent::ToolCallRequested(AgentToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "workspace.snapshot".to_string(),
                input: json!({}),
            }),
        );

        assert!(processing.horizon_events.iter().any(|provider_event| {
            matches!(
                &provider_event.event,
                AgentEvent::ToolCallFinished(result) if result.call_id == call_id
            )
        }));
        assert!(processing.provider_commands.iter().any(|command| {
            matches!(
                command,
                AgentCommand::ToolCallResult(result) if result.call_id == call_id
            )
        }));
    }

    #[test]
    fn processing_preserves_provider_payload_on_original_event_only() {
        let workspace = Workspace::mvp();
        let call_id = AgentToolCallId("call-1".to_string());
        let payload = json!({ "provider": "rig", "version": 1 });
        let processing = process_agent_provider_event(
            &workspace,
            AgentProviderEvent::with_provider_payload(
                AgentEvent::ToolCallRequested(AgentToolCallRequest {
                    call_id: call_id.clone(),
                    tool_id: "workspace.snapshot".to_string(),
                    input: json!({}),
                }),
                payload.clone(),
            ),
        );

        assert_eq!(processing.horizon_events[0].provider_payload, Some(payload));
        assert!(processing
            .horizon_events
            .iter()
            .skip(1)
            .all(|event| { event.provider_payload.is_none() }));
    }
}
