//! Receipt status/duration text and the collapsed-receipt aggregation
//! family (owner feedback 2026-07-13: query/edit/bash calls fold into
//! prose counts rather than a row of low-signal chips).

use std::collections::HashSet;
use std::time::Duration;

use horizon_agent::config::{DEFAULT_DOOM_LOOP_WINDOW, DEFAULT_ITERATION_CAP};
use horizon_agent::contract::TurnEndReason;

use super::grouping::TurnEnd;
use super::tool_call::ToolCallView;
use super::{classify_call, pluralize};

/// The receipt row's trailing content (`render_receipt`'s `tail`
/// parameter). Round 5 (monotone burst splitting): only the turn's
/// *final* burst -- the one closed by `TurnEnded` -- carries the turn's
/// end-reason status, total elapsed, and model, exactly as a completed
/// turn's one receipt always has; every other burst's receipt
/// (including the last one while the turn is still running) carries
/// none of that -- the contract has no per-burst timing, only a
/// whole-turn one. `Final` carries a `&TurnEnd` rather than duplicating
/// its fields so it can never drift from [`receipt_status`]'s own
/// reading of it.
pub(crate) enum ReceiptTail<'a> {
    Final(&'a TurnEnd),
    Intermediate,
}

/// A turn's end-reason rendered as receipt status text -- the
/// `Cancelled` -> `stopped · {elapsed}` / `Failed` -> error-marked variant
/// from decision 1's end-reason handling. A guard halt
/// (`HaltedByIterationCap`/`HaltedByDoomLoop`, plus the legacy bare
/// `Halted`) reads as a calm pause rather than an error
/// (`docs/issues/002-agent-iteration-cap-halts-real-work.md`'s resolution):
/// `is_error: false`, the same treatment `Cancelled` gets, and text naming
/// the specific guard that fired (the legacy variant can't, since it never
/// recorded which one).
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
        TurnEndReason::HaltedByIterationCap => ReceiptStatus {
            text: format!(
                "paused after {DEFAULT_ITERATION_CAP} consecutive tool-driven turns · {elapsed}"
            ),
            is_error: false,
        },
        TurnEndReason::HaltedByDoomLoop => ReceiptStatus {
            text: format!(
                "paused after {DEFAULT_DOOM_LOOP_WINDOW} consecutive identical tool results · \
                 {elapsed}"
            ),
            is_error: false,
        },
        TurnEndReason::Halted => ReceiptStatus {
            text: format!("paused · {elapsed}"),
            is_error: false,
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

/// A tool call's class for collapsed-receipt aggregation (owner feedback
/// 2026-07-13 -- "rows of glob/grep/read chips carry no information",
/// see `docs/agent-output-ui-amendment.md`'s post-review note): `Edit`
/// and `Query` calls fold into prose counts on the receipt line; `Bash`
/// always stays individual chips (the command itself is meaningful, per
/// the owner's own framing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallClass {
    Edit,
    Bash,
    Query,
}

/// The collapsed receipt line's aggregated view. `query_count` counts
/// successful `Query`-class calls *excluding* `fs.read` (which gets its
/// own `read_file_count` instead, expressed as *distinct file paths* so
/// re-reading the same file within a turn doesn't inflate the count);
/// `edited_file_count` is the same distinct-path treatment for
/// successful `Edit`-class calls; `bash_count` is the plain call count
/// for successful `Bash`-class calls (owner feedback 2026-07-13, round 3
/// follow-up: a turn with a dozen near-identical `cd … && …` bash chips
/// conveyed nothing either, the same complaint that motivated the
/// query/edit aggregation -- bash folds into prose too now).
/// `individual_calls` (any failed call of any class, plus the defensive
/// case of a call that never finished within a supposedly completed
/// turn) is the only thing left rendering as its own chip, so a failure
/// -- or an anomaly -- never goes silently missing from the collapsed
/// line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ReceiptAggregate {
    pub query_count: usize,
    pub read_file_count: usize,
    pub edited_file_count: usize,
    pub bash_count: usize,
    pub individual_calls: Vec<ToolCallView>,
}

/// Aggregates `tool_calls` (a single receipt's worth, from
/// [`build_tool_call_views`]) into a [`ReceiptAggregate`]. Order within
/// `individual_calls` follows `tool_calls`' own (first-request) order.
pub(crate) fn aggregate_receipt(tool_calls: &[ToolCallView]) -> ReceiptAggregate {
    let mut aggregate = ReceiptAggregate::default();
    let mut read_paths: HashSet<String> = HashSet::new();
    let mut edited_paths: HashSet<String> = HashSet::new();

    for call in tool_calls {
        if call.is_error || !call.finished {
            // A failed call never aggregates, regardless of class (the
            // owner's explicit requirement) -- nor does the defensive
            // "never finished within a completed turn" case, which
            // shouldn't happen by contract but must not silently vanish
            // into a count either.
            aggregate.individual_calls.push(call.clone());
            continue;
        }
        match classify_call(&call.tool_id) {
            CallClass::Edit => {
                if let Some(path) = &call.target {
                    edited_paths.insert(path.clone());
                }
            }
            CallClass::Bash => aggregate.bash_count += 1,
            CallClass::Query if call.tool_id == "fs.read" => {
                if let Some(path) = &call.target {
                    read_paths.insert(path.clone());
                }
            }
            CallClass::Query => aggregate.query_count += 1,
        }
    }

    aggregate.read_file_count = read_paths.len();
    aggregate.edited_file_count = edited_paths.len();
    aggregate
}

/// The collapsed receipt line's prose prefix (owner feedback
/// 2026-07-13): `None` when every aggregated count is zero (e.g. an
/// all-individual-chips turn), so the line never shows a hollow "0 tool
/// calls" -- it just goes straight to whatever chips/status/model
/// follow.
pub(crate) fn receipt_prose(aggregate: &ReceiptAggregate) -> Option<String> {
    let mut parts = Vec::new();
    if aggregate.query_count > 0 {
        parts.push(pluralize(aggregate.query_count, "tool call", "tool calls"));
    }
    if aggregate.read_file_count > 0 {
        parts.push(format!(
            "read {}",
            pluralize(aggregate.read_file_count, "file", "files")
        ));
    }
    if aggregate.edited_file_count > 0 {
        parts.push(format!(
            "edited {}",
            pluralize(aggregate.edited_file_count, "file", "files")
        ));
    }
    if aggregate.bash_count > 0 {
        parts.push(format!(
            "ran {}",
            pluralize(aggregate.bash_count, "command", "commands")
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::build_tool_call_views;
    use super::super::test_support::*;
    use super::*;

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
            receipt_status(&end(TurnEndReason::HaltedByIterationCap)),
            ReceiptStatus {
                text: format!(
                    "paused after {DEFAULT_ITERATION_CAP} consecutive tool-driven turns · 38s"
                ),
                is_error: false
            },
            "a guard halt reads as a calm pause, not an error"
        );
        assert_eq!(
            receipt_status(&end(TurnEndReason::HaltedByDoomLoop)),
            ReceiptStatus {
                text: format!(
                    "paused after {DEFAULT_DOOM_LOOP_WINDOW} consecutive identical tool \
                     results · 38s"
                ),
                is_error: false
            }
        );
        assert_eq!(
            receipt_status(&end(TurnEndReason::Halted)),
            ReceiptStatus {
                text: "paused · 38s".to_string(),
                is_error: false
            },
            "the legacy bare Halted reason (pre-resolution persisted logs) reads calmly too"
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
    fn classify_call_sorts_every_tool_id_into_its_class() {
        assert_eq!(classify_call("fs.edit"), CallClass::Edit);
        assert_eq!(classify_call("fs.write"), CallClass::Edit);
        assert_eq!(classify_call("bash"), CallClass::Bash);
        for tool_id in [
            "fs.read",
            "fs.grep",
            "fs.glob",
            "recall.search",
            "recall.read",
            "workspace.snapshot",
            "skill.read",
            "some.future.tool",
        ] {
            assert_eq!(classify_call(tool_id), CallClass::Query, "{tool_id}");
        }
    }

    #[test]
    fn aggregate_receipt_folds_mixed_classes_into_prose_counts() {
        let items = vec![
            tool_requested("q1", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_finished("q1", json!({"returned_count": 1})),
            tool_requested(
                "q2",
                "fs.glob",
                json!({"base_path": ".", "pattern": "*.rs"}),
            ),
            tool_finished("q2", json!({"returned_count": 2})),
            tool_requested("r1", "fs.read", json!({"path": "a.rs"})),
            tool_finished("r1", json!({"total_lines": 10})),
            tool_requested(
                "e1",
                "fs.edit",
                json!({"path": "b.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("e1", json!({"path": "b.rs", "replaced": true})),
            tool_requested("b1", "bash", json!({"command": "cargo test"})),
            tool_finished("b1", json!({"exit_code": 0, "output": ""})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        assert_eq!(aggregate.query_count, 2); // fs.grep + fs.glob
        assert_eq!(aggregate.read_file_count, 1);
        assert_eq!(aggregate.edited_file_count, 1);
        assert_eq!(aggregate.bash_count, 1);
        assert!(aggregate.individual_calls.is_empty());
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("2 tool calls · read 1 file · edited 1 file · ran 1 command")
        );
    }

    #[test]
    fn aggregate_receipt_counts_distinct_paths_not_call_counts() {
        let items = vec![
            tool_requested("r1", "fs.read", json!({"path": "a.rs"})),
            tool_finished("r1", json!({"total_lines": 10})),
            tool_requested("r2", "fs.read", json!({"path": "a.rs"})),
            tool_finished("r2", json!({"total_lines": 10})),
            tool_requested("r3", "fs.read", json!({"path": "b.rs"})),
            tool_finished("r3", json!({"total_lines": 5})),
            tool_requested(
                "e1",
                "fs.edit",
                json!({"path": "c.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("e1", json!({"path": "c.rs", "replaced": true})),
            tool_requested("e2", "fs.write", json!({"path": "c.rs", "content": "z"})),
            tool_finished("e2", json!({"path": "c.rs", "created": false})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        // Two reads of a.rs collapse to one distinct path; b.rs adds a
        // second. An edit and a write to the same c.rs collapse to one
        // distinct edited path.
        assert_eq!(aggregate.read_file_count, 2);
        assert_eq!(aggregate.edited_file_count, 1);
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("read 2 files · edited 1 file")
        );
    }

    #[test]
    fn aggregate_receipt_breaks_out_a_failed_call_of_any_class_individually() {
        let items = vec![
            tool_requested("q1", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_finished("q1", json!({"returned_count": 1})),
            tool_requested("bad_read", "fs.read", json!({"path": "missing.rs"})),
            tool_finished(
                "bad_read",
                json!({"is_error": true, "message": "not found"}),
            ),
            tool_requested(
                "bad_edit",
                "fs.edit",
                json!({"path": "d.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished(
                "bad_edit",
                json!({"is_error": true, "message": "old_string not found"}),
            ),
            tool_requested("bad_bash", "bash", json!({"command": "false"})),
            tool_finished(
                "bad_bash",
                json!({"is_error": true, "message": "boom", "exit_code": 1}),
            ),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        // The failed read, edit, and bash never reach any count...
        assert_eq!(aggregate.read_file_count, 0);
        assert_eq!(aggregate.edited_file_count, 0);
        assert_eq!(aggregate.bash_count, 0);
        assert_eq!(aggregate.query_count, 1); // only the successful grep
                                              // ...and stay individually chip-able instead, regardless of class.
        assert_eq!(aggregate.individual_calls.len(), 3);
        let individual_ids: Vec<&str> = aggregate
            .individual_calls
            .iter()
            .map(|call| call.call_id.0.as_str())
            .collect();
        assert!(individual_ids.contains(&"bad_read"));
        assert!(individual_ids.contains(&"bad_edit"));
        assert!(individual_ids.contains(&"bad_bash"));
    }

    #[test]
    fn receipt_prose_uses_singular_wording_for_a_count_of_one() {
        let aggregate = ReceiptAggregate {
            query_count: 1,
            read_file_count: 1,
            edited_file_count: 1,
            bash_count: 1,
            ..Default::default()
        };
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("1 tool call · read 1 file · edited 1 file · ran 1 command")
        );
    }

    #[test]
    fn receipt_prose_uses_plural_wording_above_one() {
        let aggregate = ReceiptAggregate {
            query_count: 3,
            read_file_count: 2,
            edited_file_count: 5,
            bash_count: 4,
            ..Default::default()
        };
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("3 tool calls · read 2 files · edited 5 files · ran 4 commands")
        );
    }

    #[test]
    fn receipt_prose_is_none_when_every_count_is_zero() {
        // An all-individual-chip turn (every call failed, or is the
        // defensive never-finished case): the collapsed line still
        // shows those chips plus status/elapsed (view concern), but the
        // prose prefix itself is simply absent.
        assert_eq!(receipt_prose(&ReceiptAggregate::default()), None);
    }

    #[test]
    fn aggregate_receipt_folds_bash_into_the_ran_commands_count() {
        // Owner feedback 2026-07-13 (round 3 follow-up): a dozen
        // near-identical bash chips (e.g. every command sharing the same
        // `cd … && …` prefix) conveyed nothing -- bash now aggregates
        // into prose exactly like query/edit calls, leaving no chip
        // behind for a successful run.
        let items = vec![
            tool_requested("b1", "bash", json!({"command": "cargo build"})),
            tool_finished("b1", json!({"exit_code": 0, "output": ""})),
            tool_requested("b2", "bash", json!({"command": "cargo test"})),
            tool_finished("b2", json!({"exit_code": 0, "output": ""})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let aggregate = aggregate_receipt(&tool_calls);
        assert_eq!(aggregate.bash_count, 2);
        assert!(aggregate.individual_calls.is_empty());
        assert_eq!(receipt_prose(&aggregate).as_deref(), Some("ran 2 commands"));
    }

    #[test]
    fn receipt_tail_final_carries_the_turn_end_while_intermediate_carries_nothing() {
        // Pins the two `ReceiptTail` variants' own shapes: `Final` wraps
        // a `&TurnEnd` (status/elapsed/model all recoverable from it via
        // `receipt_status`/its own fields), `Intermediate` is a unit
        // variant with nothing to recover at all -- the render side is
        // the one place status/elapsed/model ever get read from `tail`,
        // but this pins that `Intermediate` truly carries none of them
        // before that render-side code ever runs.
        let end = TurnEnd {
            reason: TurnEndReason::Completed,
            model: Some("gpt-5".to_string()),
            elapsed: Duration::from_secs(38),
        };
        match ReceiptTail::Final(&end) {
            ReceiptTail::Final(end) => {
                assert_eq!(receipt_status(end).text, "38s");
                assert_eq!(end.model.as_deref(), Some("gpt-5"));
            }
            ReceiptTail::Intermediate => panic!("expected Final"),
        }
        assert!(matches!(
            ReceiptTail::Intermediate,
            ReceiptTail::Intermediate
        ));
    }
}
