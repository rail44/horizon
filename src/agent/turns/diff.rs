//! The Changes overview bar's summary text. The reconstructed-diff and
//! file-change aggregation this is built on (`FileChange`/
//! `aggregate_changes`/`DiffLine`/`DiffLineKind`/`reconstruct_line_diff`)
//! moved to `horizon_agent::transcript`, re-exported from `super` under
//! their original names (see `turns/mod.rs`'s doc comment).

use super::{pluralize, FileChange};

/// The Changes overview bar's summary text (decision 9): `None` when no
/// file was ever edited/written this session -- the bar itself is hidden
/// entirely in that case (the view gates on this, not a separate emptiness
/// check on `aggregate_changes`'s own output, so the two can never
/// drift). `+`/`−` counts sum every aggregated file's own diffstat,
/// inheriting `aggregate_changes`'s documented "summed hunk stats, not a
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::test_support::*;
    use super::super::{aggregate_changes, build_tool_call_views};
    use super::*;

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
