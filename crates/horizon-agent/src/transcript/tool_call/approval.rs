use crate::contract::{ApprovalDecisionPayload, ToolCallResult, ToolOutcome};

use super::view::ApprovalState;

/// The display register an abandoned attempt's row reports instead of a
/// tool-specific summary.
pub const SUPERSEDED_SUMMARY: &str = "superseded by retry";

/// Derives a call's [`ApprovalState`] from whether it ever had an
/// `ApprovalRequested` item and, if resolved, its `ToolCallStarted`/
/// `ToolCallFinished` acks. `started` takes priority over an absent
/// `result`: a `bash` approve folds `ToolCallStarted` immediately and its
/// `ToolCallFinished` only once the child actually exits, so a call can
/// read `Approved` here well before it reads `finished` in the same
/// [`ToolCallView`].
pub(super) fn derive_approval_state(
    had_approval_request: bool,
    started: bool,
    decision: Option<&ApprovalDecisionPayload>,
    result: Option<&ToolCallResult>,
) -> ApprovalState {
    if !had_approval_request {
        return ApprovalState::None;
    }
    if started {
        return ApprovalState::Approved;
    }
    match result {
        Some(result) if result.is_denied() => ApprovalState::Denied,
        Some(result) if result.outcome == ToolOutcome::Cancelled => ApprovalState::Cancelled,
        Some(result) if result.is_superseded() => ApprovalState::Superseded,
        Some(_) => ApprovalState::Approved,
        None => match decision {
            Some(ApprovalDecisionPayload::Approve) => ApprovalState::Approved,
            Some(ApprovalDecisionPayload::Deny { .. }) => ApprovalState::Denied,
            None => ApprovalState::Waiting,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{
        ApprovalKind, ApprovalRequest, ApprovalResolved, OccurrenceId, ToolCallId, ToolCallRequest,
    };
    use crate::frame::AgentFrameItem;

    #[test]
    fn a_saved_decision_closes_only_its_own_rows_buttons_before_execution_starts() {
        for decision in [
            ApprovalDecisionPayload::Approve,
            ApprovalDecisionPayload::Deny { reason: None },
        ] {
            let request = ToolCallRequest {
                call_id: ToolCallId("same".into()),
                occurrence_id: OccurrenceId("old".into()),
                tool_id: "bash".into(),
                input: serde_json::json!({"command":"true"}).into(),
            };
            let approval = ApprovalRequest {
                call_id: request.call_id.clone(),
                occurrence_id: request.occurrence_id.clone(),
                reason: "run".into(),
                kind: ApprovalKind::Standard,
            };
            let mut retry = request.clone();
            retry.occurrence_id = OccurrenceId("retry".into());
            let items = vec![
                AgentFrameItem::ToolCallRequested(request.clone()),
                AgentFrameItem::ApprovalRequested(approval.clone()),
                AgentFrameItem::ApprovalResolved(ApprovalResolved {
                    call_id: request.call_id.clone(),
                    occurrence_id: request.occurrence_id.clone(),
                    decision: decision.clone(),
                }),
                AgentFrameItem::ToolCallRequested(retry.clone()),
                AgentFrameItem::ApprovalRequested(ApprovalRequest {
                    occurrence_id: retry.occurrence_id.clone(),
                    ..approval
                }),
            ];
            let rows = crate::transcript::build_tool_call_views(&items);
            assert_eq!(rows[0].identity(), request.identity());
            assert_eq!(
                rows[0].approval,
                if matches!(decision, ApprovalDecisionPayload::Approve) {
                    ApprovalState::Approved
                } else {
                    ApprovalState::Denied
                }
            );
            assert_eq!(rows[1].identity(), retry.identity());
            assert_eq!(rows[1].approval, ApprovalState::Waiting);
        }
    }
}
