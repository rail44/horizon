//! A tool call's expanded-row body (diff/content-preview/command/summary/
//! raw-JSON) and its terse one-line summary fallback. The per-call
//! view-model, approval-lifecycle derivation, and classifier
//! (`ToolCallView`/`ApprovalState`/`build_tool_call_views`/`ToolCallKind`/
//! `classify`) moved to `horizon_agent::transcript`, re-exported from
//! `super` under their original names (see `turns/mod.rs`'s doc comment).
//! This file's own contents stayed behind because [`build_tool_call_body`]'s
//! fallback arm for a terse, known-but-not-specially-bodied tool calls
//! [`terse_summary`], a wording function -- see `horizon_agent::
//! transcript`'s module doc for why that kept the whole family together
//! rather than splitting the enum from its one constructor.

use horizon_agent::contract::ToolCallId;
use horizon_agent::frame::AgentFrameItem;
use serde_json::Value;

use super::{cap_lines_head, cap_lines_tail, reconstruct_line_diff};
use super::{classify, str_field, DiffLine};

/// A tool call's expanded-row body (stage D, decision 3's "each row
/// expands further individually"), keyed off the tool id the same way
/// `ToolCallKind` is. Every line-list variant is already height-capped
/// by [`build_tool_call_body`]; the view additionally wraps them in a
/// scrollable, height-bounded container so one body can't swallow the
/// transcript. Deliberately reusable beyond the receipt: stage F's
/// failed-call log (running-card row) wants the same per-tool body
/// machinery, so this and [`tool_call_body`] take a plain item slice +
/// call id rather than anything receipt-specific.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolCallBody {
    /// fs.edit -- a reconstructed line diff; `omitted` counts any lines
    /// trimmed by the cap.
    Diff {
        lines: Vec<DiffLine>,
        omitted: usize,
    },
    /// fs.write -- a content preview labeled created/overwritten from the
    /// output, head-capped (the start of a new file matters most).
    ContentPreview {
        label: String,
        lines: Vec<String>,
        omitted: usize,
    },
    /// bash -- the command, its exit code (when the call didn't error
    /// before producing one), and captured output, tail-capped (the
    /// final pass/fail summary matters most -- mirrors
    /// `tools::bash::output::cap`'s own head/tail trade-off note).
    Command {
        command: String,
        exit_code: Option<i64>,
        lines: Vec<String>,
        omitted: usize,
    },
    /// fs.read/glob/grep and other known-but-terse tools -- one summary
    /// line (path + range, match counts, ...).
    Summary(String),
    /// An unrecognized tool id -- the base design's raw-JSON fallback,
    /// pretty-printed and head-capped.
    Raw { lines: Vec<String>, omitted: usize },
}

/// Diff body line cap -- generous, since a single `fs.edit` replacement is
/// normally small; guards against an unusually large one still bounding
/// the number of elements the view has to build.
const MAX_DIFF_LINES: usize = 300;
/// fs.write content-preview line cap (head-capped: the file's start
/// matters most for a preview).
const CONTENT_PREVIEW_MAX_LINES: usize = 200;
/// bash captured-output line cap (tail-capped: the final summary line
/// matters most, see `ToolCallBody::Command`'s doc comment).
const BASH_OUTPUT_TAIL_LINES: usize = 100;
/// Raw-JSON-fallback line cap (head-capped).
const RAW_FALLBACK_MAX_LINES: usize = 200;

/// The tool ids `classify` gives a dedicated verb/target/summary to --
/// shared with [`build_tool_call_body`] so a genuinely unrecognized tool
/// id (a future tool this crate hasn't been taught about yet) still falls
/// back to the raw-JSON body rather than a blank one, per decision 3's
/// "raw JSON pretty-print only as the unknown-tool fallback".
fn is_known_tool_id(tool_id: &str) -> bool {
    matches!(
        tool_id,
        "fs.edit"
            | "fs.write"
            | "fs.patch"
            | "bash"
            | "fs.read"
            | "fs.grep"
            | "fs.glob"
            | "workspace.snapshot"
            | "config.read"
            | "config.write"
            | "recall.search"
            | "recall.read"
            | "skill.read"
    )
}

/// A terse one-line summary for a known-but-not-specially-bodied tool
/// call. fs.read/grep/glob get shapes derived from their actual output
/// JSON (see `crates/horizon-agent/src/tools/fs/{read,grep,glob}.rs`);
/// every other known tool id falls back to `classify`'s own
/// verb/target/summary, reused rather than duplicated.
fn terse_summary(tool_id: &str, input: &Value, output: Option<&Value>) -> String {
    match tool_id {
        "fs.read" => {
            let path = str_field(input, "path").unwrap_or_default();
            let range = output.and_then(|output| {
                let start = output.get("start_line").and_then(Value::as_u64)?;
                let end = output.get("end_line").and_then(Value::as_u64)?;
                let total = output.get("total_lines").and_then(Value::as_u64)?;
                Some(format!("lines {start}-{end} of {total}"))
            });
            match range {
                Some(range) => format!("{path} · {range}"),
                None => path.to_string(),
            }
        }
        "fs.grep" => {
            let pattern = str_field(input, "pattern").unwrap_or_default();
            let base = str_field(input, "base_path").unwrap_or_default();
            let count = output
                .and_then(|output| output.get("returned_count"))
                .and_then(Value::as_u64);
            match count {
                Some(count) => format!("\"{pattern}\" in {base} · {count} matches"),
                None => format!("\"{pattern}\" in {base}"),
            }
        }
        "fs.glob" => {
            let pattern = str_field(input, "pattern").unwrap_or_default();
            let base = str_field(input, "base_path").unwrap_or_default();
            let count = output
                .and_then(|output| output.get("returned_count"))
                .and_then(Value::as_u64);
            match count {
                Some(count) => format!("{pattern} in {base} · {count} matches"),
                None => format!("{pattern} in {base}"),
            }
        }
        _ => {
            let (verb, target, result_summary, _kind) = classify(tool_id, input, output);
            match (target, result_summary) {
                (Some(target), Some(summary)) => format!("{verb} {target} · {summary}"),
                (Some(target), None) => format!("{verb} {target}"),
                (None, Some(summary)) => format!("{verb} · {summary}"),
                (None, None) => verb,
            }
        }
    }
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Builds the raw-JSON fallback body's lines for a tool id `classify`
/// doesn't recognize.
fn raw_json_fallback(tool_id: &str, input: &Value, output: Option<&Value>) -> (Vec<String>, usize) {
    let mut text = format!("{tool_id}\ninput: {}", pretty_json(input));
    if let Some(output) = output {
        text.push_str(&format!("\noutput: {}", pretty_json(output)));
    }
    cap_lines_head(
        text.lines().map(str::to_string).collect(),
        RAW_FALLBACK_MAX_LINES,
    )
}

/// Maps a tool call's id/input/(optional) output to its [`ToolCallBody`]
/// -- the per-tool body renderers of decision 3: fs.edit gets a
/// reconstructed diff, fs.write a content preview, bash a command+output
/// block, and every other known tool id a terse summary; a truly unknown
/// id falls back to raw JSON.
pub(crate) fn build_tool_call_body(
    tool_id: &str,
    input: &Value,
    output: Option<&Value>,
) -> ToolCallBody {
    match tool_id {
        "fs.edit" => {
            let old = str_field(input, "old_string").unwrap_or_default();
            let new = str_field(input, "new_string").unwrap_or_default();
            let (lines, omitted) = cap_lines_head(reconstruct_line_diff(old, new), MAX_DIFF_LINES);
            ToolCallBody::Diff { lines, omitted }
        }
        "fs.patch" => {
            let patch = str_field(input, "patch").unwrap_or_default();
            let lines = patch
                .lines()
                .filter_map(|line| {
                    if line.starts_with("***") || line.starts_with("@@") {
                        Some(horizon_agent::transcript::DiffLine {
                            kind: horizon_agent::transcript::DiffLineKind::Context,
                            text: line.to_string(),
                        })
                    } else if let Some(line) = line.strip_prefix('+') {
                        Some(horizon_agent::transcript::DiffLine {
                            kind: horizon_agent::transcript::DiffLineKind::Added,
                            text: line.to_string(),
                        })
                    } else if let Some(line) = line.strip_prefix('-') {
                        Some(horizon_agent::transcript::DiffLine {
                            kind: horizon_agent::transcript::DiffLineKind::Removed,
                            text: line.to_string(),
                        })
                    } else {
                        line.strip_prefix(' ')
                            .map(|line| horizon_agent::transcript::DiffLine {
                                kind: horizon_agent::transcript::DiffLineKind::Context,
                                text: line.to_string(),
                            })
                    }
                })
                .collect();
            let (lines, omitted) = cap_lines_head(lines, MAX_DIFF_LINES);
            ToolCallBody::Diff { lines, omitted }
        }
        "fs.write" => {
            let label = output
                .and_then(|output| output.get("created"))
                .and_then(Value::as_bool)
                .map(|created| if created { "created" } else { "overwritten" })
                .unwrap_or("written")
                .to_string();
            let content = str_field(input, "content").unwrap_or_default();
            let (lines, omitted) = cap_lines_head(
                content.lines().map(str::to_string).collect(),
                CONTENT_PREVIEW_MAX_LINES,
            );
            ToolCallBody::ContentPreview {
                label,
                lines,
                omitted,
            }
        }
        "bash" => {
            let command = str_field(input, "command").unwrap_or_default().to_string();
            let exit_code = output
                .and_then(|output| output.get("exit_code"))
                .and_then(Value::as_i64);
            let output_text = output
                .and_then(|output| output.get("output"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let all_lines: Vec<String> = output_text.lines().map(str::to_string).collect();
            let (lines, omitted) = cap_lines_tail(all_lines, BASH_OUTPUT_TAIL_LINES);
            ToolCallBody::Command {
                command,
                exit_code,
                lines,
                omitted,
            }
        }
        _ if is_known_tool_id(tool_id) => {
            ToolCallBody::Summary(terse_summary(tool_id, input, output))
        }
        _ => {
            let (lines, omitted) = raw_json_fallback(tool_id, input, output);
            ToolCallBody::Raw { lines, omitted }
        }
    }
}

/// Finds `call_id`'s request/result within `items` (a single turn's item
/// slice, same contract as `build_tool_call_views`) and builds its
/// [`ToolCallBody`]. `None` if `call_id` has no matching request in
/// `items` at all (shouldn't happen for a row the caller already built a
/// `ToolCallView` from).
///
/// `.rev()` on both lookups: same reused-call_id reasoning as
/// `build_tool_call_views` -- picks the *most recently requested*
/// occurrence's request/result, not a stale earlier one that happened to
/// share the id, so a `Waiting` row's proposal body always reflects the
/// call actually pending approval.
pub(crate) fn tool_call_body(
    items: &[AgentFrameItem],
    call_id: &ToolCallId,
) -> Option<ToolCallBody> {
    let request = items.iter().rev().find_map(|item| match item {
        AgentFrameItem::ToolCallRequested(request) if &request.call_id == call_id => Some(request),
        _ => None,
    })?;
    let result = items.iter().rev().find_map(|item| match item {
        AgentFrameItem::ToolCallFinished(result) if &result.call_id == call_id => Some(result),
        _ => None,
    });
    Some(build_tool_call_body(
        &request.tool_id,
        &request.input,
        result.map(|result| &result.output.0),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::test_support::*;
    use super::super::{build_tool_call_views, ApprovalState, DiffLineKind};
    use super::*;

    #[test]
    fn build_tool_call_body_reconstructs_an_fs_edit_diff() {
        let body = build_tool_call_body(
            "fs.edit",
            &json!({
                "path": "src/agent/view.rs",
                "old_string": "line1\nold\nline3",
                "new_string": "line1\nnew a\nnew b\nline3",
            }),
            Some(&json!({"path": "src/agent/view.rs", "replaced": true})),
        );
        match body {
            ToolCallBody::Diff { lines, omitted } => {
                assert_eq!(omitted, 0);
                assert_eq!(
                    diff_texts(&lines),
                    vec![
                        (DiffLineKind::Context, "line1"),
                        (DiffLineKind::Removed, "old"),
                        (DiffLineKind::Added, "new a"),
                        (DiffLineKind::Added, "new b"),
                        (DiffLineKind::Context, "line3"),
                    ]
                );
            }
            other => panic!("expected a Diff body, got {other:?}"),
        }
    }

    #[test]
    fn build_tool_call_body_labels_fs_write_created_vs_overwritten() {
        let created = build_tool_call_body(
            "fs.write",
            &json!({"path": "new.rs", "content": "fn main() {}"}),
            Some(&json!({"path": "new.rs", "bytes_written": 12, "created": true})),
        );
        match created {
            ToolCallBody::ContentPreview {
                label,
                lines,
                omitted,
            } => {
                assert_eq!(label, "created");
                assert_eq!(lines, vec!["fn main() {}".to_string()]);
                assert_eq!(omitted, 0);
            }
            other => panic!("expected a ContentPreview body, got {other:?}"),
        }

        let overwritten = build_tool_call_body(
            "fs.write",
            &json!({"path": "old.rs", "content": "x"}),
            Some(&json!({"path": "old.rs", "bytes_written": 1, "created": false})),
        );
        match overwritten {
            ToolCallBody::ContentPreview { label, .. } => assert_eq!(label, "overwritten"),
            other => panic!("expected a ContentPreview body, got {other:?}"),
        }
    }

    #[test]
    fn build_tool_call_body_renders_fs_patch_as_a_diff() {
        let body = build_tool_call_body(
            "fs.patch",
            &json!({
                "patch": "*** Begin Patch\n*** Update File: /w/a.rs\n@@\n-old\n+new\n*** End Patch"
            }),
            None,
        );
        let ToolCallBody::Diff { lines, omitted } = body else {
            panic!("expected patch diff body");
        };
        assert_eq!(omitted, 0);
        assert!(lines.iter().any(|line| {
            line.kind == horizon_agent::transcript::DiffLineKind::Removed && line.text == "old"
        }));
        assert!(lines.iter().any(|line| {
            line.kind == horizon_agent::transcript::DiffLineKind::Added && line.text == "new"
        }));
    }

    #[test]
    fn build_tool_call_body_carries_bash_command_exit_code_and_output() {
        let body = build_tool_call_body(
            "bash",
            &json!({"command": "cargo test"}),
            Some(&json!({"exit_code": 0, "output": "line1\nline2\n", "truncated": false})),
        );
        match body {
            ToolCallBody::Command {
                command,
                exit_code,
                lines,
                omitted,
            } => {
                assert_eq!(command, "cargo test");
                assert_eq!(exit_code, Some(0));
                assert_eq!(lines, vec!["line1".to_string(), "line2".to_string()]);
                assert_eq!(omitted, 0);
            }
            other => panic!("expected a Command body, got {other:?}"),
        }
    }

    #[test]
    fn build_tool_call_body_tail_caps_a_long_bash_output() {
        let output_text = (0..(BASH_OUTPUT_TAIL_LINES + 10))
            .map(|line_number| format!("line {line_number}"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = build_tool_call_body(
            "bash",
            &json!({"command": "seq"}),
            Some(&json!({"exit_code": 0, "output": output_text})),
        );
        match body {
            ToolCallBody::Command { lines, omitted, .. } => {
                assert_eq!(omitted, 10);
                assert_eq!(lines.len(), BASH_OUTPUT_TAIL_LINES);
                // The tail is kept, not the head.
                assert_eq!(lines.last().unwrap(), "line 109");
            }
            other => panic!("expected a Command body, got {other:?}"),
        }
    }

    #[test]
    fn build_tool_call_body_summarizes_fs_read_with_the_line_range() {
        let body = build_tool_call_body(
            "fs.read",
            &json!({"path": "src/lib.rs"}),
            Some(&json!({"start_line": 1, "end_line": 40, "total_lines": 120})),
        );
        assert_eq!(
            body,
            ToolCallBody::Summary("src/lib.rs · lines 1-40 of 120".to_string())
        );
    }

    #[test]
    fn build_tool_call_body_summarizes_fs_grep_with_the_match_count() {
        let body = build_tool_call_body(
            "fs.grep",
            &json!({"base_path": ".", "pattern": "notify"}),
            Some(&json!({"returned_count": 3})),
        );
        assert_eq!(
            body,
            ToolCallBody::Summary("\"notify\" in . · 3 matches".to_string())
        );
    }

    #[test]
    fn build_tool_call_body_summarizes_fs_glob_with_the_match_count() {
        let body = build_tool_call_body(
            "fs.glob",
            &json!({"base_path": ".", "pattern": "*.rs"}),
            Some(&json!({"returned_count": 5})),
        );
        assert_eq!(
            body,
            ToolCallBody::Summary("*.rs in . · 5 matches".to_string())
        );
    }

    #[test]
    fn build_tool_call_body_falls_back_to_raw_json_for_an_unknown_tool() {
        let body = build_tool_call_body(
            "some.future.tool",
            &json!({"foo": "bar"}),
            Some(&json!({"ok": true})),
        );
        match body {
            ToolCallBody::Raw { lines, omitted } => {
                assert_eq!(omitted, 0);
                let joined = lines.join("\n");
                assert!(joined.contains("some.future.tool"));
                assert!(joined.contains("\"foo\""));
                assert!(joined.contains("\"ok\""));
            }
            other => panic!("expected a Raw body, got {other:?}"),
        }
    }

    #[test]
    fn a_reused_call_id_still_shows_the_second_occurrence_as_waiting() {
        // Root-caused 2026-07-18: the owner's real agent session (a
        // rig/Kimi-K2.7-Code provider) reused the exact call_id
        // "functions.fs.edit:66" for two structurally different `fs.edit`
        // calls -- the first fully resolved (approved and finished
        // successfully) before the second was ever requested. Forward
        // `.find()` in `build_tool_call_views` kept attributing every
        // subsequent event for that call_id to the first (already
        // resolved) entry, so the second occurrence's own
        // `ApprovalRequested` never reached it: it stayed
        // `ApprovalState::None` (misread as "never needed approval")
        // forever, with no Approve/Deny row -- the session the owner had
        // to interrupt because no approval UI ever appeared, though the
        // daemon really was sitting in `WaitingForApproval`.
        let items = vec![
            tool_requested(
                "dup",
                "fs.edit",
                json!({"path": "a.rs", "old_string": "first old", "new_string": "first new"}),
            ),
            approval_requested("dup"),
            tool_started("dup"),
            tool_finished("dup", json!({"path": "a.rs", "replaced": true})),
            // A second, distinct call reuses the same call_id after the
            // first one's cycle is fully closed.
            tool_requested(
                "dup",
                "fs.edit",
                json!({"path": "b.rs", "old_string": "second old", "new_string": "second new"}),
            ),
            approval_requested("dup"),
            // No `ToolCallStarted`/`ToolCallFinished` yet for this second
            // occurrence: it's the one currently pending approval.
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(views.len(), 2);

        // The first occurrence keeps its own, correct resolution.
        assert_eq!(views[0].approval, ApprovalState::Approved);
        assert!(views[0].finished);
        assert_eq!(views[0].target.as_deref(), Some("a.rs"));

        // The second occurrence -- the actionable one -- must render as
        // `Waiting`, not `None`, so the UI shows Approve/Deny for it.
        assert_eq!(views[1].approval, ApprovalState::Waiting);
        assert!(!views[1].finished);
        assert_eq!(views[1].target.as_deref(), Some("b.rs"));

        // Its proposal body must reflect the *second* call's own content,
        // not the already-finished first one that happens to share the
        // id (`tool_call_body`'s matching `.rev()` fix).
        match tool_call_body(&items, &views[1].call_id) {
            Some(ToolCallBody::Diff { lines, .. }) => {
                assert_eq!(
                    diff_texts(&lines),
                    vec![
                        (DiffLineKind::Removed, "second old"),
                        (DiffLineKind::Added, "second new")
                    ]
                );
            }
            other => panic!("expected the second occurrence's own diff, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_body_finds_the_matching_call_within_a_turns_items() {
        let items = vec![
            tool_requested("a", "fs.read", json!({"path": "a.rs"})),
            tool_requested(
                "b",
                "fs.edit",
                json!({"path": "b.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("a", json!({"total_lines": 10})),
            tool_finished("b", json!({"path": "b.rs", "replaced": true})),
        ];
        let call_id = ToolCallId("b".to_string());
        match tool_call_body(&items, &call_id) {
            Some(ToolCallBody::Diff { lines, .. }) => {
                assert_eq!(
                    diff_texts(&lines),
                    vec![(DiffLineKind::Removed, "x"), (DiffLineKind::Added, "y")]
                );
            }
            other => panic!("expected a Diff body for call `b`, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_body_is_none_for_an_unknown_call_id() {
        let items = vec![tool_requested("a", "fs.read", json!({"path": "a.rs"}))];
        let call_id = ToolCallId("missing".to_string());
        assert!(tool_call_body(&items, &call_id).is_none());
    }

    #[test]
    fn tool_call_body_for_a_waiting_bash_call_carries_the_full_command_not_the_row_head() {
        // Row-centric approval v2: a `Waiting` row auto-displays this body
        // as its proposal (decision 4's "proposal — not applied") before
        // any `ToolCallFinished` exists -- unlike `ToolCallKind::Bash`'s
        // `command_head` (the row's own collapsed line and the receipt
        // chip), which truncates to the first line's first 32 characters
        // (see `bash_chip_carries_a_truncated_command_head`, now in
        // `horizon_agent::transcript::tool_call::tests` alongside
        // `ToolCallKind`).
        let long_command = format!("echo {}", "x".repeat(50));
        let items = vec![
            tool_requested("a", "bash", json!({"command": long_command})),
            approval_requested("a"),
        ];
        match tool_call_body(&items, &ToolCallId("a".to_string())) {
            Some(ToolCallBody::Command {
                command, exit_code, ..
            }) => {
                assert_eq!(command, format!("echo {}", "x".repeat(50)));
                assert!(command.chars().count() > 32);
                assert_eq!(exit_code, None);
            }
            other => panic!("expected a Command body, got {other:?}"),
        }
    }
}
