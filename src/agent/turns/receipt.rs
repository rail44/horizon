//! Receipt status/duration text and the collapsed-receipt prose (owner
//! feedback 2026-07-13: query/edit/bash calls fold into prose counts
//! rather than a row of low-signal chips). The aggregation itself
//! (`CallClass`/`ReceiptAggregate`/`aggregate_receipt`) moved to
//! `horizon_agent::transcript` -- this file holds only the wording built
//! on top of it, re-exported from `super` under its original name (see
//! `turns/mod.rs`'s doc comment).

use std::time::Duration;

use horizon_agent::config::{DEFAULT_DOOM_LOOP_WINDOW, DEFAULT_ITERATION_CAP};
use horizon_agent::contract::TurnEndReason;

use super::{pluralize, ReceiptAggregate, TurnEnd};

/// The receipt row's trailing content (`render_receipt`'s `tail`
/// parameter). Round 5 (monotone burst splitting): only the turn's
/// *final* burst -- the one closed by `TurnEnded` -- carries the turn's
/// end-reason status, total elapsed, and model, exactly as a completed
/// turn's one receipt always has; every other burst's receipt
/// (including the last one while the turn is still running) carries
/// none of that -- the contract has no per-burst timing. `Final` carries
/// a `&TurnEnd` rather than duplicating its fields so it can never drift
/// from [`receipt_status`]'s own reading of it.
///
/// Stayed in this (wording) crate rather than moving to
/// `horizon_agent::transcript` alongside `TurnEnd`: it has no structural
/// consumer of its own -- `view.rs` decides `Final` vs. `Intermediate`
/// itself (whether this burst is the turn's last, closed one), and the
/// only thing this type is ever used for is selecting which of
/// [`receipt_status`]'s wording branches `render_receipt` takes. Moving
/// it would have stranded a borrowed-lifetime type in the crate with no
/// structural code ever touching it.
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
        // The legacy bare variant and the skew catch-all render the same
        // calm pause with no guard-specific sentence (`TurnEndReason::
        // Unknown`'s doc): a reason this build can't name must not read as
        // an error.
        TurnEndReason::Halted | TurnEndReason::Unknown(_) => ReceiptStatus {
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

    use super::super::test_support::*;
    use super::super::{aggregate_receipt, build_tool_call_views, segment_bursts};
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

    #[test]
    fn a_burst_reconstructs_the_same_receipt_content_a_completed_turns_own_aggregation_would() {
        // A closed burst's own item range feeds `aggregate_receipt`/
        // `receipt_prose` exactly the way a whole completed turn's items
        // used to -- proving per-burst aggregation reuses the existing
        // machinery verbatim, just scoped to the burst's own range. Lives
        // here (not with `segment_bursts`, which moved to
        // `horizon_agent::transcript`) because its punchline assertion is
        // on `receipt_prose`'s wording output.
        let items = vec![
            user_message("fix the bug"),
            tool_requested("a", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_finished("a", json!({"returned_count": 2})),
            tool_requested("b", "fs.read", json!({"path": "a.rs"})),
            tool_finished("b", json!({"total_lines": 10})),
            assistant_delta("Looking at the code, I"),
        ];
        let bursts = segment_bursts(&items);
        assert_eq!(bursts.len(), 1);
        let burst = &bursts[0];
        assert!(burst.closed);
        let tool_calls = build_tool_call_views(&items[burst.start..burst.end]);
        let aggregate = aggregate_receipt(&tool_calls);
        assert_eq!(
            receipt_prose(&aggregate).as_deref(),
            Some("1 tool call · read 1 file")
        );
        assert_eq!(aggregate.bash_count, 0);
        assert!(aggregate.individual_calls.is_empty());
    }
}
