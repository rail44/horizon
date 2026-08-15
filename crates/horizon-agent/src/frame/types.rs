use std::time::Duration;

use crate::contract::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentFrame {
    pub state: Option<SessionState>,
    pub items: Vec<AgentFrameItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentFrameItem {
    Message(Message),
    ReasoningDelta(MessageDelta),
    AssistantTextDelta(MessageDelta),
    ToolCallRequested(ToolCallRequest),
    ToolCallStarted(ToolCallId),
    ToolCallFinished(ToolCallResult),
    ApprovalRequested(ApprovalRequest),
    /// Ephemeral tool-call-argument-streaming progress (see
    /// [`ToolCallProgress`]): folded in place by
    /// [`apply_tool_call_progress_to_frame`] while arguments stream, and
    /// superseded in place once the real `ToolCallRequested` arrives (see
    /// the `Event::ToolCallRequested` arm in
    /// [`apply_agent_event_to_frame`]). Never produced by
    /// `agent_frame_from_events`/persisted replay — it never reaches the
    /// event log in the first place (`ProviderEvent::tool_call_progress`).
    ToolCallPreparing(ToolCallProgress),
    /// A Tier 1 compaction pass's divider row (`Event::HistoryCleared`,
    /// `docs/agent-compaction-design.md`'s "transcript に区切りを表示"):
    /// the point in the transcript from which older tool-result bodies stop
    /// being sent to the provider. Deliberately a visible marker rather than
    /// a silent projection detail — the transcript keeps showing the full
    /// results above it, so without the divider the session would look
    /// unchanged while the model's view of it changed.
    HistoryCleared(HistoryCleared),
    /// A standing-agent memory checkpoint: one turn's incremental update to
    /// the memory document (`Event::MemoryDigest`), or the harness's marker
    /// that a standing turn ended without one (`Event::MemoryCheckpointMissed`).
    /// Visible in the transcript so the digest's arrival — or its absence —
    /// is not silent (`docs/standing-agent-memory-design.md` decision 1,
    /// transparency).
    MemoryDigest(MemoryDigest),
    /// The turn-end checkpoint closed a standing turn without a memory update
    /// or a no-update declaration, after one reminder (`Event::MemoryCheckpointMissed`).
    MemoryCheckpointMissed,
    /// The provider rate-limited (or transiently rejected) a pre-generation
    /// request and the turn is waiting out a backoff before retrying.
    /// Visible but muted — the turn is still in progress. Deliberately not
    /// a turn boundary (see `is_turn_boundary_item`).
    ProviderRateLimited(ProviderRateLimited),
    Error(Error),
    Exited(Exit),
    /// A turn's receipt: the end reason `Event::TurnEnded` carries, plus the
    /// model id and elapsed duration folded in at reducer time -- see
    /// `docs/agent-output-ui-amendment.md`'s 2026-07-12 addendum (decision
    /// 1's turn-receipt line, decision 2's running-card footer) and
    /// [`TurnClock`]'s doc comment for the elapsed-time trade-off.
    TurnEnded {
        reason: TurnEndReason,
        /// The model id reported by the turn's most recent
        /// `Event::ProviderRequestSent`, if the turn made any provider
        /// request at all (a turn that ends before one -- e.g. an
        /// immediate cancel -- has none).
        model: Option<String>,
        /// Wall-clock time from the turn's opening `MessageCommitted`
        /// (`MessageRole::User`) to this fold. See [`TurnClock`].
        elapsed: Duration,
    },
}
