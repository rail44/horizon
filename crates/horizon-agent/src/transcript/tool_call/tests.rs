use serde_json::json;

use super::super::test_support::*;
use super::*;

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

/// `task`'s `description` input is what the requester's transcript
/// shows while a delegated task runs -- the session it spawns is
/// withheld from the client-visible session list, so this row is the
/// only place it announces itself. Since the launch became
/// asynchronous, the row's summary is the launch receipt's own status.
#[test]
fn a_task_call_is_labelled_with_its_description() {
    let items = vec![
        tool_requested(
            "t",
            "task",
            json!({"description": "map the emit sites", "prompt": "where are they?"}),
        ),
        tool_finished(
            "t",
            json!({"session_id": "3f2b", "description": "map the emit sites",
                       "status": "started"}),
        ),
    ];
    let views = build_tool_call_views(&items);
    assert_eq!(views[0].verb, "Task");
    assert_eq!(views[0].target.as_deref(), Some("map the emit sites"));
    assert_eq!(views[0].result_summary.as_deref(), Some("started"));
    assert!(!views[0].is_error);
}

/// The pull half of the same pair: `task_output` echoes the launch's
/// label back so its row reads like the launch row, and distinguishes a
/// task still running from one whose report is ready.
#[test]
fn a_task_output_call_reports_running_and_finished_distinctly() {
    let running = build_tool_call_views(&[
        tool_requested("o", "task_output", json!({"session_id": "3f2b"})),
        tool_finished(
            "o",
            json!({"session_id": "3f2b", "description": "map the emit sites",
                       "status": "running"}),
        ),
    ]);
    assert_eq!(running[0].verb, "Task Output");
    assert_eq!(running[0].target.as_deref(), Some("map the emit sites"));
    assert_eq!(running[0].result_summary.as_deref(), Some("running"));
    assert!(!running[0].is_error);

    let finished = build_tool_call_views(&[
        tool_requested("o", "task_output", json!({"session_id": "3f2b"})),
        tool_finished(
            "o",
            json!({"session_id": "3f2b", "description": "map the emit sites",
                       "status": "finished", "report": "session.rs:1747"}),
        ),
    ]);
    assert_eq!(finished[0].result_summary.as_deref(), Some("finished"));
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
fn running_row_expandable_for_any_finished_call_but_not_a_still_running_one() {
    let still_running =
        build_tool_call_views(&[tool_requested("a", "bash", json!({"command": "x"}))]);
    assert!(!running_row_expandable(&still_running[0]));

    let succeeded = build_tool_call_views(&[
        tool_requested("a", "bash", json!({"command": "x"})),
        tool_finished("a", json!({"exit_code": 0})),
    ]);
    assert!(running_row_expandable(&succeeded[0]));

    let failed = build_tool_call_views(&[
        tool_requested("a", "bash", json!({"command": "x"})),
        tool_finished("a", json!({"is_error": true, "message": "boom"})),
    ]);
    assert!(running_row_expandable(&failed[0]));
}

#[test]
fn a_call_with_no_approval_request_has_approval_state_none() {
    let items = vec![
        tool_requested("a", "fs.read", json!({"path": "a.rs"})),
        tool_finished("a", json!({"total_lines": 1})),
    ];
    let views = build_tool_call_views(&items);
    assert_eq!(views[0].approval, ApprovalState::None);
}

#[test]
fn a_call_with_an_unresolved_approval_request_is_waiting() {
    let items = vec![
        tool_requested("a", "bash", json!({"command": "cargo test"})),
        approval_requested("a"),
        // no tool_finished yet: still pending.
    ];
    let views = build_tool_call_views(&items);
    assert_eq!(views[0].approval, ApprovalState::Waiting);
}

#[test]
fn a_call_whose_tool_call_started_folded_is_approved_even_while_still_running() {
    // Root-caused 2026-07-13: `bash`'s approve ack folds
    // `ToolCallStarted` synchronously, one IPC hop after the click,
    // with the eventual `ToolCallFinished` arriving later and
    // asynchronously. The row must read `Approved` (buttons/proposal
    // body gone, muted "approved" phrase shown) the moment the ack
    // folds -- not stay `Waiting` for the whole tool run.
    let items = vec![
        tool_requested("a", "bash", json!({"command": "cargo test"})),
        approval_requested("a"),
        tool_started("a"),
        // no tool_finished yet: the command is still running.
    ];
    let views = build_tool_call_views(&items);
    assert_eq!(views[0].approval, ApprovalState::Approved);
    assert!(!views[0].finished);
}

#[test]
fn a_call_resolved_with_the_denied_marker_is_denied() {
    // The current production path: `ToolCallResult::denied` sets the
    // contract-explicit marker, read directly with no message-text
    // sniffing at all.
    let items = vec![
        tool_requested("a", "bash", json!({"command": "rm -rf /tmp/x"})),
        approval_requested("a"),
        AgentFrameItem::ToolCallFinished(ToolCallResult::denied(
            ToolCallId("a".to_string()),
            None,
            json!({"is_error": true, "message": "denied by user"}),
        )),
    ];
    let views = build_tool_call_views(&items);
    assert_eq!(views[0].approval, ApprovalState::Denied);
}

#[test]
fn a_call_resolved_with_the_denied_by_user_convention_is_denied() {
    // The fallback path: `tool_finished` builds its `ToolCallResult`
    // via `ToolCallResult::new`, which never sets `denied` -- exactly
    // what a pre-marker persisted JSONL log deserializes as
    // (`#[serde(default)]`). Classification must still land on
    // `Denied` by recognizing the old message-text convention.
    let items = vec![
        tool_requested("a", "bash", json!({"command": "rm -rf /tmp/x"})),
        approval_requested("a"),
        tool_finished("a", json!({"is_error": true, "message": "denied by user"})),
    ];
    let views = build_tool_call_views(&items);
    assert_eq!(views[0].approval, ApprovalState::Denied);
}

#[test]
fn a_call_resolved_successfully_after_approval_is_approved() {
    let items = vec![
        tool_requested("a", "bash", json!({"command": "cargo build"})),
        approval_requested("a"),
        tool_finished("a", json!({"exit_code": 0, "output": ""})),
    ];
    let views = build_tool_call_views(&items);
    assert_eq!(views[0].approval, ApprovalState::Approved);
}

#[test]
fn an_approved_call_that_then_fails_on_its_own_is_still_approved_not_denied() {
    // Distinguishes a genuine denial from an *approved* call that
    // later fails for its own reasons (e.g. fs.edit's old_string not
    // found) -- both are `is_error: true`, but only the denial
    // carries the exact "denied by user" message.
    let items = vec![
        tool_requested(
            "a",
            "fs.edit",
            json!({"edits": [{"path": "a.rs", "old_string": "x", "new_string": "y"}]}),
        ),
        approval_requested("a"),
        tool_finished(
            "a",
            json!({"is_error": true, "message": "`old_string` not found in `a.rs`"}),
        ),
    ];
    let views = build_tool_call_views(&items);
    assert_eq!(views[0].approval, ApprovalState::Approved);
}

#[test]
fn fs_edit_derives_a_diffstat_from_old_and_new_string() {
    let items = vec![
        tool_requested(
            "a",
            "fs.edit",
            json!({
                "edits": [{
                    "path": "src/agent/view.rs",
                    "old_string": "line1\nold\nline3",
                    "new_string": "line1\nnew a\nnew b\nline3",
                }],
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
fn an_fs_edit_batch_reads_as_its_own_cardinality_and_keeps_every_affected_file() {
    let items = vec![
        tool_requested(
            "a",
            "fs.edit",
            json!({
                "edits": [
                    {"path": "/w/a.rs", "old_string": "old", "new_string": "new"},
                    {"path": "/w/b.rs", "old_string": "x", "new_string": "y\nz"},
                    {"path": "/w/a.rs", "old_string": "p", "new_string": "q"},
                ],
            }),
        ),
        tool_finished(
            "a",
            json!({
                "applied_count": 3,
                "file_count": 2,
                "edits": [
                    {"index": 0, "path": "/w/a.rs", "status": "applied", "occurrences": 1},
                    {"index": 1, "path": "/w/b.rs", "status": "applied", "occurrences": 1},
                    {"index": 2, "path": "/w/a.rs", "status": "applied", "occurrences": 1},
                ],
            }),
        ),
    ];
    let views = build_tool_call_views(&items);
    assert_eq!(views[0].verb, "Edit");
    assert_eq!(views[0].target.as_deref(), Some("3 edits in 2 files"));
    // Summed across the batch: three replacements, one of which adds a
    // line.
    assert_eq!(views[0].result_summary.as_deref(), Some("+4 -3"));
    // No single file represents a multi-file batch, so it gets no file
    // chip.
    assert_eq!(views[0].kind, ToolCallKind::Generic);
    assert_eq!(
        views[0]
            .affected_files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec!["/w/a.rs", "/w/b.rs", "/w/a.rs"],
    );
}

#[test]
fn an_fs_edit_batch_within_one_file_keeps_its_file_chip() {
    let items = vec![tool_requested(
        "a",
        "fs.edit",
        json!({
            "edits": [
                {"path": "/w/a.rs", "old_string": "old", "new_string": "new"},
                {"path": "/w/a.rs", "old_string": "p", "new_string": "q\nr"},
            ],
        }),
    )];
    let views = build_tool_call_views(&items);
    assert_eq!(views[0].target.as_deref(), Some("2 edits in 1 file"));
    match &views[0].kind {
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

#[test]
fn line_diffstat_matches_the_reconstructed_diffs_own_counts() {
    assert_eq!(line_diffstat("a\nold1\nold2\nb", "a\nnew1\nb"), (1, 2));
    assert_eq!(line_diffstat("a\nb\nc", "a\nb\nc"), (0, 0));
}

/// Provider-reuse shape -- the `functions.fs.edit:66` incident in
/// session 05254b6a, generalized: a single provider `call_id` is
/// legitimately used by two completely distinct tool calls. Without
/// per-occurrence identity, both requests collapse onto the same
/// `Building` slot and the first result attributes to the second
/// request (or vice versa), leaving the genuine occurrence stuck
/// "started-but-never-finished" in the transcript. See
/// `backlog 42 / 55`.
#[test]
fn provider_reused_call_id_attributes_each_occurrence_to_its_own_result() {
    // Both requests land before either result -- the shape the
    // `.rev()` fallback actually gets wrong. A batched turn requests
    // two calls at once, and a provider that reuses ids hands both
    // the same `call_id`; with positional matching alone, `fin(occ_a)`
    // attributes to the *newest* entry with that call_id, which is
    // occurrence B. Each request carries its own fresh `OccurrenceId`,
    // exactly what `rig_tool_call_request` mints at the provider
    // boundary.
    let occ_a = OccurrenceId("occ-A".to_string());
    let occ_b = OccurrenceId("occ-B".to_string());
    let items = vec![
        tool_requested_with_occurrence(
            "fs.edit:1",
            "fs.edit",
            json!({"edits": [{"path": "a.txt", "old_string": "x", "new_string": "y"}]}),
            occ_a.clone(),
        ),
        tool_requested_with_occurrence(
            "fs.edit:1",
            "fs.edit",
            json!({"edits": [{"path": "b.txt", "old_string": "p", "new_string": "q"}]}),
            occ_b.clone(),
        ),
        // A's result arrives first and must land on A's row, not on
        // the more recent B.
        tool_finished_with_occurrence(
            "fs.edit:1",
            json!({ "is_error": false, "applied": true }),
            occ_a.clone(),
        ),
        tool_finished_with_occurrence(
            "fs.edit:1",
            json!({ "is_error": true, "message": "old_string not found" }),
            occ_b.clone(),
        ),
    ];
    let views = build_tool_call_views(&items);
    // Two rows, one per occurrence -- exactly what the user wants
    // for the provider-reuse shape.
    assert_eq!(views.len(), 2);
    // Each row's `call_id` is the same (the provider gave us the same
    // string twice); the second key decides which `Building` entry
    // each result attached to. Attribution is observable two ways:
    // `affected_files` carries the path of the *request* the row was
    // built from, and `is_error` carries the outcome of the *result*
    // that attached to it -- so a misattribution pairs a.txt with B's
    // failure (and vice versa) rather than going unnoticed.
    assert_eq!(views[0].call_id, ToolCallId("fs.edit:1".to_string()));
    assert!(views[0].finished);
    assert_eq!(
        views[0]
            .affected_files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        vec!["a.txt"],
    );
    assert!(
        !views[0].is_error,
        "a.txt's row must carry occurrence A's successful result, not B's failure"
    );
    assert_eq!(views[1].call_id, ToolCallId("fs.edit:1".to_string()));
    assert!(views[1].finished);
    assert_eq!(
        views[1]
            .affected_files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        vec!["b.txt"],
    );
    assert!(
        views[1].is_error,
        "b.txt's row must carry occurrence B's failing result"
    );
}

/// Domain-denial-retry shape, in the sequence the daemon actually
/// emits (`crates/horizon-agentd/src/session/completion.rs`'s
/// `fold_domain_denied` plus `tools::approval::
/// resolve_domain_denial_retry`): a tier-1 auto-approved bash call
/// starts, is refused a domain, and is *reissued* under the same
/// `call_id` with a fresh occurrence and its own approval prompt.
/// `fold_domain_denied` emits no result for the first attempt -- it
/// parks the outcome on the approval's `prior_result` -- so the only
/// `ToolCallFinished` here arrives after the reissue. Denying it
/// forwards that parked result, stamped with the *first* attempt's
/// occurrence, which is precisely where positional `.rev()` matching
/// misfires: the newest request with this `call_id` is the reissue.
/// See `backlog 42 / 55`.
#[test]
fn a_denial_retrys_parked_result_attaches_to_the_attempt_that_produced_it() {
    let occ_1 = OccurrenceId("occ-1".to_string());
    let occ_2 = OccurrenceId("occ-2".to_string());
    let items = vec![
        // Initial attempt: tier-1 auto-approved, so it starts with no
        // approval prompt of its own, and gets no result event.
        tool_requested_with_occurrence(
            "bash:1",
            "bash",
            json!({"command": "curl -sS http://evil.example.com/x"}),
            occ_1.clone(),
        ),
        tool_started("bash:1"),
        // `fold_domain_denied`'s reissue: same `call_id`, fresh
        // occurrence, and the retry prompt.
        tool_requested_with_occurrence(
            "bash:1",
            "bash",
            json!({"command": "curl -sS http://evil.example.com/x"}),
            occ_2.clone(),
        ),
        approval_requested_with_occurrence("bash:1", occ_2.clone()),
        // The user declined the domain grant, so the parked first
        // attempt's outcome is what reaches the provider -- carrying
        // `occ_1`, the occurrence that actually ran it.
        tool_finished_with_occurrence(
            "bash:1",
            json!({
                "is_error": true,
                "denied_domains": ["evil.example.com"],
                "exit_code": 0,
            }),
            occ_1.clone(),
        ),
    ];
    let views = build_tool_call_views(&items);
    // Both attempts are visible -- two rows, same conceptual
    // `call_id` (the agentd reissues the id, only the occurrence is
    // fresh).
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].call_id, ToolCallId("bash:1".to_string()));
    assert_eq!(views[1].call_id, ToolCallId("bash:1".to_string()));
    // The result belongs to the attempt that ran, not to the reissue
    // that was declined.
    assert!(
        views[0].finished && views[0].is_error,
        "the parked outcome must land on the first attempt's row"
    );
    assert!(
        !views[1].finished,
        "the declined reissue produced no result of its own"
    );
    // The reissue is the row that carries the approval, and it
    // resolved as a denial (its prompt was answered, and no
    // `ToolCallStarted` followed).
    assert_eq!(views[0].approval, ApprovalState::None);
    assert_eq!(views[1].approval, ApprovalState::Waiting);
    // The deny path was already the one that closed the first row; the
    // approve path's counterpart is
    // `an_approved_denial_retry_closes_the_abandoned_attempt_as_superseded`
    // below (backlog 55).
}

/// The same denial-retry shape, taken down the *approve* branch --
/// backlog 55's fix (owner decision 2026-07-28). The parked outcome is
/// discarded there (the retry recomputes it), so
/// `tools::approval::superseded_by_retry_result` closes the abandoned
/// occurrence with a terminal marker instead, and the retry's own
/// result closes the reissue. Both rows must read closed, and the
/// abandoned one must read as *superseded* rather than as a success or
/// a failure.
#[test]
fn an_approved_denial_retry_closes_the_abandoned_attempt_as_superseded() {
    let occ_1 = OccurrenceId("occ-1".to_string());
    let occ_2 = OccurrenceId("occ-2".to_string());
    let command = json!({"command": "cargo build --workspace"});
    let items = vec![
        tool_requested_with_occurrence("bash:1", "bash", command.clone(), occ_1.clone()),
        tool_started("bash:1"),
        // `fold_filesystem_denied`'s reissue plus its retry prompt.
        tool_requested_with_occurrence("bash:1", "bash", command, occ_2.clone()),
        approval_requested_with_occurrence("bash:1", occ_2.clone()),
        // Approve: the abandoned attempt is closed, the retry starts.
        tool_finished_with_occurrence(
            "bash:1",
            json!({
                SUPERSEDED_BY_RETRY: true,
                "retry_occurrence_id": occ_2.0,
                "message": "this attempt was abandoned; an approved retry of the same call \
                            replaced it",
            }),
            occ_1.clone(),
        ),
        tool_started("bash:1"),
        // ... and finishes on its own.
        tool_finished_with_occurrence("bash:1", json!({ "exit_code": 0 }), occ_2.clone()),
    ];

    let views = build_tool_call_views(&items);
    assert_eq!(views.len(), 2);

    assert!(
        views[0].finished,
        "the abandoned attempt must no longer render started-but-never-finished"
    );
    assert!(views[0].superseded);
    assert!(
        !views[0].is_error,
        "an attempt a retry replaced did not fail on its own terms"
    );
    assert_eq!(views[0].result_summary.as_deref(), Some(SUPERSEDED_SUMMARY));
    assert_eq!(views[0].approval, ApprovalState::None);

    assert!(views[1].finished);
    assert!(!views[1].superseded);
    assert!(!views[1].is_error);
    assert_eq!(views[1].result_summary.as_deref(), Some("exit 0"));
    assert_eq!(views[1].approval, ApprovalState::Approved);
}

/// The retry's own result can land before the abandoned attempt's
/// close in a replayed log; occurrence-first matching must keep each
/// result on the row that produced it either way.
#[test]
fn a_superseded_close_arriving_after_the_retrys_result_still_lands_on_its_own_row() {
    let occ_1 = OccurrenceId("occ-1".to_string());
    let occ_2 = OccurrenceId("occ-2".to_string());
    let command = json!({"command": "cargo build --workspace"});
    let items = vec![
        tool_requested_with_occurrence("bash:1", "bash", command.clone(), occ_1.clone()),
        tool_requested_with_occurrence("bash:1", "bash", command, occ_2.clone()),
        tool_finished_with_occurrence("bash:1", json!({ "exit_code": 0 }), occ_2),
        tool_finished_with_occurrence("bash:1", json!({ SUPERSEDED_BY_RETRY: true }), occ_1),
    ];

    let views = build_tool_call_views(&items);
    assert_eq!(views.len(), 2);
    assert!(views[0].superseded && views[0].finished);
    assert!(!views[1].superseded && views[1].finished);
    assert_eq!(views[1].result_summary.as_deref(), Some("exit 0"));
}

/// A superseded attempt is a genuine attempt, not a failure and not an
/// anomaly, so the collapsed receipt line must neither break it out as
/// an individual chip nor count it alongside the retry that carries the
/// real outcome.
#[test]
fn the_collapsed_receipt_counts_a_superseded_attempt_once_not_twice() {
    let occ_1 = OccurrenceId("occ-1".to_string());
    let occ_2 = OccurrenceId("occ-2".to_string());
    let command = json!({"command": "cargo build --workspace"});
    let views = build_tool_call_views(&[
        tool_requested_with_occurrence("bash:1", "bash", command.clone(), occ_1.clone()),
        tool_requested_with_occurrence("bash:1", "bash", command, occ_2.clone()),
        tool_finished_with_occurrence("bash:1", json!({ SUPERSEDED_BY_RETRY: true }), occ_1),
        tool_finished_with_occurrence("bash:1", json!({ "exit_code": 0 }), occ_2),
    ]);

    let aggregate = super::super::aggregate_receipt(&views);
    assert_eq!(aggregate.bash_count, 1);
    assert!(aggregate.individual_calls.is_empty());
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
fn cap_thinking_text_keeps_everything_when_it_already_fits() {
    let (kept, omitted) = cap_thinking_text("one\ntwo\nthree", 6);
    assert_eq!(kept, "one\ntwo\nthree");
    assert_eq!(omitted, 0);
}

#[test]
fn cap_thinking_text_keeps_only_the_trailing_lines_once_it_overflows() {
    let text = "one\ntwo\nthree\nfour\nfive";
    let (kept, omitted) = cap_thinking_text(text, 2);
    // The newest lines survive -- the earlier ones are the ones
    // dropped, matching "newest content visible" (owner requirement).
    assert_eq!(kept, "four\nfive");
    assert_eq!(omitted, 3);
}

#[test]
fn cap_thinking_text_bounds_a_streaming_block_growing_delta_by_delta() {
    // The reducer coalesces every `ReasoningDelta` into one item's
    // growing `.text` (`frame.rs`'s `Event::ReasoningDelta` fold) --
    // this pins that re-running the cap on each successive render
    // never lets the *rendered* line count grow past the cap, even
    // though the underlying accumulated text keeps growing.
    let mut accumulated = String::new();
    let mut last_kept_lines = 0;
    for line in 0..20 {
        if !accumulated.is_empty() {
            accumulated.push('\n');
        }
        accumulated.push_str(&format!("thought {line}"));
        let (kept, _omitted) = cap_thinking_text(&accumulated, THINKING_TAIL_LINES);
        last_kept_lines = kept.lines().count();
        assert!(last_kept_lines <= THINKING_TAIL_LINES);
    }
    assert_eq!(last_kept_lines, THINKING_TAIL_LINES);
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
