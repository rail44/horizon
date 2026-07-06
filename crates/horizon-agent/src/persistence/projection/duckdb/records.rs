use serde_json::Value;

use crate::contract::SessionId;
use crate::contract::{Event, ProviderId};
#[cfg(test)]
use crate::contract::{MessageRole, ToolCallId};
#[cfg(test)]
use crate::frame::AgentFrame;

// Not `cfg(test)`, unlike its sibling record types below: `Store::sessions`
// (see `query.rs`) isn't test-only — a downstream crate's tests (`horizon`'s
// DuckDB-replay regression tests) can't trigger this crate's own
// `cfg(test)`, so this and `Store::sessions` stay real API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStoredSession {
    pub session_id: SessionId,
    pub provider_id: Option<ProviderId>,
    pub last_sequence: i64,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub struct AgentStoredSessionSnapshot {
    pub session: AgentStoredSession,
    pub frame: AgentFrame,
    pub message_count: usize,
    pub tool_call_count: usize,
    pub approval_count: usize,
}

#[derive(Clone, Debug)]
#[cfg(test)]
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
#[cfg(test)]
pub struct AgentStoredMessage {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub role: MessageRole,
    pub text: String,
    pub is_delta: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub struct AgentStoredToolCall {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: ToolCallId,
    pub tool_id: String,
    pub input: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub struct AgentStoredToolResult {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: ToolCallId,
    pub output: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub struct AgentStoredApproval {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: ToolCallId,
    pub reason: String,
}

/// One row surfaced by the recall tools (`tools::recall`) via
/// `Store::search_history`/`Store::read_history_window` (`query.rs`): a
/// committed message, a tool call, or a tool result read straight from the
/// durable projection. Not test-only, unlike this file's `AgentStored*`
/// siblings above: `tools::recall` (a different module in this same crate,
/// not a downstream crate) needs it as real API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallEntry {
    pub session_id: SessionId,
    pub sequence: i64,
    pub kind: RecallEntryKind,
    /// The message role (`"user"`/`"assistant"`) for a
    /// [`RecallEntryKind::Message`], or the tool id -- falling back to the
    /// bare `call_id` if no matching `agent_tool_calls` row exists, see
    /// `Store::search_history`'s doc comment -- for a `ToolCall`/
    /// `ToolResult` entry.
    pub role_or_tool: String,
    /// Bounded to a few KB at the SQL layer (`query::RECALL_TEXT_BOUND_CHARS`);
    /// callers building a search snippet or a windowed transcript
    /// (`tools::recall`) trim this further.
    pub text: String,
    /// The event's real wall-clock time (`agent_events.event_at`, see its
    /// own doc comment in `schema.rs`), as DuckDB's own `TEXT` rendering.
    pub at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecallEntryKind {
    Message,
    ToolCall,
    ToolResult,
}

impl RecallEntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
        }
    }
}

/// [`super::Store::search_history`]'s result: the rows within its `limit`,
/// plus the total match count across the whole search (scope included) so a
/// caller (`recall.search`) can report "there were N total, here are the
/// first `limit`" instead of silently truncating.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallSearchReport {
    pub hits: Vec<RecallEntry>,
    pub total: usize,
}
