//! Wording and composer-interaction view-model for the agent transcript
//! (`docs/agent-output-ui-amendment.md` stage C, decisions 1-2). The
//! *structural* reading of the event stream this used to hold in full --
//! turn/burst grouping, tool-call/approval derivation, and receipt/change
//! aggregation -- moved to `horizon_agent::transcript` (owner decision
//! 2026-07-18, shape c: "structure moves, presentation stays"), so any
//! future frontend shares one official reading of "which turn is this
//! item in" / "did the user approve this call" rather than each
//! reimplementing it. What's left here is display-only: humanized
//! durations, receipt/changes-overview prose, the composer's placeholder
//! and mode state machine, the model chip's text composition, and the
//! per-tool expanded body (which leans on a wording fallback, so it
//! stayed whole rather than splitting one function across the crate
//! boundary -- see `horizon_agent::transcript`'s module doc for the full
//! boundary rule and the two items that didn't cleanly split).
//!
//! Split into responsibility-focused submodules -- `receipt` (status/
//! duration text, the collapsed-receipt prose, and `ReceiptTail`, a
//! thin view-side wrapper that only ever selects between two wording
//! branches), `tool_call` (the expanded per-tool body and its terse
//! summary fallback), `composer` (composer mode/placeholder/model chip)
//! and `diff` (the changes-overview summary text) -- each re-exported
//! here so every `turns::X` call site elsewhere in the crate is
//! unaffected by the split. This module also re-exports every moved
//! structural type/function from `horizon_agent::transcript` under its
//! original name, so call sites outside `turns/` (`view.rs`,
//! `session.rs`) needed no changes at all.

mod composer;
mod diff;
mod receipt;
mod tool_call;

pub(crate) use composer::*;
pub(crate) use diff::*;
pub(crate) use receipt::*;
pub(crate) use tool_call::*;

// Re-exported under their original names so every pre-move `turns::X`
// call site elsewhere in the crate (`view.rs`, `session.rs`) keeps
// working unchanged -- the structural reading itself now lives in
// `horizon_agent::transcript` (see this module's own doc comment).
pub(crate) use horizon_agent::transcript::{
    aggregate_changes, aggregate_receipt, build_tool_call_views, cap_lines_head, cap_lines_tail,
    cap_thinking_text, classify, contains_user_message, group_into_turns,
    is_approval_still_pending, latest_turn_model, progress, reconstruct_line_diff,
    running_row_expandable, str_field, ApprovalState, DiffLine, DiffLineKind, FileChange,
    ReceiptAggregate, ToolCallKind, ToolCallView, TurnEnd, THINKING_TAIL_LINES,
};
pub(crate) use horizon_agent::transcript::{segment_bursts, thinking_visible_outside_burst};

/// `1 {singular}` / `{count} {plural}`. Shared by `receipt::receipt_prose`
/// and `diff::changes_summary_text` -- kept here rather than in either
/// submodule since both are equally "primary" users.
fn pluralize(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use horizon_agent::contract::{
        ApprovalRequest, Message, MessageDelta, MessageRole, ToolCallId, ToolCallRequest,
        ToolCallResult,
    };
    use horizon_agent::frame::AgentFrameItem;
    use serde_json::Value;

    use super::{DiffLine, DiffLineKind};

    pub(crate) fn user_message(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            role: MessageRole::User,
            text: text.to_string(),
        })
    }

    pub(crate) fn assistant_delta(text: &str) -> AgentFrameItem {
        AgentFrameItem::AssistantTextDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    pub(crate) fn tool_requested(call_id: &str, tool_id: &str, input: Value) -> AgentFrameItem {
        AgentFrameItem::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId(call_id.to_string()),
            tool_id: tool_id.to_string(),
            input,
        })
    }

    pub(crate) fn tool_finished(call_id: &str, output: Value) -> AgentFrameItem {
        AgentFrameItem::ToolCallFinished(ToolCallResult::new(
            ToolCallId(call_id.to_string()),
            output,
        ))
    }

    pub(crate) fn tool_started(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallStarted(ToolCallId(call_id.to_string()))
    }

    pub(crate) fn approval_requested(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ApprovalRequested(ApprovalRequest {
            call_id: ToolCallId(call_id.to_string()),
            reason: "writes a file".to_string(),
        })
    }

    pub(crate) fn diff_texts(lines: &[DiffLine]) -> Vec<(DiffLineKind, &str)> {
        lines
            .iter()
            .map(|line| (line.kind, line.text.as_str()))
            .collect()
    }
}
