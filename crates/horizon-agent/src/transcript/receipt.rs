//! The collapsed-receipt aggregation (owner feedback 2026-07-13: query/
//! edit/bash calls fold into prose counts rather than a row of low-signal
//! chips). This is the structural half only -- status/duration/prose
//! *text* stayed in the `horizon` binary crate's `src/agent/turns`, along
//! with `ReceiptTail` (a thin view-side wrapper with no structural
//! consumer of its own; see `transcript`'s module doc).

use std::collections::HashSet;

use super::classify_call;
use super::tool_call::ToolCallView;

/// A tool call's class for collapsed-receipt aggregation (owner feedback
/// 2026-07-13 -- "rows of glob/grep/read chips carry no information",
/// see `docs/agent-output-ui-amendment.md`'s post-review note): `Edit`
/// and `Query` calls fold into prose counts on the receipt line; `Bash`
/// always stays individual chips (the command itself is meaningful, per
/// the owner's own framing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallClass {
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
pub struct ReceiptAggregate {
    pub query_count: usize,
    pub read_file_count: usize,
    pub edited_file_count: usize,
    pub bash_count: usize,
    pub individual_calls: Vec<ToolCallView>,
}

/// Aggregates `tool_calls` (a single receipt's worth, from
/// [`super::build_tool_call_views`]) into a [`ReceiptAggregate`]. Order
/// within `individual_calls` follows `tool_calls`' own (first-request)
/// order.
pub fn aggregate_receipt(tool_calls: &[ToolCallView]) -> ReceiptAggregate {
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
                for file in &call.affected_files {
                    edited_paths.insert(file.path.clone());
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::build_tool_call_views;
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn classify_call_sorts_every_tool_id_into_its_class() {
        assert_eq!(classify_call("fs.edit"), CallClass::Edit);
        assert_eq!(classify_call("fs.write"), CallClass::Edit);
        assert_eq!(classify_call("fs.patch"), CallClass::Edit);
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
}
