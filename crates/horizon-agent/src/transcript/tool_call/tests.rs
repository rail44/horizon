use serde_json::json;

use super::super::test_support::*;
use super::files::affected_files;
use super::*;

#[test]
fn a_task_call_is_labelled_with_its_description() {
    let call = classify(
        "task",
        &json!({"description": "map the emit sites", "prompt": "where are they?"}),
        Some(
            &json!({"session_id": "3f2b", "description": "map the emit sites",
                     "status": "started"}),
        ),
    );
    assert_eq!(call.verb, "Task");
    assert_eq!(call.target.as_deref(), Some("map the emit sites"));
    assert_eq!(call.summary.as_deref(), Some("started"));
}

/// The pull half of the same pair: `task_output` echoes the launch's
/// label back so its row reads like the launch row, and distinguishes a
/// task still running from one whose report is ready.
#[test]
fn a_task_output_call_reports_running_and_finished_distinctly() {
    let input = json!({"session_id": "3f2b"});
    let running = classify(
        "task_output",
        &input,
        Some(
            &json!({"session_id": "3f2b", "description": "map the emit sites",
                     "status": "running", "message": "not finished"}),
        ),
    );
    assert_eq!(running.verb, "Task Output");
    assert_eq!(running.target.as_deref(), Some("map the emit sites"));
    assert_eq!(running.summary.as_deref(), Some("running"));

    let finished = classify(
        "task_output",
        &input,
        Some(
            &json!({"session_id": "3f2b", "description": "map the emit sites",
                     "status": "finished", "report": "session.rs:1747"}),
        ),
    );
    assert_eq!(finished.summary.as_deref(), Some("finished"));
}

#[test]
fn a_failed_bash_call_summarizes_its_exit_code() {
    let call = classify(
        "bash",
        &json!({"command": "cargo test"}),
        Some(
            &json!({"is_error": true, "message": "boom", "exit_code": 1, "termination": "exited", "output_file": null, "output": "", "truncated": false}),
        ),
    );
    assert_eq!(call.summary.as_deref(), Some("exit 1"));
}

#[test]
fn a_still_running_call_has_no_summary() {
    let call = classify("bash", &json!({"command": "cargo test"}), None);
    assert!(call.summary.is_none());
}

#[test]
fn fs_edit_derives_a_diffstat_from_old_and_new_string() {
    let call = classify(
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
    assert_eq!(call.verb, "Edit");
    assert_eq!(call.target.as_deref(), Some("src/agent/view.rs"));
    assert_eq!(call.summary.as_deref(), Some("+2 -1"));
    match &call.kind {
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
    let call = classify(
        "fs.write",
        &json!({"path": "new.rs", "content": "fn main() {}"}),
        Some(&json!({"path": "new.rs", "bytes_written": 12, "created": true})),
    );
    assert_eq!(call.verb, "Write");
    assert_eq!(call.summary.as_deref(), Some("created"));
    match &call.kind {
        ToolCallKind::File { diffstat, .. } => assert_eq!(*diffstat, None),
        other => panic!("expected a File chip, got {other:?}"),
    }
}

#[test]
fn an_fs_edit_batch_reads_as_its_own_cardinality_and_keeps_every_affected_file() {
    let input = json!({
        "edits": [
            {"path": "/w/a.rs", "old_string": "old", "new_string": "new"},
            {"path": "/w/b.rs", "old_string": "x", "new_string": "y\nz"},
            {"path": "/w/a.rs", "old_string": "p", "new_string": "q"},
        ],
    });
    let output = json!({
        "applied_count": 3,
        "file_count": 2,
        "edits": [
            {"index": 0, "path": "/w/a.rs", "status": "applied", "occurrences": 1},
            {"index": 1, "path": "/w/b.rs", "status": "applied", "occurrences": 1},
            {"index": 2, "path": "/w/a.rs", "status": "applied", "occurrences": 1},
        ],
    });
    let call = classify("fs.edit", &input, Some(&output));
    assert_eq!(call.verb, "Edit");
    assert_eq!(call.target.as_deref(), Some("3 edits in 2 files"));
    // Summed across the batch: three replacements, one of which adds a
    // line.
    assert_eq!(call.summary.as_deref(), Some("+4 -3"));
    // No single file represents a multi-file batch, so it gets no file
    // chip.
    assert_eq!(call.kind, ToolCallKind::Generic);
    assert_eq!(
        affected_files("fs.edit", &input, Some(&output))
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec!["/w/a.rs", "/w/b.rs", "/w/a.rs"],
    );
}

#[test]
fn an_fs_edit_batch_within_one_file_keeps_its_file_chip() {
    let call = classify(
        "fs.edit",
        &json!({
            "edits": [
                {"path": "/w/a.rs", "old_string": "old", "new_string": "new"},
                {"path": "/w/a.rs", "old_string": "p", "new_string": "q\nr"},
            ],
        }),
        None,
    );
    assert_eq!(call.target.as_deref(), Some("2 edits in 1 file"));
    match &call.kind {
        ToolCallKind::File {
            file_name,
            diffstat,
        } => {
            assert_eq!(file_name, "a.rs");
            assert_eq!(*diffstat, Some((3, 2)));
        }
        other => panic!("expected a File chip, got {other:?}"),
    }
}

#[test]
fn bash_chip_carries_a_truncated_command_head() {
    let long_command = "cargo test --workspace --all-targets -- --nocapture and-then-some-more";
    let call = classify("bash", &json!({"command": long_command}), None);
    match &call.kind {
        ToolCallKind::Bash { command_head } => {
            assert!(command_head.ends_with('…'));
            assert!(command_head.chars().count() <= 32);
        }
        other => panic!("expected a Bash chip, got {other:?}"),
    }
}

#[test]
fn line_diffstat_matches_the_reconstructed_diffs_own_counts() {
    assert_eq!(line_diffstat("a\nold1\nold2\nb", "a\nnew1\nb"), (1, 2));
    assert_eq!(line_diffstat("a\nb\nc", "a\nb\nc"), (0, 0));
}

#[test]
fn cap_lines_head_trims_the_tail_and_reports_the_omitted_count() {
    let (kept, omitted) = cap_lines_head(vec![1, 2, 3, 4, 5], 3);
    assert_eq!(kept, vec![1, 2, 3]);
    assert_eq!(omitted, 2);

    let (kept, omitted) = cap_lines_head(vec![1, 2], 3);
    assert_eq!(kept, vec![1, 2]);
    assert_eq!(omitted, 0);
}

#[test]
fn cap_lines_tail_trims_the_head_and_reports_the_omitted_count() {
    let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let (kept, omitted) = cap_lines_tail(lines, 2);
    assert_eq!(kept, vec!["b".to_string(), "c".to_string()]);
    assert_eq!(omitted, 1);
}

#[test]
fn truncate_chars_cuts_at_a_multibyte_boundary() {
    // "aé日" is 3 chars; cutting at 2 must land between é and 日,
    // not mid-code-point.
    assert_eq!(truncate_chars("aé日", 2), ("aé".to_string(), true));
}

#[test]
fn truncate_chars_returns_original_at_exactly_cap() {
    // 3 chars, cap 3: no truncation, original returned.
    assert_eq!(truncate_chars("aé日", 3), ("aé日".to_string(), false));
}

#[test]
fn truncate_chars_returns_original_below_cap() {
    assert_eq!(truncate_chars("abc", 10), ("abc".to_string(), false));
}

#[test]
fn truncate_chars_handles_empty_string() {
    assert_eq!(truncate_chars("", 10), ("".to_string(), false));
}

#[test]
fn truncate_chars_truncates_long_ascii() {
    let long = "z".repeat(400);
    let (head, truncated) = truncate_chars(&long, 120);
    assert!(truncated);
    assert_eq!(head.chars().count(), 120);
    assert_eq!(head, "z".repeat(120));
}
