//! Read-only questions over a folded frame.

use super::types::*;

impl AgentFrame {
    /// The execution state: the phase, with a running phase that has a tool
    /// call in progress read as `ToolRunning`.
    pub(crate) fn state(&self) -> Option<SessionState> {
        Some(match self.phase? {
            SessionPhase::Running => {
                let tool_running = self.items.iter().any(|item| {
                    matches!(item, AgentFrameItem::ToolCall(call) if call.status == ToolCallStatus::InProgress)
                });
                if tool_running {
                    SessionState::ToolRunning
                } else {
                    SessionState::Running
                }
            }
            SessionPhase::RequiresAction => SessionState::WaitingForApproval,
            SessionPhase::Idle => SessionState::Idle,
            SessionPhase::Terminated => SessionState::Terminated,
        })
    }

    /// The current state with the retained stop result. A completed,
    /// failed, cancelled or halted result survives idle until the session
    /// runs again; idle without a result reads as starting until the
    /// session has a message.
    pub(crate) fn status(&self) -> Option<SessionStatus> {
        Some(match self.state()? {
            SessionState::Running => SessionStatus::Running,
            SessionState::ToolRunning => SessionStatus::ToolRunning,
            SessionState::WaitingForApproval => SessionStatus::WaitingForApproval,
            SessionState::Terminated => SessionStatus::Terminated,
            SessionState::Idle => match self.turn_end_reason {
                Some(TurnEndReason::Failed) => SessionStatus::Failed,
                Some(TurnEndReason::Cancelled) => SessionStatus::Cancelled,
                Some(TurnEndReason::HaltedByIterationCap | TurnEndReason::HaltedByDoomLoop) => {
                    SessionStatus::Paused
                }
                Some(TurnEndReason::Completed) => SessionStatus::Completed,
                None if !self
                    .items
                    .iter()
                    .any(|item| matches!(item, AgentFrameItem::Message(_))) =>
                {
                    SessionStatus::Starting
                }
                None => SessionStatus::WaitingForInput,
            },
        })
    }

    /// Whether the session is idle on a guard-halted turn (`max_turn_requests`
    /// or `_horizon/doom_loop`), i.e. `_horizon/continue_turn` has a turn to
    /// resume.
    pub(crate) fn halted_awaiting_continue(&self) -> bool {
        self.phase == Some(SessionPhase::Idle)
            && matches!(
                self.turn_end_reason,
                Some(TurnEndReason::HaltedByIterationCap | TurnEndReason::HaltedByDoomLoop)
            )
    }
}

/// Whether a turn is in flight and therefore cancellable: running, running
/// a tool, or waiting on an approval.
pub(crate) fn state_indicates_turn_in_flight(state: Option<SessionState>) -> bool {
    matches!(
        state,
        Some(SessionState::Running | SessionState::ToolRunning | SessionState::WaitingForApproval)
    )
}

/// Permission requests still awaiting the shell's answer, oldest first,
/// excluding any whose turn has since ended.
pub(crate) fn actionable_pending_approval_identities_in(
    items: &[AgentFrameItem],
) -> Vec<ToolCallIdentity> {
    let start = items
        .iter()
        .rposition(|item| matches!(item, AgentFrameItem::TurnEnded { .. }))
        .map_or(0, |index| index + 1);
    pending_approval_identities_in(&items[start..])
}

/// Every unanswered permission request in `items`, oldest first.
pub(crate) fn pending_approval_identities_in(items: &[AgentFrameItem]) -> Vec<ToolCallIdentity> {
    items
        .iter()
        .filter_map(|item| match item {
            AgentFrameItem::Permission(permission) if permission.decision.is_none() => {
                Some(permission.identity.clone())
            }
            _ => None,
        })
        .collect()
}
