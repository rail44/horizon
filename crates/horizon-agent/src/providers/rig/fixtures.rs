//! Prompts and tool results that make the deterministic fallback responder
//! call tools, so tests drive the tool loop without a network provider.
//! Compiled only for tests and for a build with the `test-fixtures`
//! feature (the `horizon-agent-acp` binary the end-to-end tests spawn): a production
//! build without a model answers with plain text whatever it is sent.

use rig_core::completion::{AssistantContent, Message};

use super::mapping::{rig_fs_read_call, rig_multi_snapshot_calls, rig_workspace_snapshot_call};
use crate::contract::ToolCallResult;

/// Line prefix the deterministic fallback reads an `fs_read` target from;
/// the rest of the line is the path, verbatim (case-sensitive, unlike the
/// keyword triggers below).
pub(crate) const READ_PATH_TRIGGER: &str = "fs_read path: ";

/// How many tool calls the "multi tool" trigger and the `loop_again_batch`
/// hook request — arbitrary but fixed, so tests can assert an exact count.
pub(super) const MULTI_TOOL_TEST_BATCH_SIZE: usize = 4;

pub(super) fn response_to(text: &str) -> Option<Message> {
    if let Some(path) = text
        .lines()
        .find_map(|line| line.trim().strip_prefix(READ_PATH_TRIGGER))
    {
        // A real `fs_read` -- including one whose path leaves the
        // workspace root, so the approval/refusal routing is exercisable.
        return Some(tool_calls(vec![rig_fs_read_call(path.trim())]));
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("multi tool") {
        // A parallel-tool-call batch (see `rig_multi_snapshot_calls`).
        Some(tool_calls(rig_multi_snapshot_calls(
            MULTI_TOOL_TEST_BATCH_SIZE,
        )))
    } else if lower.contains("snapshot") {
        Some(tool_calls(vec![rig_workspace_snapshot_call()]))
    } else {
        None
    }
}

pub(super) fn response_to_result(result: &ToolCallResult) -> Option<Message> {
    // A result whose output sets `"loop_again": true` requests the snapshot
    // tool again, so tests can drive consecutive tool-driven turns (e.g. the
    // iteration-cap guard). Real tool outputs never carry this key.
    if result.output.get("loop_again") == Some(&serde_json::Value::Bool(true)) {
        return Some(tool_calls(vec![rig_workspace_snapshot_call()]));
    }
    // The same for a parallel batch: `loop_again_batch`-many calls at once,
    // so tests can assert the iteration-cap guard counts one turn per batch.
    let count = result
        .output
        .get("loop_again_batch")
        .and_then(serde_json::Value::as_u64)?;
    Some(tool_calls(rig_multi_snapshot_calls(count as usize)))
}

fn tool_calls(calls: Vec<rig_core::completion::message::ToolCall>) -> Message {
    Message::Assistant(rig_core::completion::message::AssistantMessage::new(
        calls.into_iter().map(AssistantContent::ToolCall).collect(),
    ))
}
