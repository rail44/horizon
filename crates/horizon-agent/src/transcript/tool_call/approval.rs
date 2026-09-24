use crate::contract::{ToolCallResult, ToolOutcome};

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
        None => ApprovalState::Waiting,
    }
}
