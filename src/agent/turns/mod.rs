//! Pure view-model for turn grouping and receipt summarization
//! (`docs/agent-output-ui-amendment.md` stage C, decisions 1-2). Kept
//! separate from `view.rs` so the grouping/aggregation logic has
//! colocated tests independent of GPUI rendering, and out of
//! `horizon-agent` so that crate stays UI-agnostic (verb naming, chip
//! composition, and humanized durations are display concerns, not
//! contract ones).
//!
//! Split into responsibility-focused submodules -- `grouping` (turn
//! spans), `bursts` (tool-activity bursts within a turn), `receipt`
//! (status/duration text and the collapsed-receipt aggregation),
//! `tool_call` (per-call view-model, approval derivation, and the
//! expanded body), `composer` (composer mode/placeholder/model chip),
//! and `diff` (reconstructed line diffs and the session-wide changes
//! overview) -- each re-exported here so every `turns::X` call site
//! elsewhere in the crate is unaffected by the split.

mod bursts;
mod composer;
mod diff;
mod grouping;
mod receipt;
mod tool_call;

pub(crate) use bursts::*;
pub(crate) use composer::*;
pub(crate) use diff::*;
pub(crate) use grouping::*;
pub(crate) use receipt::*;
pub(crate) use tool_call::*;

use std::path::Path;

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

/// `fs.edit`/`fs.write` are `Edit`, `bash` is `Bash`, and everything
/// else -- `fs.read`/`fs.grep`/`fs.glob`/`recall.*`/`workspace.snapshot`/
/// `skill.read`/any future tool id this crate doesn't otherwise
/// recognize -- is `Query` (the "read-only, low-signal" bucket the
/// receipt aggregates, `fs.read` aside, which gets its own
/// `read_file_count`). Shared by `receipt::aggregate_receipt` and
/// `diff::aggregate_changes` -- kept here for the same reason as
/// [`pluralize`].
fn classify_call(tool_id: &str) -> CallClass {
    match tool_id {
        "fs.edit" | "fs.write" => CallClass::Edit,
        "bash" => CallClass::Bash,
        _ => CallClass::Query,
    }
}

/// Extracts a path's file name for chip/overview display (`ToolCallKind::
/// File::file_name`/`FileChange::file_name`). Shared by `tool_call::
/// classify` and `diff::aggregate_changes` -- kept here for the same
/// reason as [`pluralize`].
fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::time::Duration;

    use horizon_agent::contract::{
        ApprovalRequest, Message, MessageDelta, MessageRole, ToolCallId, ToolCallRequest,
        ToolCallResult, TurnEndReason,
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

    pub(crate) fn assistant_message(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    pub(crate) fn assistant_delta(text: &str) -> AgentFrameItem {
        AgentFrameItem::AssistantTextDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    pub(crate) fn reasoning_delta(text: &str) -> AgentFrameItem {
        AgentFrameItem::ReasoningDelta(MessageDelta {
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

    pub(crate) fn turn_ended(
        reason: TurnEndReason,
        model: Option<&str>,
        elapsed_secs: u64,
    ) -> AgentFrameItem {
        AgentFrameItem::TurnEnded {
            reason,
            model: model.map(str::to_string),
            elapsed: Duration::from_secs(elapsed_secs),
        }
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
