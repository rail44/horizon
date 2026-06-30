use serde_json::Value;

use crate::{
    agent::{AgentEvent, AgentFrame, AgentMessageRole, AgentProviderId, AgentToolCallId},
    workspace::SessionId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredSession {
    pub session_id: SessionId,
    pub provider_id: Option<AgentProviderId>,
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
pub struct AppendAgentEvent {
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub provider_id: Option<AgentProviderId>,
    pub event: AgentEvent,
    pub provider_payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredEvent {
    pub event_id: String,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub sequence: i64,
    pub event_kind: String,
    pub event: AgentEvent,
    pub provider_id: Option<AgentProviderId>,
    pub provider_payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredMessage {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub role: AgentMessageRole,
    pub text: String,
    pub is_delta: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredToolCall {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: AgentToolCallId,
    pub tool_id: String,
    pub input: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredToolResult {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: AgentToolCallId,
    pub output: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredApproval {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: AgentToolCallId,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredConversationMessage {
    pub event_id: String,
    pub session_id: SessionId,
    pub conversation_id: String,
    pub turn_id: Option<String>,
    pub sequence: i64,
    pub provider_id: Option<AgentProviderId>,
    pub horizon_event_kind: String,
    pub rig_message_json: String,
}
