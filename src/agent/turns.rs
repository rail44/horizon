//! Pure view-model for turn grouping and receipt summarization
//! (`docs/agent-output-ui-amendment.md` stage C, decisions 1-2). Kept
//! separate from `view.rs` so the grouping/aggregation logic has
//! colocated tests independent of GPUI rendering, and out of
//! `horizon-agent` so that crate stays UI-agnostic (verb naming, chip
//! composition, and humanized durations are display concerns, not
//! contract ones).

use std::path::Path;
use std::time::Duration;

use horizon_agent::contract::{Message, MessageRole, ToolCallId, ToolCallResult, TurnEndReason};
use horizon_agent::frame::{pending_approval_call_ids_in, AgentFrameItem};
use serde_json::Value;

/// One turn's items, sliced from `AgentFrame::items` by index range
/// `[start, end)`. `ended` is `None` for the turn currently in
/// progress -- the last span produced by [`group_into_turns`], and only
/// meaningful to render as such while the session state indicates a turn
/// is in flight (`horizon_agent::frame::state_indicates_turn_in_flight`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnSpan {
    pub start: usize,
    pub end: usize,
    pub ended: Option<TurnEnd>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnEnd {
    pub reason: TurnEndReason,
    pub model: Option<String>,
    pub elapsed: Duration,
}

/// Groups `items` into turn segments: a segment opens at a user
/// `Message` and closes at the next `TurnEnded` (inclusive). A trailing
/// segment with no closing `TurnEnded` yet is the turn in progress.
///
/// Defensive: if a user `Message` opens a new segment before the
/// previous one saw a `TurnEnded` (shouldn't happen by contract -- the
/// session loop never sends a new turn until the previous one settled),
/// the stale segment is closed with `ended: None` rather than silently
/// merging into the new one, so no items are dropped.
pub(crate) fn group_into_turns(items: &[AgentFrameItem]) -> Vec<TurnSpan> {
    let mut spans = Vec::new();
    let mut current_start: Option<usize> = None;
    for (index, item) in items.iter().enumerate() {
        if is_user_message(item) {
            if let Some(start) = current_start.take() {
                spans.push(TurnSpan {
                    start,
                    end: index,
                    ended: None,
                });
            }
            current_start = Some(index);
        }
        if let AgentFrameItem::TurnEnded {
            reason,
            model,
            elapsed,
        } = item
        {
            let start = current_start.take().unwrap_or(index);
            spans.push(TurnSpan {
                start,
                end: index + 1,
                ended: Some(TurnEnd {
                    reason: *reason,
                    model: model.clone(),
                    elapsed: *elapsed,
                }),
            });
        }
    }
    if let Some(start) = current_start {
        spans.push(TurnSpan {
            start,
            end: items.len(),
            ended: None,
        });
    }
    spans
}

fn is_user_message(item: &AgentFrameItem) -> bool {
    matches!(
        item,
        AgentFrameItem::Message(Message {
            role: MessageRole::User,
            ..
        })
    )
}

/// A turn's end-reason rendered as receipt status text -- the
/// `Cancelled` -> `stopped · {elapsed}` / `Failed`/`Halted` ->
/// error-marked variants from decision 1's end-reason handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceiptStatus {
    pub text: String,
    pub is_error: bool,
}

pub(crate) fn receipt_status(end: &TurnEnd) -> ReceiptStatus {
    let elapsed = humanize_duration(end.elapsed);
    match end.reason {
        TurnEndReason::Completed => ReceiptStatus {
            text: elapsed,
            is_error: false,
        },
        TurnEndReason::Cancelled => ReceiptStatus {
            text: format!("stopped · {elapsed}"),
            is_error: false,
        },
        TurnEndReason::Failed => ReceiptStatus {
            text: format!("failed · {elapsed}"),
            is_error: true,
        },
        TurnEndReason::Halted => ReceiptStatus {
            text: format!("halted · {elapsed}"),
            is_error: true,
        },
    }
}

/// Humanizes a duration the way the receipt/running-card elapsed field
/// wants it: `38s`, `2m 05s`. Whole seconds only -- sub-second precision
/// isn't meaningful at this display granularity.
pub(crate) fn humanize_duration(elapsed: Duration) -> String {
    let total_secs = elapsed.as_secs();
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// Structured, tool-specific data a receipt chip or running-card row
/// needs beyond the generic verb/target/summary -- the file-chip
/// diffstat and the bash chip's command head (decision 1's chip
/// composition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolCallKind {
    Generic,
    File {
        file_name: String,
        /// `(added, removed)` line counts, derived from `old_string`/
        /// `new_string` for `fs.edit`. `None` when not derivable (e.g.
        /// `fs.write`, which replaces wholesale rather than diffing).
        diffstat: Option<(u32, u32)>,
    },
    Bash {
        command_head: String,
    },
}

/// One tool call's view-model, shared by the running card's per-row
/// rendering (full `verb + target + result summary` line, one row per
/// call) and the completed-turn receipt's chip rendering (terser, keyed
/// off `kind`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolCallView {
    pub call_id: ToolCallId,
    pub verb: String,
    pub target: Option<String>,
    /// Set once the call has finished; a still-running call has no
    /// result to summarize yet.
    pub result_summary: Option<String>,
    pub kind: ToolCallKind,
    pub finished: bool,
    pub is_error: bool,
}

/// Builds one [`ToolCallView`] per distinct tool call requested within
/// `items` (a single turn span's slice), in first-request order. A call
/// with no matching `ToolCallFinished` yet (the running turn's
/// in-flight calls) gets `finished: false` and no result summary.
pub(crate) fn build_tool_call_views(items: &[AgentFrameItem]) -> Vec<ToolCallView> {
    struct Building<'a> {
        call_id: ToolCallId,
        tool_id: &'a str,
        input: &'a Value,
        result: Option<&'a ToolCallResult>,
    }

    let mut building: Vec<Building> = Vec::new();
    for item in items {
        match item {
            AgentFrameItem::ToolCallRequested(request) => {
                building.push(Building {
                    call_id: request.call_id.clone(),
                    tool_id: &request.tool_id,
                    input: &request.input,
                    result: None,
                });
            }
            AgentFrameItem::ToolCallFinished(result) => {
                if let Some(entry) = building
                    .iter_mut()
                    .find(|entry| entry.call_id == result.call_id)
                {
                    entry.result = Some(result);
                }
            }
            _ => {}
        }
    }

    building
        .into_iter()
        .map(|entry| {
            let output = entry.result.map(|result| &result.output);
            let (verb, target, result_summary, kind) = classify(entry.tool_id, entry.input, output);
            ToolCallView {
                call_id: entry.call_id,
                verb,
                target,
                result_summary: if entry.result.is_some() {
                    result_summary
                } else {
                    None
                },
                kind,
                finished: entry.result.is_some(),
                is_error: entry.result.map(|result| result.is_error).unwrap_or(false),
            }
        })
        .collect()
}

/// Whether `call_id`'s approval request is still unresolved within
/// `turn_items` -- a single turn's own item slice is enough to answer
/// this without consulting the whole frame: every tool call this crate
/// emits, Horizon-executed or provider-forwarded, resolves via a
/// `ToolCallFinished` with the same `call_id` (see
/// `crates/horizon-agent/src/tools/approval.rs`'s `synchronous_result`,
/// the one path every approve/deny decision funnels through) before its
/// turn can end in the normal case, so the resolving item -- if any --
/// already lives in the same span as the request. A turn that ends with
/// a still-pending approval (e.g. `Halted`) is the shouldn't-happen case
/// this stays `true` for, so a completed turn still renders it rather
/// than silently dropping it (`docs/agent-output-ui-amendment.md` stage
/// C's owner-reported fold bug: answered approvals must fold into the
/// receipt like any other tool activity, not linger as boxes forever).
pub(crate) fn is_approval_still_pending(
    turn_items: &[AgentFrameItem],
    call_id: &ToolCallId,
) -> bool {
    pending_approval_call_ids_in(turn_items).contains(call_id)
}

/// `(finished, total)` tool-call counts for a running card's `n / m`
/// progress header.
pub(crate) fn progress(tool_calls: &[ToolCallView]) -> (usize, usize) {
    let finished = tool_calls.iter().filter(|call| call.finished).count();
    (finished, tool_calls.len())
}

/// Maps a tool id to its display verb, target, (would-be) result
/// summary, and any tool-specific structured data -- the one place that
/// knows the exact input/output JSON shape each tool in
/// `crates/horizon-agent/src/tools` uses (see that crate's `tools/fs`,
/// `tools/bash` modules). Unknown tool ids fall back to the raw id as
/// the verb with no target/summary, so a future tool renders *something*
/// sane rather than nothing.
fn classify(
    tool_id: &str,
    input: &Value,
    output: Option<&Value>,
) -> (String, Option<String>, Option<String>, ToolCallKind) {
    match tool_id {
        "fs.edit" => {
            let path = str_field(input, "path").unwrap_or_default().to_string();
            let old = str_field(input, "old_string").unwrap_or_default();
            let new = str_field(input, "new_string").unwrap_or_default();
            let diffstat = Some(line_diffstat(old, new));
            let summary = diffstat.map(|(added, removed)| format!("+{added} -{removed}"));
            (
                "Edit".to_string(),
                Some(path.clone()),
                summary,
                ToolCallKind::File {
                    file_name: file_name(&path),
                    diffstat,
                },
            )
        }
        "fs.write" => {
            let path = str_field(input, "path").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("created"))
                .and_then(Value::as_bool)
                .map(|created| {
                    if created {
                        "created".to_string()
                    } else {
                        "overwritten".to_string()
                    }
                });
            (
                "Write".to_string(),
                Some(path.clone()),
                summary,
                ToolCallKind::File {
                    file_name: file_name(&path),
                    diffstat: None,
                },
            )
        }
        "bash" => {
            let command = str_field(input, "command").unwrap_or_default();
            let head = command_head(command);
            let summary = output
                .and_then(|output| output.get("exit_code"))
                .and_then(Value::as_i64)
                .map(|code| format!("exit {code}"));
            (
                "Bash".to_string(),
                Some(head.clone()),
                summary,
                ToolCallKind::Bash { command_head: head },
            )
        }
        "fs.read" => {
            let path = str_field(input, "path").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("total_lines"))
                .and_then(Value::as_u64)
                .map(|lines| format!("{lines} lines"));
            (
                "Read".to_string(),
                Some(path),
                summary,
                ToolCallKind::Generic,
            )
        }
        "fs.grep" => {
            let pattern = str_field(input, "pattern").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("returned_count"))
                .and_then(Value::as_u64)
                .map(|count| format!("{count} matches"));
            (
                "Grep".to_string(),
                Some(pattern),
                summary,
                ToolCallKind::Generic,
            )
        }
        "fs.glob" => {
            let pattern = str_field(input, "pattern").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("returned_count"))
                .and_then(Value::as_u64)
                .map(|count| format!("{count} matches"));
            (
                "Glob".to_string(),
                Some(pattern),
                summary,
                ToolCallKind::Generic,
            )
        }
        "workspace.snapshot" => ("Snapshot".to_string(), None, None, ToolCallKind::Generic),
        "config.read" => ("Config Read".to_string(), None, None, ToolCallKind::Generic),
        "config.write" => (
            "Config Write".to_string(),
            None,
            None,
            ToolCallKind::Generic,
        ),
        "recall.search" => (
            "Recall Search".to_string(),
            None,
            None,
            ToolCallKind::Generic,
        ),
        "recall.read" => ("Recall Read".to_string(), None, None, ToolCallKind::Generic),
        "skill.read" => {
            let id = str_field(input, "id").unwrap_or_default().to_string();
            ("Skill".to_string(), Some(id), None, ToolCallKind::Generic)
        }
        other => (other.to_string(), None, None, ToolCallKind::Generic),
    }
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

/// First line of `command`, truncated to a display-friendly length.
fn command_head(command: &str) -> String {
    let first_line = command.lines().next().unwrap_or("");
    truncate_chars(first_line, 32)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let head: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// A simple common-prefix/common-suffix line diffstat between `old` and
/// `new` -- not a full diff algorithm (no interior-line matching), but
/// enough to report `+added -removed` for `fs.edit`'s single
/// old_string/new_string replacement, which is the shape every `fs.edit`
/// call has today (see `crates/horizon-agent/src/tools/fs/edit.rs`).
fn line_diffstat(old: &str, new: &str) -> (u32, u32) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let removed = (old_lines.len() - prefix - suffix) as u32;
    let added = (new_lines.len() - prefix - suffix) as u32;
    (added, removed)
}

#[cfg(test)]
mod tests {
    use horizon_agent::contract::{ApprovalRequest, ToolCallId, ToolCallRequest, ToolCallResult};
    use serde_json::json;

    use super::*;

    fn user_message(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            role: MessageRole::User,
            text: text.to_string(),
        })
    }

    fn assistant_message(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    fn tool_requested(call_id: &str, tool_id: &str, input: Value) -> AgentFrameItem {
        AgentFrameItem::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId(call_id.to_string()),
            tool_id: tool_id.to_string(),
            input,
        })
    }

    fn tool_finished(call_id: &str, output: Value) -> AgentFrameItem {
        AgentFrameItem::ToolCallFinished(ToolCallResult::new(
            ToolCallId(call_id.to_string()),
            output,
        ))
    }

    fn turn_ended(reason: TurnEndReason, model: Option<&str>, elapsed_secs: u64) -> AgentFrameItem {
        AgentFrameItem::TurnEnded {
            reason,
            model: model.map(str::to_string),
            elapsed: Duration::from_secs(elapsed_secs),
        }
    }

    #[test]
    fn groups_a_completed_turn_followed_by_a_running_one() {
        let items = vec![
            user_message("fix the bug"),
            tool_requested(
                "a",
                "fs.grep",
                json!({"base_path": ".", "pattern": "notify"}),
            ),
            tool_finished("a", json!({"returned_count": 1})),
            assistant_message("fixed it"),
            turn_ended(TurnEndReason::Completed, Some("gpt-5"), 38),
            user_message("check the other form too"),
            tool_requested("b", "fs.read", json!({"path": "signup_form.rs"})),
        ];

        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 2);

        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, 5); // inclusive of TurnEnded
        let ended = spans[0].ended.as_ref().expect("first turn settled");
        assert_eq!(ended.reason, TurnEndReason::Completed);
        assert_eq!(ended.model.as_deref(), Some("gpt-5"));
        assert_eq!(ended.elapsed, Duration::from_secs(38));

        assert_eq!(spans[1].start, 5);
        assert_eq!(spans[1].end, 7);
        assert!(spans[1].ended.is_none());
    }

    #[test]
    fn a_turn_with_no_tool_calls_still_groups_and_has_no_chips() {
        let items = vec![
            user_message("hello"),
            assistant_message("hi"),
            turn_ended(TurnEndReason::Completed, None, 2),
        ];
        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        assert!(span.ended.is_some());
        assert!(build_tool_call_views(&items[span.start..span.end]).is_empty());
    }

    #[test]
    fn a_stray_boundary_without_turn_ended_closes_as_unended_rather_than_dropping_items() {
        // Defensive case only -- shouldn't happen by contract.
        let items = vec![user_message("first"), user_message("second")];
        let spans = group_into_turns(&items);
        assert_eq!(spans.len(), 2);
        assert_eq!(
            spans[0],
            TurnSpan {
                start: 0,
                end: 1,
                ended: None
            }
        );
        assert_eq!(spans[1].start, 1);
        assert_eq!(spans[1].end, 2);
        assert!(spans[1].ended.is_none());
    }

    #[test]
    fn receipt_status_covers_every_end_reason() {
        let end = |reason| TurnEnd {
            reason,
            model: None,
            elapsed: Duration::from_secs(38),
        };
        assert_eq!(
            receipt_status(&end(TurnEndReason::Completed)),
            ReceiptStatus {
                text: "38s".to_string(),
                is_error: false
            }
        );
        assert_eq!(
            receipt_status(&end(TurnEndReason::Cancelled)),
            ReceiptStatus {
                text: "stopped · 38s".to_string(),
                is_error: false
            }
        );
        assert_eq!(
            receipt_status(&end(TurnEndReason::Failed)),
            ReceiptStatus {
                text: "failed · 38s".to_string(),
                is_error: true
            }
        );
        assert_eq!(
            receipt_status(&end(TurnEndReason::Halted)),
            ReceiptStatus {
                text: "halted · 38s".to_string(),
                is_error: true
            }
        );
    }

    #[test]
    fn humanize_duration_matches_the_docs_examples() {
        assert_eq!(humanize_duration(Duration::from_secs(0)), "0s");
        assert_eq!(humanize_duration(Duration::from_secs(38)), "38s");
        assert_eq!(humanize_duration(Duration::from_secs(59)), "59s");
        assert_eq!(humanize_duration(Duration::from_secs(60)), "1m 00s");
        assert_eq!(humanize_duration(Duration::from_secs(125)), "2m 05s");
    }

    #[test]
    fn build_tool_call_views_pairs_requests_with_their_results_in_request_order() {
        let items = vec![
            tool_requested("a", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_requested("b", "fs.read", json!({"path": "src/lib.rs"})),
            tool_finished("a", json!({"returned_count": 3})),
            tool_finished("b", json!({"total_lines": 40})),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].call_id, ToolCallId("a".to_string()));
        assert_eq!(views[0].verb, "Grep");
        assert_eq!(views[0].result_summary.as_deref(), Some("3 matches"));
        assert!(views[0].finished);
        assert!(!views[0].is_error);

        assert_eq!(views[1].call_id, ToolCallId("b".to_string()));
        assert_eq!(views[1].verb, "Read");
        assert_eq!(views[1].result_summary.as_deref(), Some("40 lines"));
    }

    #[test]
    fn a_still_running_tool_call_has_no_result_summary() {
        let items = vec![tool_requested(
            "a",
            "bash",
            json!({"command": "cargo test"}),
        )];
        let views = build_tool_call_views(&items);
        assert_eq!(views.len(), 1);
        assert!(!views[0].finished);
        assert!(views[0].result_summary.is_none());
        assert!(!views[0].is_error);
    }

    #[test]
    fn an_errored_tool_call_is_marked_is_error_via_the_output_convention() {
        let items = vec![
            tool_requested("a", "bash", json!({"command": "cargo test"})),
            tool_finished(
                "a",
                json!({"is_error": true, "message": "boom", "exit_code": 1}),
            ),
        ];
        let views = build_tool_call_views(&items);
        assert!(views[0].is_error);
        assert_eq!(views[0].result_summary.as_deref(), Some("exit 1"));
    }

    #[test]
    fn fs_edit_derives_a_diffstat_from_old_and_new_string() {
        let items = vec![
            tool_requested(
                "a",
                "fs.edit",
                json!({
                    "path": "src/agent/view.rs",
                    "old_string": "line1\nold\nline3",
                    "new_string": "line1\nnew a\nnew b\nline3",
                }),
            ),
            tool_finished("a", json!({"path": "src/agent/view.rs", "replaced": true})),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].verb, "Edit");
        assert_eq!(views[0].target.as_deref(), Some("src/agent/view.rs"));
        assert_eq!(views[0].result_summary.as_deref(), Some("+2 -1"));
        match &views[0].kind {
            ToolCallKind::File {
                file_name,
                diffstat,
            } => {
                assert_eq!(file_name, "view.rs");
                assert_eq!(*diffstat, Some((2, 1)));
            }
            other => panic!("expected a File chip, got {other:?}"),
        }
    }

    #[test]
    fn fs_write_reports_created_vs_overwritten_with_no_diffstat() {
        let items = vec![
            tool_requested(
                "a",
                "fs.write",
                json!({"path": "new.rs", "content": "fn main() {}"}),
            ),
            tool_finished(
                "a",
                json!({"path": "new.rs", "bytes_written": 12, "created": true}),
            ),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views[0].verb, "Write");
        assert_eq!(views[0].result_summary.as_deref(), Some("created"));
        match &views[0].kind {
            ToolCallKind::File { diffstat, .. } => assert_eq!(*diffstat, None),
            other => panic!("expected a File chip, got {other:?}"),
        }
    }

    #[test]
    fn bash_chip_carries_a_truncated_command_head() {
        let long_command = "cargo test --workspace --all-targets -- --nocapture and-then-some-more";
        let items = vec![tool_requested(
            "a",
            "bash",
            json!({"command": long_command}),
        )];
        let views = build_tool_call_views(&items);
        match &views[0].kind {
            ToolCallKind::Bash { command_head } => {
                assert!(command_head.ends_with('…'));
                assert!(command_head.chars().count() <= 32);
            }
            other => panic!("expected a Bash chip, got {other:?}"),
        }
    }

    #[test]
    fn progress_counts_finished_vs_total_tool_calls() {
        let items = vec![
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_requested("b", "fs.read", json!({"path": "b.rs"})),
            tool_requested("c", "fs.read", json!({"path": "c.rs"})),
            tool_finished("a", json!({"total_lines": 1})),
            tool_finished("b", json!({"total_lines": 1})),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(progress(&views), (2, 3));
    }

    fn approval_requested(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ApprovalRequested(ApprovalRequest {
            call_id: ToolCallId(call_id.to_string()),
            reason: "writes a file".to_string(),
        })
    }

    #[test]
    fn a_resolved_approval_within_the_turn_is_no_longer_pending() {
        let call_id = ToolCallId("a".to_string());
        let items = vec![
            approval_requested("a"),
            tool_finished("a", json!({"path": "x.rs", "replaced": true})),
        ];
        assert!(!is_approval_still_pending(&items, &call_id));
    }

    #[test]
    fn an_unresolved_approval_is_still_pending_defensively() {
        // Shouldn't happen by contract (a turn shouldn't end with a
        // dangling approval), but a `Halted`/`Cancelled` turn could leave
        // one -- the completed-turn receipt still renders it rather than
        // silently dropping it.
        let call_id = ToolCallId("a".to_string());
        let items = vec![approval_requested("a")];
        assert!(is_approval_still_pending(&items, &call_id));
    }
}
