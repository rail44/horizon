use serde_json::Value;

use super::util::{line_diffstat, str_field};
use super::view::FileEffect;

pub(super) fn affected_files(
    tool_id: &str,
    input: &Value,
    output: Option<&Value>,
) -> Vec<FileEffect> {
    let Some(output) = output else {
        return Vec::new();
    };
    use crate::contract::tool_output::{decode, EditOutcome, FileEdits, FileWritten};
    match tool_id {
        "fs.edit" => decode::<FileEdits>(output)
            .map(|result| {
                let inputs = edit_entries(input);
                result
                    .edits
                    .into_iter()
                    .filter_map(|receipt| {
                        let EditOutcome::Applied { occurrences } = receipt.outcome else {
                            return None;
                        };
                        let edit = inputs
                            .get(receipt.index)
                            .filter(|edit| edit.path == receipt.path)?;
                        let (added, removed) = line_diffstat(edit.old_string, edit.new_string);
                        let occurrences = u32::try_from(occurrences).unwrap_or(u32::MAX);
                        Some(FileEffect {
                            path: receipt.path,
                            added: added.saturating_mul(occurrences),
                            removed: removed.saturating_mul(occurrences),
                            created: false,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "fs.write" => decode::<FileWritten>(output)
            .map(|result| {
                vec![FileEffect {
                    path: result.path,
                    added: 0,
                    removed: 0,
                    created: result.created,
                }]
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// One entry of an `fs.edit` call's `edits` list (`crate::tools::fs::edit`).
/// Public alongside [`edit_entries`] so `src/agent/turns`'s own body
/// renderer reads the batch exactly the way this classifier does.
pub struct EditEntry<'a> {
    pub path: &'a str,
    pub old_string: &'a str,
    pub new_string: &'a str,
}

/// Reads an `fs.edit` call's `edits` list in input order. An entry with no
/// `path` is skipped (there is nothing to display it against); a missing or
/// non-array `edits` yields an empty list, so a malformed call renders as an
/// empty batch rather than panicking.
pub fn edit_entries(input: &Value) -> Vec<EditEntry<'_>> {
    input
        .get("edits")
        .and_then(Value::as_array)
        .map(|edits| {
            edits
                .iter()
                .filter_map(|edit| {
                    Some(EditEntry {
                        path: str_field(edit, "path")?,
                        old_string: str_field(edit, "old_string").unwrap_or_default(),
                        new_string: str_field(edit, "new_string").unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The distinct paths an `fs.edit` batch touches, in first-touch order.
pub(super) fn distinct_edit_paths<'a>(edits: &[EditEntry<'a>]) -> Vec<&'a str> {
    let mut paths: Vec<&str> = Vec::new();
    for edit in edits {
        if !paths.contains(&edit.path) {
            paths.push(edit.path);
        }
    }
    paths
}
