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

use horizon_agent::contract::tool_output::{
    decode, BashOutput, EditOutcome, FileEdits, FileRead, FileWritten, Location, Matches,
};
use horizon_agent::frame::AgentFrameItem;
use horizon_agent::transcript::ToolCallClassification;
use serde_json::Value;

use super::{cap_lines_head, cap_lines_tail, reconstruct_line_diff};
use super::{classify, edit_entries, str_field, DiffLine, DiffLineKind, ToolCallView};

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
    /// fs.edit -- one reconstructed line diff per edit in the call's
    /// `edits` list, concatenated; `omitted` counts any lines trimmed by
    /// the cap.
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

/// Diff body line cap -- generous, since an `fs.edit` batch's replacements
/// are normally small; guards against an unusually large one still bounding
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

/// A terse one-line summary for a known-but-not-specially-bodied tool
/// call. fs.read/grep/glob get shapes derived from their actual output
/// JSON (see `crates/horizon-agent/src/tools/fs/{read,grep,glob}.rs`);
/// every other known tool id falls back to `classify`'s own
/// verb/target/summary, reused rather than duplicated.
fn terse_summary(
    tool_id: &str,
    input: &Value,
    output: Option<&Value>,
    classification: ToolCallClassification,
) -> String {
    match tool_id {
        "fs.read" => {
            let path = str_field(input, "path").unwrap_or_default();
            let range = output.and_then(decode::<FileRead>).map(|result| {
                format!(
                    "lines {}-{} of {}",
                    result.start_line, result.end_line, result.total_lines
                )
            });
            match range {
                Some(range) => format!("{path} · {range}"),
                None => path.to_string(),
            }
        }
        "fs.grep" | "fs.glob" => {
            let pattern = str_field(input, "pattern").unwrap_or_default();
            let pattern = if tool_id == "fs.grep" {
                format!("\"{pattern}\"")
            } else {
                pattern.to_string()
            };
            let base = str_field(input, "base_path").unwrap_or_default();
            let count = output.and_then(|output| {
                if tool_id == "fs.grep" {
                    decode::<Matches<Location>>(output).map(|result| result.returned_count)
                } else {
                    decode::<Matches<String>>(output).map(|result| result.returned_count)
                }
            });
            match count {
                Some(count) => format!("{pattern} in {base} · {count} matches"),
                None => format!("{pattern} in {base}"),
            }
        }
        _ => {
            let ToolCallClassification {
                verb,
                target,
                summary: result_summary,
                ..
            } = classification;
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

fn raw_body(tool_id: &str, input: &Value, output: Option<&Value>) -> ToolCallBody {
    let (lines, omitted) = raw_json_fallback(tool_id, input, output);
    ToolCallBody::Raw { lines, omitted }
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
            // One diff per edit, concatenated in list order. A batch gets a
            // `--- <path>` header line before each edit so the reader can
            // tell which file (and which hunk of it) a run of lines belongs
            // to; a single edit keeps the bare diff it always had.
            let edits = edit_entries(input);
            let results = output.and_then(decode::<FileEdits>);
            if output.is_some() && results.is_none() {
                return raw_body(tool_id, input, output);
            }
            let labeled =
                edits.len() > 1 || results.as_ref().is_some_and(|r| r.failed_index.is_some());
            let mut all_lines = Vec::new();
            for (index, edit) in edits.iter().enumerate() {
                let outcome = results
                    .as_ref()
                    .and_then(|result| {
                        result
                            .edits
                            .iter()
                            .find(|receipt| receipt.index == index && receipt.path == edit.path)
                    })
                    .map(|receipt| &receipt.outcome);
                let label = match outcome {
                    Some(EditOutcome::Applied { occurrences, .. }) => {
                        format!("applied ({occurrences} replacements)")
                    }
                    Some(EditOutcome::Failed { message }) => format!("failed: {message}"),
                    Some(EditOutcome::NotAttempted) => "not attempted".into(),
                    None if results.is_some() => "no recorded result".into(),
                    None => "proposed".into(),
                };
                if labeled {
                    all_lines.push(DiffLine {
                        kind: DiffLineKind::Context,
                        text: format!("--- {} · {label}", edit.path),
                    });
                }
                if matches!(outcome, Some(EditOutcome::Applied { .. })) || results.is_none() {
                    all_lines.extend(reconstruct_line_diff(edit.old_string, edit.new_string));
                }
            }
            let (lines, omitted) = cap_lines_head(all_lines, MAX_DIFF_LINES);
            ToolCallBody::Diff { lines, omitted }
        }
        "fs.write" => {
            let result = output.and_then(decode::<FileWritten>);
            if output.is_some() && result.is_none() {
                return raw_body(tool_id, input, output);
            }
            let label = result
                .map(|result| {
                    if result.created {
                        "created"
                    } else {
                        "overwritten"
                    }
                })
                .unwrap_or("proposed")
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
            let result = output.and_then(decode::<BashOutput>);
            if output.is_some() && result.is_none() {
                return raw_body(tool_id, input, output);
            }
            let exit_code = result.as_ref().and_then(BashOutput::exit_code);
            let output_text = result
                .as_ref()
                .map(|result| match &result.message {
                    Some(message) => format!("{message}\n{}", result.output),
                    None => result.output.clone(),
                })
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
        _ => {
            let classification = classify(tool_id, input, output);
            if classification.known {
                ToolCallBody::Summary(terse_summary(tool_id, input, output, classification))
            } else {
                let (lines, omitted) = raw_json_fallback(tool_id, input, output);
                ToolCallBody::Raw { lines, omitted }
            }
        }
    }
}

/// Builds a row's body from the request/result positions already correlated by
/// `build_tool_call_views`. `items` must be the same slice used to build `call`.
/// This preserves occurrence-aware matching and legacy replay ordering without
/// letting the view independently bind a reused call id to a different attempt.
pub(crate) fn tool_call_body(
    items: &[AgentFrameItem],
    call: &ToolCallView,
) -> Option<ToolCallBody> {
    let AgentFrameItem::ToolCallRequested(request) = items.get(call.request_index)? else {
        return None;
    };
    let output = match call.result_index {
        Some(index) => {
            let AgentFrameItem::ToolCallFinished(result) = items.get(index)? else {
                return None;
            };
            Some(&result.output.0)
        }
        None => None,
    };
    Some(build_tool_call_body(
        &request.tool_id,
        &request.input,
        output,
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::test_support::*;
    use super::super::{build_tool_call_views, ApprovalState, DiffLineKind};
    use super::*;

    #[test]
    fn expanded_bodies_keep_each_reused_call_occurrence_and_pending_result_separate() {
        {
            let mut items = vec![
                tool_requested("dup", "bash", json!({"command": "echo first"})),
                tool_finished(
                    "dup",
                    json!({"exit_code": 0, "output": "first", "termination": "exited", "output_file": null, "truncated": false}),
                ),
                tool_requested("dup", "bash", json!({"command": "echo second"})),
            ];
            {
                use horizon_agent::contract::OccurrenceId;
                for (index, item) in items.iter_mut().enumerate() {
                    let occurrence =
                        OccurrenceId(if index < 2 { "first" } else { "second" }.into());
                    match item {
                        AgentFrameItem::ToolCallRequested(request) => {
                            request.occurrence_id = occurrence
                        }
                        AgentFrameItem::ToolCallFinished(result) => {
                            result.occurrence_id = occurrence
                        }
                        _ => unreachable!(),
                    }
                }
            }
            let views = build_tool_call_views(&items);
            assert_eq!(
                tool_call_body(&items, &views[0]),
                Some(ToolCallBody::Command {
                    command: "echo first".into(),
                    exit_code: Some(0),
                    lines: vec!["first".into()],
                    omitted: 0,
                }),
                "the completed row must retain its own request and output",
            );
            assert_eq!(
                tool_call_body(&items, &views[1]),
                Some(ToolCallBody::Command {
                    command: "echo second".into(),
                    exit_code: None,
                    lines: vec![],
                    omitted: 0,
                }),
                "a pending row must not borrow the previous output",
            );
        }
    }

    #[test]
    fn expanded_body_follows_occurrence_binding_when_an_old_attempt_finishes_last() {
        use horizon_agent::contract::{OccurrenceId, ToolCallId, ToolCallResult};

        let mut items = vec![
            tool_requested("dup", "bash", json!({"command": "first"})),
            tool_requested("dup", "bash", json!({"command": "retry"})),
            tool_finished(
                "dup",
                json!({"exit_code": 0, "output": "retry result", "termination": "exited", "output_file": null, "truncated": false}),
            ),
            AgentFrameItem::ToolCallFinished(
                ToolCallResult::new(
                    ToolCallId("dup".into()),
                    OccurrenceId("first".into()),
                    json!({}),
                )
                .superseded_by_retry(&OccurrenceId("retry".into())),
            ),
        ];
        for (item, id) in items.iter_mut().zip(["first", "retry", "retry", "first"]) {
            let occurrence = OccurrenceId(id.into());
            match item {
                AgentFrameItem::ToolCallRequested(request) => request.occurrence_id = occurrence,
                AgentFrameItem::ToolCallFinished(result) => result.occurrence_id = occurrence,
                _ => unreachable!(),
            }
        }
        let views = build_tool_call_views(&items);
        assert!(views[0].superseded());
        assert!(
            matches!(tool_call_body(&items, &views[0]), Some(ToolCallBody::Raw { ref lines, .. }) if lines.iter().any(|line| line.contains("replaces it")))
        );
        assert_eq!(
            tool_call_body(&items, &views[1]),
            Some(ToolCallBody::Command {
                command: "retry".into(),
                exit_code: Some(0),
                lines: vec!["retry result".into()],
                omitted: 0,
            }),
        );
    }

    #[test]
    fn build_tool_call_body_reconstructs_an_fs_edit_diff() {
        let body = build_tool_call_body(
            "fs.edit",
            &json!({
                "edits": [{
                    "path": "src/agent/view.rs",
                    "old_string": "line1\nold\nline3",
                    "new_string": "line1\nnew a\nnew b\nline3",
                }],
            }),
            Some(&edit_result("src/agent/view.rs")),
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
    fn build_tool_call_body_concatenates_an_fs_edit_batchs_diffs_with_path_headers() {
        let body = build_tool_call_body(
            "fs.edit",
            &json!({
                "edits": [
                    {"path": "/w/a.rs", "old_string": "old", "new_string": "new"},
                    {"path": "/w/b.rs", "old_string": "gone", "new_string": "kept"},
                ],
            }),
            None,
        );
        let ToolCallBody::Diff { lines, omitted } = body else {
            panic!("expected a Diff body");
        };
        assert_eq!(omitted, 0);
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "--- /w/a.rs · proposed"),
                (DiffLineKind::Removed, "old"),
                (DiffLineKind::Added, "new"),
                (DiffLineKind::Context, "--- /w/b.rs · proposed"),
                (DiffLineKind::Removed, "gone"),
                (DiffLineKind::Added, "kept"),
            ]
        );
    }

    #[test]
    fn build_tool_call_body_carries_bash_command_exit_code_and_output() {
        let body = build_tool_call_body(
            "bash",
            &json!({"command": "cargo test"}),
            Some(
                &json!({"exit_code": 0, "output": "line1\nline2\n", "truncated": false, "termination": "exited", "output_file": null}),
            ),
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
            Some(
                &json!({"exit_code": 0, "output": output_text, "termination": "exited", "output_file": null, "truncated": false}),
            ),
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
            Some(
                &json!({"start_line": 1, "end_line": 40, "total_lines": 120, "path": "fixture", "content_version": null, "content": "", "content_chars": 0, "truncated": false, "notice": null, "next_offset": null}),
            ),
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
            Some(
                &json!({"returned_count": 3, "base_path": ".", "pattern": "fixture", "matches": [], "truncated": false, "total_matches": 3}),
            ),
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
            Some(
                &json!({"returned_count": 5, "base_path": ".", "pattern": "fixture", "matches": [], "truncated": false, "total_matches": 5}),
            ),
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
        let mut items = vec![
            tool_requested(
                "dup",
                "fs.edit",
                json!({"edits": [{"path": "a.rs", "old_string": "first old", "new_string": "first new"}]}),
            ),
            approval_requested("dup"),
            tool_started("dup"),
            tool_finished("dup", edit_result("a.rs")),
            // A second, distinct call reuses the same call_id after the
            // first one's cycle is fully closed.
            tool_requested(
                "dup",
                "fs.edit",
                json!({"edits": [{"path": "b.rs", "old_string": "second old", "new_string": "second new"}]}),
            ),
            approval_requested("dup"),
            // No `ToolCallStarted`/`ToolCallFinished` yet for this second
            // occurrence: it's the one currently pending approval.
        ];
        for (index, item) in items.iter_mut().enumerate() {
            let occurrence = horizon_agent::contract::OccurrenceId(
                if index < 4 { "first" } else { "second" }.into(),
            );
            match item {
                AgentFrameItem::ToolCallRequested(request) => request.occurrence_id = occurrence,
                AgentFrameItem::ToolCallStarted(identity) => identity.occurrence_id = occurrence,
                AgentFrameItem::ToolCallFinished(result) => result.occurrence_id = occurrence,
                AgentFrameItem::ApprovalRequested(approval) => approval.occurrence_id = occurrence,
                _ => unreachable!(),
            }
        }
        let views = build_tool_call_views(&items);
        assert_eq!(views.len(), 2);

        // The first occurrence keeps its own, correct resolution.
        assert_eq!(views[0].approval, ApprovalState::Approved);
        assert!(views[0].finished());
        assert_eq!(views[0].target.as_deref(), Some("a.rs"));

        // The second occurrence -- the actionable one -- must render as
        // `Waiting`, not `None`, so the UI shows Approve/Deny for it.
        assert_eq!(views[1].approval, ApprovalState::Waiting);
        assert!(!views[1].finished());
        assert_eq!(views[1].target.as_deref(), Some("b.rs"));

        // Its proposal body must reflect the *second* call's own content,
        // not the already-finished first one that happens to share the
        // id. The body uses the view's existing source binding.
        match tool_call_body(&items, &views[1]) {
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
                json!({"edits": [{"path": "b.rs", "old_string": "x", "new_string": "y"}]}),
            ),
            tool_finished(
                "a",
                json!({"total_lines": 10, "path": "fixture", "content_version": null, "content": "", "content_chars": 0, "truncated": false, "notice": null, "next_offset": null, "start_line": 1, "end_line": 10}),
            ),
            tool_finished("b", edit_result("b.rs")),
        ];
        let views = build_tool_call_views(&items);
        match tool_call_body(&items, &views[1]) {
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
    fn tool_call_body_rejects_missing_source_items() {
        let items = vec![tool_requested("a", "fs.read", json!({"path": "a.rs"}))];
        let views = build_tool_call_views(&items);
        assert!(tool_call_body(&[], &views[0]).is_none());
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
        let views = build_tool_call_views(&items);
        match tool_call_body(&items, &views[0]) {
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

    #[test]
    fn classification_recognizes_every_displayed_tool() {
        // Known generic tools need a Summary body even when their label changes.
        for known in [
            "fs.edit",
            "fs.write",
            "bash",
            "fs.read",
            "fs.grep",
            "fs.glob",
            "workspace.snapshot",
            "config.read",
            "config.write",
            "recall.search",
            "recall.read",
            "skill.read",
            "task",
            "task_output",
        ] {
            assert!(
                classify(known, &Value::Null, None).known,
                "{known:?} should be known"
            );
        }
        // An id `classify` falls back on stays unknown -- raw-JSON body.
        assert!(!classify("some.future.tool", &Value::Null, None).known);
    }

    #[test]
    fn build_tool_call_body_summarizes_a_task_output_call_not_raw_json() {
        // `task_output` has a dedicated arm in `classify` (verb "Task
        // Output"), so it must take the Summary path. Before the fix
        // `is_known_tool_id` omitted it and the call landed in `Raw`, so a
        // task-output fetch rendered as pretty-printed JSON.
        let body = build_tool_call_body(
            "task_output",
            &json!({"session_id": "3f2b"}),
            Some(&json!({
                "session_id": "3f2b",
                "description": "map the emit sites",
                "status": "finished",
            })),
        );
        match body {
            ToolCallBody::Summary(line) => assert!(
                line.contains("Task Output") && line.contains("finished"),
                "expected a Task Output summary, got {line:?}"
            ),
            other => panic!("expected a Summary body for task_output, got {other:?}"),
        }
    }
    #[test]
    fn partial_edits_render_only_applied_diffs_and_label_the_other_outcomes() {
        let input = json!({"edits": [
            {"path": "/w/a", "old_string": "old", "new_string": "applied"},
            {"path": "/w/b", "old_string": "missing", "new_string": "failed change"},
            {"path": "/w/c", "old_string": "keep", "new_string": "unattempted change"}
        ]});
        let output = json!({"applied_count": 1, "file_count": 1, "failed_index": 1,
        "message": "second edit failed", "is_error": true, "edits": [
            {"index": 0, "path": "/w/a", "status": "applied", "occurrences": 2},
            {"index": 1, "path": "/w/b", "status": "failed", "message": "not found"},
            {"index": 2, "path": "/w/c", "status": "not_attempted"}
        ]});
        let ToolCallBody::Diff { lines, .. } =
            build_tool_call_body("fs.edit", &input, Some(&output))
        else {
            panic!("expected edit receipts")
        };
        assert!(lines
            .iter()
            .any(|line| line.text.contains("applied (2 replacements)")));
        assert!(lines
            .iter()
            .any(|line| line.text.contains("failed: not found")));
        assert!(lines.iter().any(|line| line.text.contains("not attempted")));
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.kind == DiffLineKind::Added)
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["applied"]
        );
    }
}
