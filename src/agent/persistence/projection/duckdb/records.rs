use serde_json::Value;

use crate::agent::contract::{Event, ProviderId};
#[cfg(test)]
use crate::agent::contract::{MessageRole, ToolCallId};
#[cfg(test)]
use crate::agent::frame::AgentFrame;
use crate::session::SessionId;

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredSession {
    pub(crate) session_id: SessionId,
    pub(crate) provider_id: Option<ProviderId>,
    pub(crate) last_sequence: i64,
    pub(crate) updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredSessionSnapshot {
    pub(crate) session: AgentStoredSession,
    pub(crate) frame: AgentFrame,
    pub(crate) message_count: usize,
    pub(crate) tool_call_count: usize,
    pub(crate) approval_count: usize,
}

#[derive(Clone, Debug)]
#[cfg(test)]
pub(crate) struct AppendEvent {
    pub(crate) session_id: SessionId,
    pub(crate) turn_id: Option<String>,
    pub(crate) provider_id: Option<ProviderId>,
    pub(crate) event: Event,
    pub(crate) provider_payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentStoredEvent {
    pub(crate) event_id: String,
    pub(crate) session_id: SessionId,
    pub(crate) turn_id: Option<String>,
    pub(crate) sequence: i64,
    pub(crate) event_kind: String,
    pub(crate) event: Event,
    pub(crate) provider_id: Option<ProviderId>,
    pub(crate) provider_payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredMessage {
    pub(crate) event_id: String,
    pub(crate) session_id: SessionId,
    pub(crate) sequence: i64,
    pub(crate) role: MessageRole,
    pub(crate) text: String,
    pub(crate) is_delta: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredToolCall {
    pub(crate) event_id: String,
    pub(crate) session_id: SessionId,
    pub(crate) sequence: i64,
    pub(crate) call_id: ToolCallId,
    pub(crate) tool_id: String,
    pub(crate) input: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredToolResult {
    pub(crate) event_id: String,
    pub(crate) session_id: SessionId,
    pub(crate) sequence: i64,
    pub(crate) call_id: ToolCallId,
    pub(crate) output: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredApproval {
    pub(crate) event_id: String,
    pub(crate) session_id: SessionId,
    pub(crate) sequence: i64,
    pub(crate) call_id: ToolCallId,
    pub(crate) reason: String,
}
