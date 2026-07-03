use serde_json::json;

use crate::agent::contract::{
    Error, Event, Message, MessageRole, SessionState, ToolCallId, ToolCallRequest, ToolCallResult,
    ToolPermission,
};
use crate::agent::tools::fs;
use crate::agent::tools::permission_for_tool;
use crate::agent::tools::state::ToolSessionState;
use crate::workspace::Workspace;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Execution {
    Auto(Vec<Event>),
    RequiresApproval,
    Denied(Vec<Event>),
    Unknown(Vec<Event>),
}

pub(crate) fn execute_agent_tool(
    workspace: &Workspace,
    tool_state: &ToolSessionState,
    request: &ToolCallRequest,
) -> Execution {
    match permission_for_tool(&request.tool_id) {
        Some(ToolPermission::AutoAllowRead | ToolPermission::AutoAllowUi) => {
            Execution::Auto(execute_auto_tool(workspace, tool_state, request))
        }
        Some(ToolPermission::RequireApproval) => Execution::RequiresApproval,
        Some(ToolPermission::Deny) => Execution::Denied(vec![Event::Error(Error {
            message: format!("Tool `{}` is denied by Horizon policy.", request.tool_id),
        })]),
        None => Execution::Unknown(vec![Event::Error(Error {
            message: format!("Unknown tool `{}`.", request.tool_id),
        })]),
    }
}

fn execute_auto_tool(
    workspace: &Workspace,
    tool_state: &ToolSessionState,
    request: &ToolCallRequest,
) -> Vec<Event> {
    let output = match request.tool_id.as_str() {
        "workspace.snapshot" => workspace_snapshot(workspace),
        _ => match fs::execute_auto(tool_state, &request.tool_id, &request.input) {
            Some(output) => output,
            None => {
                return vec![Event::Error(Error {
                    message: format!(
                        "Tool `{}` cannot be executed automatically.",
                        request.tool_id
                    ),
                })]
            }
        },
    };

    vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
        Event::ToolCallFinished(ToolCallResult {
            call_id: request.call_id.clone(),
            output,
        }),
        Event::StateChanged(SessionState::WaitingForUser),
    ]
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

pub(crate) fn tool_result_message(result: &ToolCallResult) -> Event {
    Event::MessageCommitted(Message {
        role: MessageRole::Assistant,
        text: format!("Tool result received for {}.", result.call_id.0),
    })
}

/// A synthetic tool result marking a pending tool call as cancelled, so a
/// pending approval belonging to a cancelled turn resolves to a terminal
/// (non-error) outcome instead of hanging forever.
pub(crate) fn cancelled_tool_call_result(call_id: ToolCallId) -> ToolCallResult {
    ToolCallResult {
        call_id,
        output: json!({ "cancelled": true }),
    }
}
