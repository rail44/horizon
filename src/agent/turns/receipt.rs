//! A receipt's one-line summary and the elapsed-time wording. The
//! aggregation itself (`ReceiptAggregate`/`aggregate_receipt`) lives in
//! `crate::agent::model`; this file holds only the wording built on it.

use std::time::Duration;

use horizon_acp::ToolOutcome;

use super::{aggregate_receipt, pluralize, ToolCallView};

/// Humanizes a duration: `38s`, `2m 05s`. Whole seconds only.
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

/// A burst's summary: `Read {n} files · Edited {n} files · Ran {n}
/// commands · {n} searches`, only the non-zero parts. The aggregate counts
/// the calls that succeeded; a call still running or failed is counted by
/// its tool as well. Calls that never ran (denied, cancelled) count only
/// in the `{n} calls` fallback for a burst with nothing else to say.
pub(crate) fn receipt_summary(calls: &[ToolCallView]) -> String {
    let aggregate = aggregate_receipt(calls);
    let mut reads = aggregate.read_file_count;
    let mut edits = aggregate.edited_file_count;
    let mut runs = aggregate.bash_count;
    let mut searches = aggregate.query_count;
    for call in &aggregate.individual_calls {
        if !matches!(call.outcome, None | Some(ToolOutcome::Failed)) {
            continue;
        }
        match call.tool_id.as_str() {
            "fs.read" => reads += 1,
            "fs.edit" | "fs.write" => edits += call.affected_files.len().max(1),
            "bash" => runs += 1,
            _ => searches += 1,
        }
    }
    let mut parts = Vec::new();
    if reads > 0 {
        parts.push(format!("Read {}", pluralize(reads, "file", "files")));
    }
    if edits > 0 {
        parts.push(format!("Edited {}", pluralize(edits, "file", "files")));
    }
    if runs > 0 {
        parts.push(format!("Ran {}", pluralize(runs, "command", "commands")));
    }
    if searches > 0 {
        parts.push(pluralize(searches, "search", "searches"));
    }
    if parts.is_empty() {
        return pluralize(calls.len(), "call", "calls");
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::test_support::*;
    use super::super::{build_tool_call_views, segment_bursts};
    use super::*;

    #[test]
    fn humanize_duration_matches_the_docs_examples() {
        assert_eq!(humanize_duration(Duration::from_secs(0)), "0s");
        assert_eq!(humanize_duration(Duration::from_secs(38)), "38s");
        assert_eq!(humanize_duration(Duration::from_secs(59)), "59s");
        assert_eq!(humanize_duration(Duration::from_secs(60)), "1m 00s");
        assert_eq!(humanize_duration(Duration::from_secs(125)), "2m 05s");
    }

    #[test]
    fn aggregate_receipt_folds_mixed_classes_into_summary_counts() {
        let items = vec![
            tool_finished(
                "q1",
                "fs.grep",
                json!({"base_path": ".", "pattern": "x"}),
                json!({"returned_count": 1, "base_path": ".", "pattern": "fixture", "matches": [], "truncated": false, "total_matches": 1}),
            ),
            tool_finished(
                "q2",
                "fs.glob",
                json!({"base_path": ".", "pattern": "*.rs"}),
                json!({"returned_count": 2, "base_path": ".", "pattern": "fixture", "matches": [], "truncated": false, "total_matches": 2}),
            ),
            tool_finished(
                "r1",
                "fs.read",
                json!({"path": "a.rs"}),
                json!({"total_lines": 10, "path": "fixture", "content_version": null, "content": "", "content_chars": 0, "truncated": false, "notice": null, "next_offset": null, "start_line": 1, "end_line": 10}),
            ),
            tool_finished(
                "e1",
                "fs.edit",
                json!({"edits": [{"path": "b.rs", "old_string": "x", "new_string": "y"}]}),
                edit_result("b.rs"),
            ),
            tool_finished(
                "b1",
                "bash",
                json!({"command": "cargo test"}),
                json!({"exit_code": 0, "output": "", "termination": "exited", "output_file": null, "truncated": false}),
            ),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        assert_eq!(aggregate.query_count, 2); // fs.grep + fs.glob
        assert_eq!(aggregate.read_file_count, 1);
        assert_eq!(aggregate.edited_file_count, 1);
        assert_eq!(aggregate.bash_count, 1);
        assert!(aggregate.individual_calls.is_empty());
        assert_eq!(
            receipt_summary(&tool_calls),
            "Read 1 file · Edited 1 file · Ran 1 command · 2 searches"
        );
    }

    #[test]
    fn aggregate_receipt_counts_distinct_paths_not_call_counts() {
        let items = vec![
            tool_finished(
                "r1",
                "fs.read",
                json!({"path": "a.rs"}),
                json!({"total_lines": 10, "path": "fixture", "content_version": null, "content": "", "content_chars": 0, "truncated": false, "notice": null, "next_offset": null, "start_line": 1, "end_line": 10}),
            ),
            tool_finished(
                "r2",
                "fs.read",
                json!({"path": "a.rs"}),
                json!({"total_lines": 10, "path": "fixture", "content_version": null, "content": "", "content_chars": 0, "truncated": false, "notice": null, "next_offset": null, "start_line": 1, "end_line": 10}),
            ),
            tool_finished(
                "r3",
                "fs.read",
                json!({"path": "b.rs"}),
                json!({"total_lines": 5, "path": "fixture", "content_version": null, "content": "", "content_chars": 0, "truncated": false, "notice": null, "next_offset": null, "start_line": 1, "end_line": 5}),
            ),
            tool_finished(
                "e1",
                "fs.edit",
                json!({"edits": [{"path": "c.rs", "old_string": "x", "new_string": "y"}]}),
                edit_result("c.rs"),
            ),
            tool_finished(
                "e2",
                "fs.write",
                json!({"path": "c.rs", "content": "z"}),
                json!({"path": "c.rs", "created": false, "bytes_written": 1}),
            ),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        // Two reads of a.rs collapse to one distinct path; b.rs adds a
        // second. An edit and a write to the same c.rs collapse to one
        // distinct edited path.
        assert_eq!(aggregate.read_file_count, 2);
        assert_eq!(aggregate.edited_file_count, 1);
        assert_eq!(receipt_summary(&tool_calls), "Read 2 files · Edited 1 file");
    }

    #[test]
    fn running_and_failed_calls_count_by_their_tool() {
        let items = vec![
            tool_requested("b1", "bash", json!({"command": "cargo build"})),
            tool_finished(
                "b2",
                "bash",
                json!({"command": "cargo test"}),
                json!({"is_error": true}),
            ),
        ];
        let tool_calls = build_tool_call_views(&items);
        assert_eq!(receipt_summary(&tool_calls), "Ran 2 commands");
    }

    #[test]
    fn aggregate_receipt_folds_bash_into_the_ran_commands_count() {
        let items = vec![
            tool_finished(
                "b1",
                "bash",
                json!({"command": "cargo build"}),
                json!({"exit_code": 0, "output": "", "termination": "exited", "output_file": null, "truncated": false}),
            ),
            tool_finished(
                "b2",
                "bash",
                json!({"command": "cargo test"}),
                json!({"exit_code": 0, "output": "", "termination": "exited", "output_file": null, "truncated": false}),
            ),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        assert_eq!(aggregate.bash_count, 2);
        assert!(aggregate.individual_calls.is_empty());
        assert_eq!(receipt_summary(&tool_calls), "Ran 2 commands");
    }

    #[test]
    fn a_burst_summarizes_only_its_own_range() {
        let items = vec![
            user_message("fix the bug"),
            tool_finished(
                "a",
                "fs.grep",
                json!({"base_path": ".", "pattern": "x"}),
                json!({"returned_count": 2, "base_path": ".", "pattern": "fixture", "matches": [], "truncated": false, "total_matches": 2}),
            ),
            tool_finished(
                "b",
                "fs.read",
                json!({"path": "a.rs"}),
                json!({"total_lines": 10, "path": "fixture", "content_version": null, "content": "", "content_chars": 0, "truncated": false, "notice": null, "next_offset": null, "start_line": 1, "end_line": 10}),
            ),
            assistant_delta("Looking at the code, I"),
        ];
        let bursts = segment_bursts(&items);
        assert_eq!(bursts.len(), 1);
        let burst = &bursts[0];
        assert!(burst.closed);
        let tool_calls = build_tool_call_views(&items[burst.start..burst.end]);
        assert_eq!(receipt_summary(&tool_calls), "Read 1 file · 1 search");
    }
}
