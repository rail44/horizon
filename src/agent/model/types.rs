//! The folded frame's data types, defined on ACP-native data: messages are
//! keyed by their ACP `messageId`, tool calls by their occurrence id (the
//! ACP `toolCallId`) with the daemon's [`ToolCallMeta`], and approvals by
//! the pending `session/request_permission` they came from.

use std::time::Duration;

use horizon_acp::{ApprovalKind, MemoryDigest, ToolCallMeta, ToolOutcome};

/// One execution attempt of a tool call: the provider's call id plus the
/// daemon's occurrence id (the ACP `toolCallId`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ToolCallIdentity {
    pub(crate) call_id: String,
    pub(crate) occurrence_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MessageRole {
    User,
    Assistant,
    /// `MessageMeta{role: TaskNotification}` on a `user_message`.
    TaskNotification,
    /// `MessageMeta{role: AutoContinue}` on a `user_message`.
    AutoContinue,
}

impl MessageRole {
    pub(crate) fn display_label(self) -> &'static str {
        match self {
            Self::User => "you",
            Self::TaskNotification => "task",
            Self::AutoContinue => "continue",
            Self::Assistant => "agent",
        }
    }
}

/// A `user_message`/`agent_message` and its chunks, folded by `messageId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Message {
    pub(crate) id: String,
    pub(crate) role: MessageRole,
    pub(crate) text: String,
}

/// An `agent_thought`/`agent_thought_chunk` stream, folded by `messageId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Thought {
    pub(crate) id: String,
    pub(crate) text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

/// One `tool_call_update` series, upserted by occurrence id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolCall {
    pub(crate) occurrence_id: String,
    pub(crate) meta: ToolCallMeta,
    pub(crate) status: ToolCallStatus,
    pub(crate) input: serde_json::Value,
    pub(crate) output: Option<serde_json::Value>,
}

impl ToolCall {
    pub(crate) fn identity(&self) -> ToolCallIdentity {
        ToolCallIdentity {
            call_id: self.meta.call_id.clone(),
            occurrence_id: self.occurrence_id.clone(),
        }
    }

    /// The terminal outcome: the daemon's `ToolCallMeta::outcome` when
    /// present, otherwise read from the ACP status.
    pub(crate) fn outcome(&self) -> Option<ToolOutcome> {
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

    pub(crate) fn started(&self) -> bool {
        self.status != ToolCallStatus::Pending
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionDecision {
    Approved,
    Denied,
    Cancelled,
}

/// A `session/request_permission` received on the current attachment and,
/// once the shell has answered it, the answer given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Permission {
    pub(crate) identity: ToolCallIdentity,
    pub(crate) kind: ApprovalKind,
    /// The request's `title`, with its `description` appended when present.
    pub(crate) reason: String,
    pub(crate) decision: Option<PermissionDecision>,
}

/// `_horizon/tool_call_progress` `Progress`, removed by its `Closed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolCallPreparing {
    pub(crate) key: String,
    pub(crate) tool_id: Option<String>,
    pub(crate) bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryCleared {
    pub(crate) cleared_occurrence_ids: Vec<String>,
    pub(crate) recovered_chars: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderRateLimited {
    pub(crate) status: Option<u16>,
    pub(crate) attempt: u32,
    pub(crate) backoff_ms: u64,
}

/// A turn's stop result, read from `state_update` idle's `stopReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnEndReason {
    Completed,
    Cancelled,
    Failed,
    HaltedByIterationCap,
    HaltedByDoomLoop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentFrameItem {
    Message(Message),
    Thought(Thought),
    ToolCall(ToolCall),
    Permission(Permission),
    ToolCallPreparing(ToolCallPreparing),
    HistoryCleared(HistoryCleared),
    MemoryDigest(MemoryDigest),
    MemoryCheckpointMissed,
    ProviderRateLimited(ProviderRateLimited),
    Error(String),
    Exited(String),
    /// Folded from an idle `state_update` carrying a stop reason, plus the
    /// model of the turn's last `_horizon/provider_request` `Sent` and the
    /// wall-clock time since the turn opened on this attachment.
    TurnEnded {
        reason: TurnEndReason,
        model: Option<String>,
        elapsed: Duration,
    },
}

/// The ACP v2 `state_update` phases, plus the terminal phase an
/// `Exited` session event leaves behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionPhase {
    Running,
    RequiresAction,
    Idle,
    Terminated,
}

/// The execution state the view labels, derived from the phase and the
/// tool calls in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionState {
    Running,
    ToolRunning,
    WaitingForApproval,
    Idle,
    Terminated,
}

/// The session status shared with the board, combining the execution state
/// with the retained stop result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionStatus {
    Running,
    ToolRunning,
    WaitingForInput,
    WaitingForApproval,
    Cancelled,
    Paused,
    Failed,
    Terminated,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentFrame {
    pub(crate) phase: Option<SessionPhase>,
    pub(crate) items: Vec<AgentFrameItem>,
    /// The latest stop result, retained through idle and cleared when the
    /// session runs again.
    pub(crate) turn_end_reason: Option<TurnEndReason>,
}

/// The session's selected provider and model, read from the `model` config
/// option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelSelection {
    pub(crate) provider: String,
    pub(crate) model: String,
}
