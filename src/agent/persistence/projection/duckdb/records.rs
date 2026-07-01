use serde_json::Value;

use crate::{
    agent::contract::{Event, MessageRole, ProviderId, ToolCallId},
    agent::frame::AgentFrame,
    session::SessionId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredSession {
    pub session_id: SessionId,
    pub provider_id: Option<ProviderId>,
    pub last_sequence: i64,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredSessionSnapshot {
    pub session: AgentStoredSession,
    pub frame: AgentFrame,
    pub message_count: usize,
    pub tool_call_count: usize,
    pub approval_count: usize,
}

#[derive(Clone, Debug)]
pub struct AppendEvent {
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub provider_id: Option<ProviderId>,
    pub event: Event,
    pub provider_payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredEvent {
    pub event_id: String,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub sequence: i64,
    pub event_kind: String,
    pub event: Event,
    pub provider_id: Option<ProviderId>,
    pub provider_payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredMessage {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub role: MessageRole,
    pub text: String,
    pub is_delta: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredToolCall {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: ToolCallId,
    pub tool_id: String,
    pub input: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredToolResult {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: ToolCallId,
    pub output: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredApproval {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: ToolCallId,
    pub reason: String,
}
