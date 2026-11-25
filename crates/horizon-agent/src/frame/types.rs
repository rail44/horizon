use crate::contract::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentFrame {
    pub state: Option<SessionState>,
    pub items: Vec<AgentFrameItem>,
    /// The current stop result, retained through idle/startup events and
    /// cleared when execution actually resumes. Rebuilt by event replay.
    pub(crate) turn_end_reason: Option<TurnEndReason>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentFrameItem {
    Message(Message),
    ToolCallRequested(ToolCallRequest),
    ToolCallStarted(ToolCallIdentity),
    ToolCallFinished(ToolCallResult),
    ApprovalRequested(ApprovalRequest),
    ApprovalResolved(ApprovalResolved),
    /// A Tier 1 compaction pass's divider row (`Event::HistoryCleared`,
    /// `docs/agent-compaction-design.md`'s "transcript に区切りを表示"):
    /// the point in the transcript from which older tool-result bodies stop
    /// being sent to the provider. Deliberately a visible marker rather than
    /// a silent projection detail — the transcript keeps showing the full
    /// results above it, so without the divider the session would look
    /// unchanged while the model's view of it changed.
    HistoryCleared(HistoryCleared),
    /// The provider rate-limited (or transiently rejected) a pre-generation
    /// request and the turn is waiting out a backoff before retrying.
    /// Visible but muted — the turn is still in progress. Deliberately not
    /// a turn boundary (see `is_turn_boundary_item`).
    ProviderRateLimited(ProviderRateLimited),
    Error(Error),
    Exited(Exit),
    /// Where a turn ended, and why (`Event::TurnEnded`).
    TurnEnded {
        reason: TurnEndReason,
    },
}
