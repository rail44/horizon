//! Wording and composer-interaction view-model for the agent transcript
//! (`docs/agent-output-ui-amendment.md` stage C, decisions 1-2). The
//! *structural* reading of the frame -- turn/burst grouping, tool-call and
//! approval derivation, receipt and change aggregation -- lives in
//! `crate::agent::model`. What's here is display-only: humanized
//! durations, receipt/changes-overview prose, the composer's placeholder
//! and mode state machine, the model chip's text composition, and the
//! per-tool expanded body.
//!
//! Split into responsibility-focused submodules -- `receipt` (status/
//! duration text and the collapsed-receipt prose), `tool_call` (the
//! expanded per-tool body and its terse summary fallback), `composer`
//! (composer mode/placeholder/model chip) and `diff` (the
//! changes-overview summary text) -- each re-exported here, together with
//! the structural items from `crate::agent::model` and the plain per-tool
//! JSON readers from `horizon_agent::transcript`, so every `turns::X`
//! call site reads one namespace.

mod composer;
mod diff;
mod receipt;
mod tool_call;

pub(crate) use composer::*;
pub(crate) use diff::*;
pub(crate) use receipt::*;
pub(crate) use tool_call::*;

pub(crate) use super::model::{
    aggregate_changes, aggregate_receipt, build_tool_call_views, contains_user_message,
    group_into_turns, is_approval_still_pending, latest_turn_model, progress,
    running_row_expandable, segment_bursts, tool_call_source, ReceiptAggregate, ToolCallView,
    TurnEnd,
};
pub(crate) use horizon_agent::transcript::{
    cap_lines_head, cap_lines_tail, classify, edit_entries, reconstruct_line_diff, str_field,
    ApprovalState, DiffLine, DiffLineKind, FileChange, ToolCallKind,
};

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
    use horizon_acp::{ApprovalKind, ToolCallMeta, ToolOutcome};
    use serde_json::Value;

    use super::super::model::{
        AgentFrameItem, Message, MessageRole, Permission, Thought, ToolCall, ToolCallIdentity,
        ToolCallStatus,
    };
    use super::{DiffLine, DiffLineKind};

    pub(crate) fn user_message(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            id: format!("user:{text}"),
            role: MessageRole::User,
            text: text.to_string(),
        })
    }

    pub(crate) fn assistant_delta(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            id: "assistant".to_string(),
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    pub(crate) fn reasoning_delta(text: &str) -> AgentFrameItem {
        AgentFrameItem::Thought(Thought {
            id: "thought".to_string(),
            text: text.to_string(),
        })
    }

    fn call(call_id: &str, tool_id: &str, input: Value) -> ToolCall {
        ToolCall {
            occurrence_id: call_id.to_string(),
            meta: ToolCallMeta {
                call_id: call_id.to_string(),
                tool_id: tool_id.to_string(),
                outcome: None,
                auto_approved: None,
                policy_tier: None,
                human_decision: None,
            },
            status: ToolCallStatus::Pending,
            input,
            output: None,
        }
    }

    /// A requested call that has not started.
    pub(crate) fn tool_requested(call_id: &str, tool_id: &str, input: Value) -> AgentFrameItem {
        AgentFrameItem::ToolCall(call(call_id, tool_id, input))
    }

    /// A finished call; an output with `is_error: true` reads as failed.
    pub(crate) fn tool_finished(
        call_id: &str,
        tool_id: &str,
        input: Value,
        output: Value,
    ) -> AgentFrameItem {
        let failed = output.get("is_error").and_then(Value::as_bool) == Some(true);
        let mut call = call(call_id, tool_id, input);
        call.status = if failed {
            ToolCallStatus::Failed
        } else {
            ToolCallStatus::Completed
        };
        call.meta.outcome = Some(if failed {
            ToolOutcome::Failed
        } else {
            ToolOutcome::Succeeded
        });
        call.output = Some(output);
        AgentFrameItem::ToolCall(call)
    }

    pub(crate) fn edit_result(path: &str) -> Value {
        use horizon_agent::contract::tool_output::{EditOutcome, EditReceipt, FileEdits};
        serde_json::to_value(FileEdits {
            edits: vec![EditReceipt {
                index: 0,
                path: path.into(),
                outcome: EditOutcome::Applied { occurrences: 1 },
            }],
            applied_count: 1,
            file_count: 1,
            failed_index: None,
            message: None,
        })
        .unwrap()
    }

    pub(crate) fn approval_requested(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::Permission(Permission {
            identity: ToolCallIdentity {
                call_id: call_id.to_string(),
                occurrence_id: call_id.to_string(),
            },
            kind: ApprovalKind::Standard,
            reason: "writes a file".to_string(),
            decision: None,
        })
    }

    pub(crate) fn diff_texts(lines: &[DiffLine]) -> Vec<(DiffLineKind, &str)> {
        lines
            .iter()
            .map(|line| (line.kind, line.text.as_str()))
            .collect()
    }
}
