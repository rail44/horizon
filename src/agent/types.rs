use serde::{Deserialize, Serialize};

use crate::workspace::SessionId;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct AgentProviderId(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct AgentRequestId(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct AgentToolCallId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct StartAgentSession {
    pub session_id: SessionId,
    pub provider_id: AgentProviderId,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentCommand {
    Initialize(AgentInitialization),
    UserMessage {
        text: String,
    },
    Cancel {
        request_id: Option<AgentRequestId>,
    },
    ApproveToolCall {
        call_id: AgentToolCallId,
    },
    DenyToolCall {
        call_id: AgentToolCallId,
        reason: Option<String>,
    },
    ToolCallResult(AgentToolCallResult),
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentInitialization {
    pub session_id: SessionId,
    pub provider_id: AgentProviderId,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentEvent {
    StateChanged(AgentSessionState),
    ReasoningDelta(AgentMessageDelta),
    AssistantTextDelta(AgentMessageDelta),
    MessageCommitted(AgentMessage),
    ToolCallRequested(AgentToolCallRequest),
    ToolCallStarted(AgentToolCallId),
    ToolCallFinished(AgentToolCallResult),
    ApprovalRequested(AgentApprovalRequest),
    Error(AgentError),
    Exited(AgentExit),
}

pub fn agent_event_kind(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::StateChanged(_) => "state_changed",
        AgentEvent::ReasoningDelta(_) => "reasoning_delta",
        AgentEvent::AssistantTextDelta(_) => "assistant_text_delta",
        AgentEvent::MessageCommitted(_) => "message_committed",
        AgentEvent::ToolCallRequested(_) => "tool_call_requested",
        AgentEvent::ToolCallStarted(_) => "tool_call_started",
        AgentEvent::ToolCallFinished(_) => "tool_call_finished",
        AgentEvent::ApprovalRequested(_) => "approval_requested",
        AgentEvent::Error(_) => "error",
        AgentEvent::Exited(_) => "exited",
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentProviderEvent {
    pub event: AgentEvent,
    pub provider_payload: Option<serde_json::Value>,
}

impl AgentProviderEvent {
    pub fn new(event: AgentEvent) -> Self {
        Self {
            event,
            provider_payload: None,
        }
    }

    pub fn with_provider_payload(event: AgentEvent, provider_payload: serde_json::Value) -> Self {
        Self {
            event,
            provider_payload: Some(provider_payload),
        }
    }
}

impl From<AgentEvent> for AgentProviderEvent {
    fn from(event: AgentEvent) -> Self {
        Self::new(event)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentSessionState {
    Created,
    Running,
    WaitingForUser,
    WaitingForApproval,
    ToolRunning,
    Completed,
    Failed,
    Terminated,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentMessage {
    pub role: AgentMessageRole,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentMessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentMessageDelta {
    pub role: AgentMessageRole,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentToolCallRequest {
    pub call_id: AgentToolCallId,
    pub tool_id: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentToolCallResult {
    pub call_id: AgentToolCallId,
    pub output: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentApprovalRequest {
    pub call_id: AgentToolCallId,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AgentToolPermission {
    AutoAllowRead,
    AutoAllowUi,
    RequireApproval,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentError {
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AgentExit {
    pub reason: String,
}

pub fn horizon_events_for_provider_event(event: &AgentEvent) -> Vec<AgentEvent> {
    let mut events = vec![event.clone()];
    if let AgentEvent::ToolCallRequested(request) = event {
        match crate::agent::tools::permission_for_tool(&request.tool_id)
            .unwrap_or(AgentToolPermission::RequireApproval)
        {
            AgentToolPermission::AutoAllowRead | AgentToolPermission::AutoAllowUi => {}
            AgentToolPermission::RequireApproval => {
                events.push(AgentEvent::ApprovalRequested(AgentApprovalRequest {
                    call_id: request.call_id.clone(),
                    reason: format!(
                        "`{}` requested Horizon approval for this tool call.",
                        request.tool_id
                    ),
                }));
                events.push(AgentEvent::StateChanged(
                    AgentSessionState::WaitingForApproval,
                ));
            }
            AgentToolPermission::Deny => {
                events.push(AgentEvent::Error(AgentError {
                    message: format!("Tool `{}` is denied by Horizon policy.", request.tool_id),
                }));
            }
        }
    }

    events
}
