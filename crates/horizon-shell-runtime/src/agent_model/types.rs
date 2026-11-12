//! The folded frame's data types, defined on ACP-native data: messages are
//! keyed by their ACP `messageId`, tool calls by their occurrence id (the
//! ACP `toolCallId`) with the daemon's [`ToolCallMeta`], and approvals by
//! the pending `session/request_permission` they came from.

use std::time::Duration;

use horizon_acp::{MemoryDigest, ToolCallMeta, ToolOutcome};

/// One execution attempt of a tool call: the provider's call id plus the
/// daemon's occurrence id (the ACP `toolCallId`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ToolCallIdentity {
    pub call_id: String,
    pub occurrence_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    /// `MessageMeta{role: TaskNotification}` on a `user_message`.
    TaskNotification,
    /// `MessageMeta{role: AutoContinue}` on a `user_message`.
    AutoContinue,
}

/// A `user_message`/`agent_message` and its chunks, folded by `messageId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub text: String,
}

/// An `agent_thought`/`agent_thought_chunk` stream, folded by `messageId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thought {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

/// One `tool_call_update` series, upserted by occurrence id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub occurrence_id: String,
    pub meta: ToolCallMeta,
    pub status: ToolCallStatus,
    pub input: serde_json::Value,
    pub output: Option<serde_json::Value>,
}

impl ToolCall {
    pub fn identity(&self) -> ToolCallIdentity {
        ToolCallIdentity {
            call_id: self.meta.call_id.clone(),
            occurrence_id: self.occurrence_id.clone(),
        }
    }

    /// The terminal outcome: the daemon's `ToolCallMeta::outcome` when
    /// present, otherwise read from the ACP status.
    pub fn outcome(&self) -> Option<ToolOutcome> {
        if let Some(outcome) = &self.meta.outcome {
            return Some(outcome.clone());
        }
        match self.status {
            ToolCallStatus::Pending | ToolCallStatus::InProgress => None,
            ToolCallStatus::Completed => Some(ToolOutcome::Succeeded),
            ToolCallStatus::Failed => Some(ToolOutcome::Failed),
            ToolCallStatus::Cancelled => Some(ToolOutcome::Cancelled),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Approved,
    Denied,
    Cancelled,
}

/// A `session/request_permission` received on the current attachment and,
/// once the shell has answered it, the answer given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub identity: ToolCallIdentity,
    /// The request's `title`, with its `description` appended when present.
    pub reason: String,
    pub decision: Option<PermissionDecision>,
}

/// `_horizon/tool_call_progress` `Progress`, removed by its `Closed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallPreparing {
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryCleared {
    pub cleared_occurrence_ids: Vec<String>,
    pub recovered_chars: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRateLimited {
    pub attempt: u32,
    pub backoff_ms: u64,
}

/// A turn's stop result, read from `state_update` idle's `stopReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnEndReason {
    Completed,
    Cancelled,
    Failed,
    HaltedByIterationCap,
    HaltedByDoomLoop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentFrameItem {
    Message(Message),
    Thought(Thought),
    ToolCall(Box<ToolCall>),
    Permission(Permission),
    ToolCallPreparing(ToolCallPreparing),
    HistoryCleared(HistoryCleared),
    MemoryDigest(MemoryDigest),
    MemoryCheckpointMissed,
    ProviderRateLimited(ProviderRateLimited),
    Error(String),
    Exited(String),
    /// Folded from an idle `state_update` carrying a stop reason, plus the
    /// wall-clock time since the turn opened on this attachment.
    TurnEnded {
        reason: TurnEndReason,
        elapsed: Duration,
    },
}

/// The ACP v2 `state_update` phases, plus the terminal phase an
/// `Exited` session event leaves behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPhase {
    Running,
    RequiresAction,
    Idle,
    Terminated,
}

/// The execution state the view labels, derived from the phase and the
/// tool calls in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Running,
    ToolRunning,
    WaitingForApproval,
    Idle,
    Terminated,
}

/// The session status shared with the board, combining the execution state
/// with the retained stop result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Starting,
    Running,
    ToolRunning,
    WaitingForInput,
    WaitingForApproval,
    Cancelled,
    Paused,
    Completed,
    Failed,
    Terminated,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentFrame {
    pub phase: Option<SessionPhase>,
    pub items: Vec<AgentFrameItem>,
    /// The latest stop result, retained through idle and cleared when the
    /// session runs again.
    pub turn_end_reason: Option<TurnEndReason>,
    /// Moves whenever an item of a turn that has ended changes or
    /// disappears, or the fold starts over. Items before the last
    /// `TurnEnded` are otherwise settled, so a reader that keeps what it
    /// built from them (the pane's adapter) rebuilds only when this moves.
    pub settled_revision: u64,
}

/// The session's selected provider and model, read from the `model` config
/// option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSelection {
    pub model: String,
}
