use serde_json::json;

use super::*;
use crate::agent::contract::{
    Command, Event, ProviderEvent, ToolCallId, ToolCallRequest, ToolPermission,
};
use crate::agent::tools::execution::{execute_agent_tool, workspace_snapshot};
use crate::workspace::Workspace;

#[test]
fn workspace_snapshot_tool_is_read_only_auto_allow() {
    assert_eq!(
        permission_for_tool("workspace.snapshot"),
        Some(ToolPermission::AutoAllowRead)
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
    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "workspace.snapshot".to_string(),
        input: json!({}),
    };

    let Execution::Auto(events) = execute_agent_tool(&workspace, &request) else {
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
    let call_id = ToolCallId("call-1".to_string());
    let processing = process_agent_provider_event(
        &workspace,
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

#[test]
fn processing_preserves_provider_payload_on_original_event_only() {
    let workspace = Workspace::mvp();
    let call_id = ToolCallId("call-1".to_string());
    let payload = json!({ "provider": "rig", "version": 1 });
    let processing = process_agent_provider_event(
        &workspace,
        ProviderEvent::with_provider_payload(
            Event::ToolCallRequested(ToolCallRequest {
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
