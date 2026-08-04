use crate::contract::ToolCallResult;
use serde_json::Value;

use super::view::ApprovalState;

/// Whether `result` represents the user's tool-call denial. Reads the
/// contract-explicit [`ToolCallResult::denied`] marker first -- set at the
/// source by `tools::approval::synchronous_result`'s `ran = false` path
/// (`crate::tools::approval`) -- and falls back to [`is_denied_output`]'s
/// old message-text convention only when the marker reads `false`. That
/// fallback exists for exactly one case: a `ToolCallResult` persisted (as
/// JSONL) before the marker field existed deserializes with `denied: false`
/// regardless of its real outcome (`#[serde(default)]`), so replaying an
/// old log still needs the message text to classify those rows correctly.
/// A freshly folded denial always carries the marker and never needs the
/// fallback.
fn is_denied(result: &ToolCallResult) -> bool {
    result.denied || is_denied_output(&result.output)
}

/// The old denial convention `tools::approval::denied_output` wrote for a
/// Horizon-executed tool's deny path, before [`ToolCallResult::denied`]
/// existed: `json!({ "is_error": true, "message": "denied by user" })`.
/// Checked by the message text specifically, not just `is_error`, because
/// an *approved* call that goes on to fail for its own reasons (e.g.
/// fs.edit's "old_string not found") is also `is_error: true` but carries a
/// different message -- `is_error` alone can't tell a denial from an
/// execution failure. Kept only as [`is_denied`]'s fallback for pre-marker
/// persisted logs; every current production write path sets the marker
/// instead.
fn is_denied_output(output: &Value) -> bool {
    output.get("is_error").and_then(Value::as_bool) == Some(true)
        && output.get("message").and_then(Value::as_str) == Some("denied by user")
}

/// Output-JSON marker key for an abandoned denial-retry attempt's terminal
/// result -- see [`ToolCallView::superseded`]. Defined here, next to
/// [`is_denied_output`]'s convention, and used by the one writer
/// (`crate::tools::approval::superseded_by_retry_result`) so the key exists
/// exactly once.
pub(crate) const SUPERSEDED_BY_RETRY: &str = "superseded_by_retry";

/// The display register an abandoned attempt's row reports instead of a
/// tool-specific summary.
pub const SUPERSEDED_SUMMARY: &str = "superseded by retry";

/// Whether `output` is an abandoned denial-retry attempt's terminal result.
pub(crate) fn is_superseded_output(output: &Value) -> bool {
    output.get(SUPERSEDED_BY_RETRY).and_then(Value::as_bool) == Some(true)
}

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
    match result {
        Some(result) if is_denied(result) => ApprovalState::Denied,
        Some(_) => ApprovalState::Approved,
        None if started => ApprovalState::Approved,
        None => ApprovalState::Waiting,
    }
}
