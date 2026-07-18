//! The structural transcript view-model: interprets the agent event
//! stream's `AgentFrameItem`s into turns, tool-activity bursts, tool-call/
//! approval state, and change aggregation.
//!
//! Relocated here from the `horizon` binary crate's `src/agent/turns/`
//! (owner decision 2026-07-18, shape c: "structure moves, presentation
//! stays"). Before this move, `src/agent/turns/` was already GPUI-free,
//! contract-only logic -- but it lived in the binary crate, so any future
//! frontend (a second GUI shell, a TUI, a CLI transcript dump, ...) would
//! have had to reimplement `group_into_turns`/`derive_approval_state`/
//! `aggregate_receipt` from scratch, with no guarantee two frontends would
//! agree on something as basic as "is this turn still running" or "did the
//! user approve this call". Moving the structural half here gives every
//! frontend one official reading of the event stream instead.
//!
//! **The boundary rule** (applied item-by-item against the pre-move code,
//! not assumed): an item MOVED here if it is GPUI-free *and* generates no
//! human-facing English text; it STAYED in `src/agent/turns/` if it
//! produces wording (labels, prose, humanized durations, summaries) or
//! encodes frontend interaction (composer modes, keyboard targeting).
//! Wording is legitimately per-frontend -- pluralization, a GPUI-specific
//! composer placeholder, a keyboard-hint string a TUI would phrase
//! differently -- none of it has one "official" answer the way "which
//! turn span does this item belong to" does, so it stays wherever it's
//! rendered.
//!
//! A few items didn't cleanly fall on one side (documented at their own
//! definition, not restructured to force the rule):
//! - `classify` (in `tool_call`) produces short display verbs ("Edit",
//!   "Bash", ...) and terse result summaries ("40 lines", "exit 0") that
//!   read as human-facing text, but it's the one place that knows each
//!   tool's input/output JSON shape -- every structural consumer
//!   (`build_tool_call_views`, receipt aggregation) needs its output, and
//!   `src/agent/turns`'s own wording layer (`terse_summary`) reuses it
//!   too. It moved here as the shared classifier; both sides agreeing on
//!   "this is an Edit call, here's its target" was judged more valuable
//!   than a clean wording/structure split would have been.
//! - `ToolCallBody`/`build_tool_call_body`/`tool_call_body` stayed in
//!   `src/agent/turns` instead of moving, despite being mostly structural
//!   (diff/content-preview/command capping): `build_tool_call_body`'s
//!   fallback arm for a terse, known-but-not-specially-bodied tool calls
//!   `terse_summary`, a wording function. Splitting the enum from its one
//!   constructor to chase the boundary rule would have meant either a
//!   signature change or duplicating that fallback logic across the crate
//!   boundary -- both against the "pure move, no logic edits" mandate --
//!   so the whole family stayed together on its wording side. The pure
//!   line-capping mechanics it leans on (`cap_lines_head`/`cap_lines_tail`)
//!   moved here regardless, since they're wording-free and reused by
//!   [`cap_thinking_text`] too.
//!
//! Split into responsibility-focused submodules, mirroring `src/agent/
//! turns/`'s own pre-move split: [`grouping`] (turn spans, plus
//! [`grouping::latest_turn_model`]'s plain model-id extraction), [`bursts`]
//! (tool-activity bursts within a turn), [`receipt`] (the collapsed-receipt
//! *aggregation* -- status/duration/prose text stayed behind in
//! `src/agent/turns`), [`tool_call`] (per-call view-model, approval
//! derivation, and the tool-call classifier), and [`diff`] (reconstructed
//! line diffs and the file-change aggregation -- the changes-overview
//! summary text stayed behind too). Each submodule is private; this file
//! curates what's re-exported, the same pattern `tools/mod.rs` uses.

mod bursts;
mod diff;
mod grouping;
mod receipt;
mod tool_call;

pub use bursts::{segment_bursts, thinking_visible_outside_burst, Burst};
pub use diff::{aggregate_changes, reconstruct_line_diff, DiffLine, DiffLineKind, FileChange};
pub use grouping::{contains_user_message, group_into_turns, latest_turn_model, TurnEnd, TurnSpan};
pub use receipt::{aggregate_receipt, CallClass, ReceiptAggregate};
pub use tool_call::{
    build_tool_call_views, cap_lines_head, cap_lines_tail, cap_thinking_text, classify,
    is_approval_still_pending, progress, running_row_expandable, str_field, ApprovalState,
    ToolCallKind, ToolCallView, THINKING_TAIL_LINES,
};

use std::path::Path;

/// `fs.edit`/`fs.write` are `Edit`, `bash` is `Bash`, and everything
/// else -- `fs.read`/`fs.grep`/`fs.glob`/`recall.*`/`workspace.snapshot`/
/// `skill.read`/any future tool id this crate doesn't otherwise
/// recognize -- is `Query` (the "read-only, low-signal" bucket the
/// receipt aggregates, `fs.read` aside, which gets its own
/// `read_file_count`). Shared by `receipt::aggregate_receipt` and
/// `diff::aggregate_changes`.
fn classify_call(tool_id: &str) -> CallClass {
    match tool_id {
        "fs.edit" | "fs.write" => CallClass::Edit,
        "bash" => CallClass::Bash,
        _ => CallClass::Query,
    }
}

/// Extracts a path's file name for chip/overview display (`ToolCallKind::
/// File::file_name`/`FileChange::file_name`). Shared by `tool_call::
/// classify` and `diff::aggregate_changes`.
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

    use crate::contract::{
        ApprovalRequest, Message, MessageDelta, MessageRole, ToolCallId, ToolCallRequest,
        ToolCallResult, TurnEndReason,
    };
    use crate::frame::AgentFrameItem;
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
