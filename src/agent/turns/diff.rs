//! Reconstructed line diffs (`fs.edit`'s body/chip diffstat) and the
//! session-wide "Changes overview" aggregation built from them.

use super::receipt::CallClass;
use super::tool_call::{ToolCallKind, ToolCallView};
use super::{classify_call, file_name, pluralize};

/// One file's cumulative edit/write activity across the *whole session*
/// (every turn, not just whichever receipt/burst is currently rendering)
/// -- the pane's collapsible "Changes overview"
/// (`docs/agent-output-ui-design.md` decision 9, never ported from the
/// retired Floem shell's own `session_changes` pure function; rebuilt
/// fresh here against this shell's own `ToolCallView`/
/// `build_tool_call_views` shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileChange {
    pub path: String,
    pub file_name: String,
    pub added: u32,
    pub removed: u32,
    /// Set once any successful `fs.write` call against this path reported
    /// `created: true` (the same `output.get("created")` convention
    /// `classify`'s `fs.write` arm reads). `fs.write` never produces a
    /// diffstat at all (`ToolCallKind::File::diffstat` is `None` for it --
    /// it replaces wholesale rather than diffing), so this flag is the
    /// overview's only signal from a write call.
    pub created: bool,
}

/// Aggregates every successful, finished `fs.edit`/`fs.write` call in
/// `tool_calls` -- the *whole session's* [`build_tool_call_views`] output,
/// not one turn/burst's -- into one [`FileChange`] per distinct path,
/// ordered by each path's first touch. A failed call (`is_error`) or one
/// still in flight contributes nothing, the same "failures never
/// aggregate" rule [`aggregate_receipt`] follows -- simplified here to a
/// plain skip, since this overview has no per-call chip fallback to fall
/// back to.
///
/// **Honest limitation**: multiple edits to the same file have their
/// diffstats *summed*, not combined into a net diff across the file's
/// whole session history -- two edits that each touch 3 lines report `+6
/// −6` here even if the second fully reverted the first's changes.
/// [`ToolCallKind::File::diffstat`] is itself only a per-call
/// reconstruction (`reconstruct_line_diff`'s common-prefix/common-suffix
/// approximation against that one call's own `old_string`/`new_string`),
/// and this aggregation has no access to the file's real end-to-end
/// content to diff against instead.
pub(crate) fn aggregate_changes(tool_calls: &[ToolCallView]) -> Vec<FileChange> {
    let mut changes: Vec<FileChange> = Vec::new();
    for call in tool_calls {
        if call.is_error || !call.finished || classify_call(&call.tool_id) != CallClass::Edit {
            continue;
        }
        let Some(path) = &call.target else {
            continue;
        };
        let entry = match changes.iter_mut().find(|change| &change.path == path) {
            Some(entry) => entry,
            None => {
                changes.push(FileChange {
                    path: path.clone(),
                    file_name: file_name(path),
                    added: 0,
                    removed: 0,
                    created: false,
                });
                changes.last_mut().expect("just pushed")
            }
        };
        if let ToolCallKind::File {
            diffstat: Some((added, removed)),
            ..
        } = &call.kind
        {
            entry.added += added;
            entry.removed += removed;
        }
        if call.tool_id == "fs.write" && call.result_summary.as_deref() == Some("created") {
            entry.created = true;
        }
    }
    changes
}

/// The Changes overview bar's summary text (decision 9): `None` when no
/// file was ever edited/written this session -- the bar itself is hidden
/// entirely in that case (the view gates on this, not a separate emptiness
/// check on [`aggregate_changes`]'s own output, so the two can never
/// drift). `+`/`−` counts sum every aggregated file's own diffstat,
/// inheriting [`aggregate_changes`]'s documented "summed hunk stats, not a
/// net diff" limitation.
pub(crate) fn changes_summary_text(changes: &[FileChange]) -> Option<String> {
    if changes.is_empty() {
        return None;
    }
    let added: u32 = changes.iter().map(|change| change.added).sum();
    let removed: u32 = changes.iter().map(|change| change.removed).sum();
    Some(format!(
        "{} · +{added} −{removed}",
        pluralize(changes.len(), "file", "files")
    ))
}

/// One line of a reconstructed diff body (stage D's fs.edit expansion,
/// `docs/agent-output-ui-design.md` decision 4): `Context` lines are the
/// common prefix/suffix trimmed below, painted with neither role;
/// `Added`/`Removed` pair with `theme::diff_added_*`/`diff_removed_*` in
/// the view (line background carries the change, sign column colored
/// separately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// Reconstructs a full line diff between `old` and `new` by trimming the
/// common prefix/suffix (kept as `Context` lines) and pairing the
/// remaining middle as removed-then-added -- not a full diff algorithm
/// (no interior-line matching), matching `fs.edit`'s single
/// old_string/new_string replacement shape. Operates on `&str` lines
/// throughout, so multibyte content (e.g. Japanese text) round-trips
/// unmodified -- no byte-level slicing here.
pub(crate) fn reconstruct_line_diff(old: &str, new: &str) -> Vec<DiffLine> {
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

    let mut lines = Vec::new();
    for text in &old_lines[..prefix] {
        lines.push(DiffLine {
            kind: DiffLineKind::Context,
            text: (*text).to_string(),
        });
    }
    for text in &old_lines[prefix..old_lines.len() - suffix] {
        lines.push(DiffLine {
            kind: DiffLineKind::Removed,
            text: (*text).to_string(),
        });
    }
    for text in &new_lines[prefix..new_lines.len() - suffix] {
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            text: (*text).to_string(),
        });
    }
    for text in &old_lines[old_lines.len() - suffix..] {
        lines.push(DiffLine {
            kind: DiffLineKind::Context,
            text: (*text).to_string(),
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::build_tool_call_views;
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn reconstruct_line_diff_handles_a_pure_insert() {
        let lines = reconstruct_line_diff("a\nb", "a\nnew\nb");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Added, "new"),
                (DiffLineKind::Context, "b"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_handles_a_pure_delete() {
        let lines = reconstruct_line_diff("a\nold\nb", "a\nb");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Removed, "old"),
                (DiffLineKind::Context, "b"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_handles_a_mixed_change() {
        let lines = reconstruct_line_diff("a\nold1\nold2\nb", "a\nnew1\nb");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Removed, "old1"),
                (DiffLineKind::Removed, "old2"),
                (DiffLineKind::Added, "new1"),
                (DiffLineKind::Context, "b"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_of_identical_strings_is_all_context() {
        let lines = reconstruct_line_diff("a\nb\nc", "a\nb\nc");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Context, "b"),
                (DiffLineKind::Context, "c"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_round_trips_multibyte_content() {
        let lines = reconstruct_line_diff(
            "こんにちは\n古い行\nさようなら",
            "こんにちは\n新しい行\nさようなら",
        );
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "こんにちは"),
                (DiffLineKind::Removed, "古い行"),
                (DiffLineKind::Added, "新しい行"),
                (DiffLineKind::Context, "さようなら"),
            ]
        );
    }

    #[test]
    fn aggregate_changes_is_empty_when_no_file_was_ever_touched() {
        let items = vec![
            tool_requested("q1", "fs.grep", json!({"base_path": ".", "pattern": "x"})),
            tool_finished("q1", json!({"returned_count": 1})),
        ];
        let tool_calls = build_tool_call_views(&items);
        assert!(aggregate_changes(&tool_calls).is_empty());
        assert_eq!(changes_summary_text(&aggregate_changes(&tool_calls)), None);
    }

    #[test]
    fn aggregate_changes_sums_diffstats_across_multiple_edits_to_one_file() {
        // Decision 9's documented "summed hunk stats, not a net diff"
        // limitation: two edits to the same file each contribute their
        // own reconstructed diffstat, added together.
        let items = vec![
            tool_requested(
                "e1",
                "fs.edit",
                json!({"path": "src/a.rs", "old_string": "x", "new_string": "y\nz"}),
            ),
            tool_finished("e1", json!({"path": "src/a.rs", "replaced": true})),
            tool_requested(
                "e2",
                "fs.edit",
                json!({"path": "src/a.rs", "old_string": "y\nz", "new_string": "w"}),
            ),
            tool_finished("e2", json!({"path": "src/a.rs", "replaced": true})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let changes = aggregate_changes(&tool_calls);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "src/a.rs");
        assert_eq!(changes[0].file_name, "a.rs");
        // e1: +2 -1 (y\nz replaces x); e2: +1 -2 (w replaces y\nz).
        assert_eq!(changes[0].added, 3);
        assert_eq!(changes[0].removed, 3);
        assert!(!changes[0].created);
    }

    #[test]
    fn aggregate_changes_orders_by_first_touch() {
        let items = vec![
            tool_requested(
                "e1",
                "fs.edit",
                json!({"path": "b.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("e1", json!({"path": "b.rs", "replaced": true})),
            tool_requested(
                "e2",
                "fs.edit",
                json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished("e2", json!({"path": "a.rs", "replaced": true})),
            // A second touch of b.rs must not move it later in the order.
            tool_requested(
                "e3",
                "fs.edit",
                json!({"path": "b.rs", "old_string": "y", "new_string": "z"}),
            ),
            tool_finished("e3", json!({"path": "b.rs", "replaced": true})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let changes = aggregate_changes(&tool_calls);
        let paths: Vec<&str> = changes.iter().map(|change| change.path.as_str()).collect();
        assert_eq!(paths, vec!["b.rs", "a.rs"]);
    }

    #[test]
    fn aggregate_changes_flags_fs_write_created() {
        let items = vec![
            tool_requested(
                "w1",
                "fs.write",
                json!({"path": "new.rs", "content": "fn main() {}"}),
            ),
            tool_finished("w1", json!({"path": "new.rs", "created": true})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let changes = aggregate_changes(&tool_calls);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].created);
        // fs.write never produces a diffstat.
        assert_eq!(changes[0].added, 0);
        assert_eq!(changes[0].removed, 0);
    }

    #[test]
    fn aggregate_changes_does_not_flag_an_overwrite_as_created() {
        let items = vec![
            tool_requested(
                "w1",
                "fs.write",
                json!({"path": "existing.rs", "content": "fn main() {}"}),
            ),
            tool_finished("w1", json!({"path": "existing.rs", "created": false})),
        ];
        let tool_calls = build_tool_call_views(&items);
        let changes = aggregate_changes(&tool_calls);
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].created);
    }

    #[test]
    fn aggregate_changes_excludes_a_failed_edit() {
        let items = vec![
            tool_requested(
                "e1",
                "fs.edit",
                json!({"path": "a.rs", "old_string": "x", "new_string": "y"}),
            ),
            tool_finished(
                "e1",
                json!({"is_error": true, "message": "old_string not found"}),
            ),
        ];
        let tool_calls = build_tool_call_views(&items);
        assert!(aggregate_changes(&tool_calls).is_empty());
    }

    #[test]
    fn aggregate_changes_ignores_non_edit_calls() {
        let items = vec![
            tool_requested("r1", "fs.read", json!({"path": "a.rs"})),
            tool_finished("r1", json!({"total_lines": 10})),
            tool_requested("b1", "bash", json!({"command": "cargo test"})),
            tool_finished("b1", json!({"exit_code": 0})),
        ];
        let tool_calls = build_tool_call_views(&items);
        assert!(aggregate_changes(&tool_calls).is_empty());
    }

    #[test]
    fn changes_summary_text_formats_files_and_totals() {
        let changes = vec![
            FileChange {
                path: "a.rs".to_string(),
                file_name: "a.rs".to_string(),
                added: 100,
                removed: 20,
                created: false,
            },
            FileChange {
                path: "b.rs".to_string(),
                file_name: "b.rs".to_string(),
                added: 20,
                removed: 16,
                created: true,
            },
        ];
        assert_eq!(
            changes_summary_text(&changes).as_deref(),
            Some("2 files · +120 −36")
        );
    }

    #[test]
    fn changes_summary_text_uses_singular_wording_for_one_file() {
        let changes = vec![FileChange {
            path: "a.rs".to_string(),
            file_name: "a.rs".to_string(),
            added: 2,
            removed: 1,
            created: false,
        }];
        assert_eq!(
            changes_summary_text(&changes).as_deref(),
            Some("1 file · +2 −1")
        );
    }
}
