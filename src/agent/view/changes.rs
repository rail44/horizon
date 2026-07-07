//! The session's Changes overview (`docs/agent-output-ui-design.md`
//! decision 9): one aggregated row per file touched by a successful
//! `fs.edit`/`fs.write` call this session, plus the session-wide diffstat
//! total. Pure data derivation only -- the collapsible bar/list view lives
//! in `mod.rs`.

use std::collections::HashMap;

use serde_json::Value;

use crate::agent::contract::ToolCallId;
use crate::agent::frame::{AgentFrame, AgentFrameItem};

use super::diff;
use super::transcript::is_error_output;

/// One file's aggregated changes across every successful edit/write call
/// this session made to it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FileChange {
    pub(super) path: String,
    pub(super) edits: usize,
    pub(super) added: usize,
    pub(super) removed: usize,
    /// The transcript block id of the tool call that most recently touched
    /// this path -- the same stable id `transcript_blocks` assigns a merged
    /// tool block (its `ToolCallRequested`/`ToolCallPreparing` item's own
    /// index in `frame.items`), so a file row's click target can feed
    /// straight into `mod.rs`'s `block_view_ids` lookup like every other
    /// scroll-to-block jump.
    pub(super) last_block_id: usize,
}

/// The session-wide diffstat total across every [`FileChange`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ChangesTotal {
    pub(super) files: usize,
    pub(super) added: usize,
    pub(super) removed: usize,
}

pub(super) fn changes_total(changes: &[FileChange]) -> ChangesTotal {
    ChangesTotal {
        files: changes.len(),
        added: changes.iter().map(|change| change.added).sum(),
        removed: changes.iter().map(|change| change.removed).sum(),
    }
}

/// Aggregates every successful `fs.edit`/`fs.write` tool call in `frame` by
/// file path, in the order each path was first touched. Derived directly
/// from `frame.items` -- the same way `transcript::latest_user_block_id`
/// is -- so the transcript's 200-block trailing window trim can never make
/// an already-applied change silently disappear from this overview.
pub(super) fn session_changes(frame: &AgentFrame) -> Vec<FileChange> {
    // call_id -> (block id, tool id, request input), captured at
    // `ToolCallRequested` and consumed once the matching `ToolCallFinished`
    // arrives. Cloned rather than borrowed so this function doesn't have to
    // thread `frame`'s lifetime through the map -- write/edit requests are
    // a small fraction of a session's items, so the clones are cheap.
    let mut pending: HashMap<ToolCallId, (usize, String, Value)> = HashMap::new();
    let mut changes: Vec<FileChange> = Vec::new();

    for (id, item) in frame.items.iter().enumerate() {
        match item {
            AgentFrameItem::ToolCallRequested(request)
                if matches!(request.tool_id.as_str(), "fs.edit" | "fs.write") =>
            {
                pending.insert(
                    request.call_id.clone(),
                    (id, request.tool_id.clone(), request.input.clone()),
                );
            }
            AgentFrameItem::ToolCallFinished(result) => {
                let Some((block_id, tool_id, input)) = pending.remove(&result.call_id) else {
                    continue;
                };
                if is_error_output(&result.output) {
                    continue;
                }
                let Some(path) = input.get("path").and_then(Value::as_str) else {
                    continue;
                };

                let (added, removed) = match tool_id.as_str() {
                    "fs.edit" => edit_stat(&input),
                    "fs.write" => write_stat(&input),
                    _ => (0, 0),
                };

                match changes.iter_mut().find(|change| change.path == path) {
                    Some(change) => {
                        change.edits += 1;
                        change.added += added;
                        change.removed += removed;
                        change.last_block_id = block_id;
                    }
                    None => changes.push(FileChange {
                        path: path.to_string(),
                        edits: 1,
                        added,
                        removed,
                        last_block_id: block_id,
                    }),
                }
            }
            _ => {}
        }
    }

    changes
}

/// `fs.edit`'s `+A -B`, reusing slice 1's `old_string`/`new_string` line
/// diff (`diff::diff_stat`) rather than re-deriving it.
fn edit_stat(input: &Value) -> (usize, usize) {
    let (Some(old), Some(new)) = (
        input.get("old_string").and_then(Value::as_str),
        input.get("new_string").and_then(Value::as_str),
    ) else {
        return (0, 0);
    };
    let stat = diff::diff_stat(&diff::line_diff(old, new));
    (stat.added, stat.removed)
}

/// `fs.write`'s approximate `+A`: the written content's line count, counted
/// as all-added regardless of whether the call created the file or
/// overwrote an existing one. A real overwrite diff would need the
/// previous file content, which Horizon doesn't have on hand here (only
/// the request's new content) -- this is a deliberate approximation, not a
/// diff against the prior file.
fn write_stat(input: &Value) -> (usize, usize) {
    let added = input
        .get("content")
        .and_then(Value::as_str)
        .map(|content| content.lines().count())
        .unwrap_or(0);
    (added, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{
        Event, MessageDelta, MessageRole, ToolCallRequest, ToolCallResult,
    };
    use crate::agent::frame::apply_agent_event_to_frame;

    fn edit_request(call_id: &str, path: &str, old: &str, new: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId(call_id.to_string()),
            tool_id: "fs.edit".to_string(),
            input: serde_json::json!({
                "path": path,
                "old_string": old,
                "new_string": new,
            }),
        })
    }

    fn write_request(call_id: &str, path: &str, content: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId(call_id.to_string()),
            tool_id: "fs.write".to_string(),
            input: serde_json::json!({
                "path": path,
                "content": content,
            }),
        })
    }

    fn finished(call_id: &str, output: Value) -> AgentFrameItem {
        AgentFrameItem::ToolCallFinished(ToolCallResult {
            call_id: ToolCallId(call_id.to_string()),
            output,
        })
    }

    #[test]
    fn a_single_finished_edit_produces_one_file_change() {
        let frame = AgentFrame {
            state: None,
            items: vec![
                edit_request("call-1", "src/lib.rs", "one\ntwo\n", "one\ntwo\nthree\n"),
                finished("call-1", serde_json::json!({ "replaced": true })),
            ],
        };

        let changes = session_changes(&frame);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "src/lib.rs");
        assert_eq!(changes[0].edits, 1);
        assert_eq!(changes[0].added, 1);
        assert_eq!(changes[0].removed, 0);
        assert_eq!(changes[0].last_block_id, 0);
    }

    #[test]
    fn repeated_edits_to_the_same_path_are_summed_into_one_row() {
        let frame = AgentFrame {
            state: None,
            items: vec![
                edit_request("call-1", "src/lib.rs", "a\n", "a\nb\n"),
                finished("call-1", serde_json::json!({ "replaced": true })),
                edit_request("call-2", "src/lib.rs", "a\nb\n", "a\nb\nc\n"),
                finished("call-2", serde_json::json!({ "replaced": true })),
            ],
        };

        let changes = session_changes(&frame);

        assert_eq!(changes.len(), 1, "same path must aggregate into one row");
        assert_eq!(changes[0].edits, 2);
        assert_eq!(changes[0].added, 2);
        assert_eq!(changes[0].removed, 0);
        // The most recent tool call's block id wins -- what a click should
        // jump to.
        assert_eq!(changes[0].last_block_id, 2);
    }

    #[test]
    fn a_created_write_counts_every_content_line_as_added() {
        let frame = AgentFrame {
            state: None,
            items: vec![
                write_request("call-1", "src/new.rs", "fn main() {}\nfn other() {}\n"),
                finished(
                    "call-1",
                    serde_json::json!({ "bytes_written": 10, "created": true }),
                ),
            ],
        };

        let changes = session_changes(&frame);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].added, 2);
        assert_eq!(changes[0].removed, 0);
    }

    #[test]
    fn an_overwrite_write_approximates_content_lines_as_added() {
        let frame = AgentFrame {
            state: None,
            items: vec![
                write_request("call-1", "src/existing.rs", "one line only\n"),
                finished(
                    "call-1",
                    serde_json::json!({ "bytes_written": 14, "created": false }),
                ),
            ],
        };

        let changes = session_changes(&frame);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].added, 1);
        assert_eq!(changes[0].removed, 0);
    }

    #[test]
    fn a_failed_call_is_excluded() {
        let frame = AgentFrame {
            state: None,
            items: vec![
                edit_request("call-1", "src/lib.rs", "a\n", "a\nb\n"),
                finished(
                    "call-1",
                    serde_json::json!({ "is_error": true, "message": "no match" }),
                ),
            ],
        };

        let changes = session_changes(&frame);

        assert!(
            changes.is_empty(),
            "an error result must not count as a change"
        );
    }

    #[test]
    fn a_non_write_tool_call_is_ignored() {
        let frame = AgentFrame {
            state: None,
            items: vec![
                AgentFrameItem::ToolCallRequested(ToolCallRequest {
                    call_id: ToolCallId("call-1".to_string()),
                    tool_id: "fs.read".to_string(),
                    input: serde_json::json!({ "path": "src/lib.rs" }),
                }),
                finished("call-1", serde_json::json!({ "content": "fn main() {}\n" })),
            ],
        };

        let changes = session_changes(&frame);

        assert!(changes.is_empty());
    }

    #[test]
    fn multiple_files_stay_in_first_touched_order() {
        let frame = AgentFrame {
            state: None,
            items: vec![
                edit_request("call-1", "b.rs", "a\n", "a\nb\n"),
                edit_request("call-2", "a.rs", "x\n", "x\ny\n"),
                finished("call-1", serde_json::json!({ "replaced": true })),
                finished("call-2", serde_json::json!({ "replaced": true })),
            ],
        };

        let changes = session_changes(&frame);

        assert_eq!(
            changes
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            vec!["b.rs", "a.rs"]
        );
    }

    #[test]
    fn changes_total_sums_files_and_diffstat() {
        let frame = AgentFrame {
            state: None,
            items: vec![
                edit_request("call-1", "a.rs", "a\n", "a\nb\nc\n"),
                finished("call-1", serde_json::json!({ "replaced": true })),
                write_request("call-2", "b.rs", "one\ntwo\n"),
                finished(
                    "call-2",
                    serde_json::json!({ "bytes_written": 8, "created": true }),
                ),
            ],
        };

        let total = changes_total(&session_changes(&frame));

        assert_eq!(
            total,
            ChangesTotal {
                files: 2,
                added: 4,
                removed: 0,
            }
        );
    }

    /// Pins the invariant `changes_bar_view`'s `items_revision` memo relies
    /// on (`src/agent/view/mod.rs`): once a streaming assistant turn has
    /// started, each further text delta coalesces into that same item in
    /// place (`apply_agent_event_to_frame`) rather than pushing a new one,
    /// so it must change neither `frame.items.len()` nor what
    /// `session_changes` reports. This is what lets that memo skip
    /// re-walking the item log on every streamed token after the first --
    /// the Changes-bar performance regression this change fixes. (The
    /// *first* delta of a turn is a real, if harmless, exception: it starts
    /// a new item because `ToolCallFinished` is a turn boundary
    /// (`is_turn_boundary_item`), so it does grow the count by one -- fine,
    /// since that's a single extra recompute per turn, not per token.)
    #[test]
    fn streaming_text_deltas_leave_item_count_and_changes_untouched() {
        let mut frame = AgentFrame::empty();

        apply_agent_event_to_frame(
            &mut frame,
            &Event::ToolCallRequested(ToolCallRequest {
                call_id: ToolCallId("call-1".to_string()),
                tool_id: "fs.edit".to_string(),
                input: serde_json::json!({
                    "path": "src/lib.rs",
                    "old_string": "a\n",
                    "new_string": "a\nb\n",
                }),
            }),
        );
        apply_agent_event_to_frame(
            &mut frame,
            &Event::ToolCallFinished(ToolCallResult {
                call_id: ToolCallId("call-1".to_string()),
                output: serde_json::json!({ "replaced": true }),
            }),
        );

        let before_changes = session_changes(&frame);

        // The first delta of the new turn starts a fresh item (see the doc
        // comment above) -- exercise that once, then pin the steady state
        // every later delta of the same turn must hold to.
        apply_agent_event_to_frame(
            &mut frame,
            &Event::AssistantTextDelta(MessageDelta {
                role: MessageRole::Assistant,
                text: "It ".to_string(),
            }),
        );
        let steady_state_len = frame.items.len();
        assert_eq!(
            session_changes(&frame),
            before_changes,
            "a text delta must never affect the Changes aggregation"
        );

        for chunk in ["looks ", "good."] {
            apply_agent_event_to_frame(
                &mut frame,
                &Event::AssistantTextDelta(MessageDelta {
                    role: MessageRole::Assistant,
                    text: chunk.to_string(),
                }),
            );

            assert_eq!(
                frame.items.len(),
                steady_state_len,
                "a delta continuing an in-progress turn must coalesce into \
                 the existing item, not push a new one"
            );
            assert_eq!(
                session_changes(&frame),
                before_changes,
                "streamed text must not change the Changes aggregation"
            );
        }
    }

    /// The other half of the `items_revision` invariant: a `ToolCallFinished`
    /// -- the only event that can add a new [`FileChange`] or grow an
    /// existing one -- is always a plain push in
    /// `apply_agent_event_to_frame`, never coalesced in place. So a change
    /// in `frame.items.len()` is a reliable (if not perfectly tight) signal
    /// that `session_changes`'s output may have changed.
    #[test]
    fn a_tool_call_finishing_always_grows_item_count() {
        let mut frame = AgentFrame::empty();
        apply_agent_event_to_frame(
            &mut frame,
            &Event::ToolCallRequested(ToolCallRequest {
                call_id: ToolCallId("call-1".to_string()),
                tool_id: "fs.edit".to_string(),
                input: serde_json::json!({
                    "path": "src/lib.rs",
                    "old_string": "a\n",
                    "new_string": "a\nb\n",
                }),
            }),
        );

        let before_len = frame.items.len();
        assert!(
            session_changes(&frame).is_empty(),
            "a requested-but-not-finished edit must not count yet"
        );

        apply_agent_event_to_frame(
            &mut frame,
            &Event::ToolCallFinished(ToolCallResult {
                call_id: ToolCallId("call-1".to_string()),
                output: serde_json::json!({ "replaced": true }),
            }),
        );

        assert!(
            frame.items.len() > before_len,
            "ToolCallFinished must grow item count so the structural-revision \
             proxy notices the Changes aggregation needs to be re-derived"
        );
        assert_eq!(session_changes(&frame).len(), 1);
    }
}
