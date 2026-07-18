use serde_json::Value;

use crate::contract::SessionId;
use crate::contract::{Event, ProviderId};
#[cfg(test)]
use crate::contract::{MessageRole, ToolCallId};
#[cfg(test)]
use crate::frame::AgentFrame;
use crate::roles::RoleId;

// `Store::sessions` (see `query.rs`) is only ever called from this crate's
// own tests (`session_snapshots`/`rebuild_projections`, both `cfg(test)`,
// plus this module's own test assertions) -- `cfg(test)`, like its sibling
// record types below.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredSession {
    pub session_id: SessionId,
    pub provider_id: Option<ProviderId>,
    /// Last-seen role for the session -- see `agent_sessions.role_id`'s
    /// doc comment in `schema.rs`.
    pub role_id: Option<RoleId>,
    pub last_sequence: i64,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredSessionSnapshot {
    pub session: AgentStoredSession,
    pub frame: AgentFrame,
    pub message_count: usize,
    pub tool_call_count: usize,
    pub approval_count: usize,
}

#[derive(Clone, Debug)]
#[cfg(test)]
pub(crate) struct AppendEvent {
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub provider_id: Option<ProviderId>,
    pub role_id: Option<RoleId>,
    pub event: Event,
    pub provider_payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentStoredEvent {
    pub event_id: String,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub sequence: i64,
    pub event_kind: String,
    pub event: Event,
    pub provider_id: Option<ProviderId>,
    /// The role active when this event was recorded -- see
    /// `agent_events.role_id`'s doc comment in `schema.rs`.
    pub role_id: Option<RoleId>,
    pub provider_payload: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredMessage {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub role: MessageRole,
    pub text: String,
    pub is_delta: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredToolCall {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: ToolCallId,
    pub tool_id: String,
    pub input: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredToolResult {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: ToolCallId,
    pub output: Value,
    /// Derived from `output`'s own `is_error` key at projection time -- see
    /// `agent_tool_results.is_error`'s doc comment in `schema.rs`.
    pub is_error: bool,
}

/// One row of `agent_turns` -- see that table's doc comment in `schema.rs`.
/// Test-only, like this file's other `AgentStored*` siblings: no caller
/// outside this crate's own tests needs turn rows as structured data yet.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredTurn {
    pub session_id: SessionId,
    pub turn_id: String,
    pub end_reason: String,
    pub ended_event_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct AgentStoredApproval {
    pub event_id: String,
    pub session_id: SessionId,
    pub sequence: i64,
    pub call_id: ToolCallId,
    pub reason: String,
    /// `None` while pending; `Some("approved"/"denied")` once resolved --
    /// see `agent_approvals.outcome`'s doc comment in `schema.rs`.
    pub outcome: Option<String>,
}

/// One row surfaced by the recall tools (`tools::recall`) via
/// `Store::search_history`/`Store::read_history_window` (`query.rs`): a
/// committed message, a tool call, or a tool result read straight from the
/// durable projection. Not test-only, unlike this file's `AgentStored*`
/// siblings above: `tools::recall` (a different module in this same crate,
/// not a downstream crate) needs it as real API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecallEntry {
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
    /// Only meaningful for a `ToolResult` entry -- `agent_tool_results.is_error`
    /// (see decision 1 in `docs/agent-feedback-design.md`). `None` for a
    /// `Message`/`ToolCall` entry, which has no such column.
    pub is_error: Option<bool>,
    /// The end reason of the turn this event belongs to (`agent_turns.end_reason`,
    /// joined via `agent_events.turn_id`) -- `None` for an event outside any
    /// turn, or whose turn hasn't ended yet. Only populated by
    /// [`super::Store::search_history`]; [`super::Store::read_history_window`]
    /// leaves this `None` on every row (see that method's doc comment).
    pub turn_outcome: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecallEntryKind {
    Message,
    ToolCall,
    ToolResult,
}

impl RecallEntryKind {
    pub(crate) fn as_str(self) -> &'static str {
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
pub(crate) struct RecallSearchReport {
    pub hits: Vec<RecallEntry>,
    pub total: usize,
}
