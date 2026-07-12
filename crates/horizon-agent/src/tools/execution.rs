use serde_json::json;

use crate::contract::{
    Error, Event, Message, MessageRole, SessionState, ToolCallId, ToolCallRequest, ToolCallResult,
    ToolPermission,
};
use crate::tools::config;
use crate::tools::fs;
use crate::tools::permission_for_tool;
use crate::tools::recall;
use crate::tools::state::ToolSessionState;

/// Seam for tools this crate doesn't implement itself because they need
/// Horizon-side state this crate can't depend on — currently just
/// `workspace.snapshot`, which reads Horizon's `Workspace`. Horizon
/// implements this trait (see `agent::host_tools::WorkspaceHostTools`) and
/// passes it in at every call into [`execute_agent_tool`]/
/// `processing::process_agent_provider_event`, keeping the tool catalog's
/// shape otherwise unchanged: an unrecognized `tool_id` here just falls
/// through to this crate's own auto-allow tools (`tools::fs`). See
/// `docs/agent-runtime-split-design.md`'s "Tools execute in the child"
/// guardrail — this is the seam that will grow into the host-tool channel
/// once tool execution moves into `horizon-sessiond`.
pub trait HostTools {
    /// Executes a host-owned auto-allow tool, returning `None` if `tool_id`
    /// isn't one this implementation handles.
    fn execute_auto(&self, tool_id: &str, input: &serde_json::Value) -> Option<serde_json::Value>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Execution {
    Auto(Vec<Event>),
    RequiresApproval,
    Denied(Vec<Event>),
    Unknown(Vec<Event>),
}

pub fn execute_agent_tool(
    host: &dyn HostTools,
    tool_state: &ToolSessionState,
    request: &ToolCallRequest,
) -> Execution {
    match permission_for_tool(&request.tool_id) {
        Some(ToolPermission::AutoAllowRead | ToolPermission::AutoAllowUi) => {
            Execution::Auto(execute_auto_tool(host, tool_state, request))
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
    host: &dyn HostTools,
    tool_state: &ToolSessionState,
    request: &ToolCallRequest,
) -> Vec<Event> {
    let output = host
        .execute_auto(&request.tool_id, &request.input)
        .or_else(|| fs::execute_auto(tool_state, &request.tool_id, &request.input))
        .or_else(|| config::execute_auto(tool_state, &request.tool_id, &request.input))
        .or_else(|| recall::execute_auto(tool_state, &request.tool_id, &request.input));
    let output = match output {
        Some(output) => output,
        None => {
            return vec![Event::Error(Error {
                message: format!(
                    "Tool `{}` cannot be executed automatically.",
                    request.tool_id
                ),
            })]
        }
    };

    vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
        Event::ToolCallFinished(ToolCallResult::new(request.call_id.clone(), output)),
        // No `StateChanged(WaitingForUser)` here: this call is only one
        // member of whatever batch the originating completion requested (a
        // single completion can request several parallel tool calls — see
        // `providers::rig::session::fold_batched_tool_result`), and this
        // executor has no visibility into whether sibling calls are still
        // outstanding or a turn is still in flight. The session loop owns
        // turn-level state and already emits its own accurate
        // `WaitingForUser` once the whole batch has resolved and no
        // follow-up turn is running; emitting it here too, per call, raced
        // ahead of that (see the production incident this fix responds to)
        // and could flip `AgentFrame::is_turn_in_flight` to `false` —
        // disabling Cancel — while more results are still outstanding.
    ]
}

pub fn tool_result_message(result: &ToolCallResult) -> Event {
    Event::MessageCommitted(Message {
        role: MessageRole::Assistant,
        text: format!("Tool result received for {}.", result.call_id.0),
    })
}

/// A synthetic tool result marking a pending tool call as cancelled, so a
/// pending approval belonging to a cancelled turn resolves to a terminal
/// (non-error) outcome instead of hanging forever.
pub fn cancelled_tool_call_result(call_id: ToolCallId) -> ToolCallResult {
    ToolCallResult::new(call_id, json!({ "cancelled": true }))
}
