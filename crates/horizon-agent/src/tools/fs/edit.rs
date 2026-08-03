//! `fs.edit`: a list of exact-string replacements applied in one call.
//!
//! The input is a list and only a list — there is no single-edit top-level
//! shape — so batching several hunks of one file, or edits across several
//! files, is the ordinary path rather than a capability the model has to
//! discover. The measurement that motivated this is
//! `docs/research/agent-editing-phase-analysis-2026-07-28.md`.
//!
//! The contract every caller depends on:
//!
//! * Edits apply **sequentially in list order**. The first failing edit
//!   stops the call. Edits already applied stay on disk — nothing is
//!   rolled back, because reverting files the model never asked to revert
//!   would fabricate state — so a partial application is reported
//!   precisely instead.
//! * The result always lists **every** edit's outcome in input order:
//!   `applied` (with its path and `occurrences`), `failed` (with the same
//!   error text a single edit would have produced), or `not_attempted`.
//!   A failed call also carries `failed_index`, so the model can re-read
//!   the affected files and resume from exactly that position.
//! * Per edit the rules are unchanged from the single-edit tool: the file
//!   must have been read this session and be unchanged on disk since, and
//!   `old_string` must match exactly once unless `replace_all` is set.
//! * Edits to the same file **compose**. Each edit is written before the
//!   next one reads the file, so a later edit sees the earlier one's
//!   result, and a path this call already wrote is exempt from the
//!   staleness gate for the rest of the call — this call is the author of
//!   that change, so it must never read as an external modification.
//! * A malformed `edits` list is rejected before any file is touched, so a
//!   shape error never leaves a partially applied call behind.

use std::{collections::HashSet, fs, path::PathBuf};

use serde_json::{json, Value};

use super::error_output;
use super::locks::FileLocks;
use super::safety::resolve_path;
use super::staleness::check_staleness;
use crate::tools::state::ToolSessionState;

struct Edit<'a> {
    path: &'a str,
    old_string: &'a str,
    new_string: &'a str,
    replace_all: bool,
}

pub(super) fn execute(tool_state: &ToolSessionState, input: &Value) -> Value {
    let edits = match parse_edits(input) {
        Ok(edits) => edits,
        Err(error) => return error,
    };

    // Every resolvable target is locked for the whole call, in the lexical
    // order `FileLocks` imposes, so an overlapping concurrent mutation
    // can't interleave between two edits of this list. A path that fails
    // to resolve is not locked here; the apply loop reports it as that
    // edit's own failure.
    let lock_paths = edits
        .iter()
        .filter_map(|edit| resolve_path(tool_state, edit.path).ok())
        .collect::<Vec<_>>();
    let locks = FileLocks::acquire(lock_paths);
    let _guards = locks.hold();

    let mut written: HashSet<PathBuf> = HashSet::new();
    let mut applied_paths: Vec<String> = Vec::new();
    let mut outcomes = Vec::with_capacity(edits.len());
    let mut failure: Option<(usize, String)> = None;

    for (index, edit) in edits.iter().enumerate() {
        if failure.is_some() {
            outcomes.push(json!({
                "index": index,
                "path": edit.path,
                "status": "not_attempted",
            }));
            continue;
        }
        match apply_one(tool_state, edit, &mut written) {
            Ok(occurrences) => {
                if !applied_paths.iter().any(|path| path == edit.path) {
                    applied_paths.push(edit.path.to_string());
                }
                outcomes.push(json!({
                    "index": index,
                    "path": edit.path,
                    "status": "applied",
                    "occurrences": occurrences,
                }));
            }
            Err(message) => {
                outcomes.push(json!({
                    "index": index,
                    "path": edit.path,
                    "status": "failed",
                    "message": message,
                }));
                failure = Some((index, message));
            }
        }
    }

    let applied_count = outcomes
        .iter()
        .filter(|outcome| outcome["status"] == "applied")
        .count();
    let file_count = applied_paths.len();

    match failure {
        None => json!({
            "edits": outcomes,
            "applied_count": applied_count,
            "file_count": file_count,
        }),
        Some((index, message)) => {
            let not_attempted = edits.len() - index - 1;
            let mut value = error_output(format!(
                "edit at index {index} failed: {message}. {applied_count} earlier edit(s) \
                 were applied and remain on disk; {not_attempted} later edit(s) were not \
                 attempted — re-read the affected files and resend from index {index}."
            ));
            if let Some(map) = value.as_object_mut() {
                map.insert("failed_index".to_string(), json!(index));
                map.insert("edits".to_string(), json!(outcomes));
                map.insert("applied_count".to_string(), json!(applied_count));
                map.insert("file_count".to_string(), json!(file_count));
            }
            value
        }
    }
}

/// Validates the whole list before anything is written: a shape error must
/// never leave some edits applied, so it is reported as a plain call-level
/// error rather than through the per-edit outcome list.
fn parse_edits(input: &Value) -> Result<Vec<Edit<'_>>, Value> {
    let Some(entries) = input.get("edits").and_then(Value::as_array) else {
        return Err(error_output(
            "fs.edit requires an `edits` array argument — pass every replacement in that one \
             list, including a single edit",
        ));
    };
    if entries.is_empty() {
        return Err(error_output("`edits` must contain at least one edit"));
    }

    let mut edits = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let Some(path) = entry.get("path").and_then(Value::as_str) else {
            return Err(error_output(format!(
                "edit at index {index} requires a `path` string argument"
            )));
        };
        let Some(old_string) = entry.get("old_string").and_then(Value::as_str) else {
            return Err(error_output(format!(
                "edit at index {index} requires an `old_string` string argument"
            )));
        };
        let Some(new_string) = entry.get("new_string").and_then(Value::as_str) else {
            return Err(error_output(format!(
                "edit at index {index} requires a `new_string` string argument"
            )));
        };
        if old_string.is_empty() {
            return Err(error_output(format!(
                "`old_string` of the edit at index {index} must not be empty"
            )));
        }
        if old_string == new_string {
            return Err(error_output(format!(
                "`old_string` and `new_string` of the edit at index {index} are identical — \
                 nothing to edit"
            )));
        }
        edits.push(Edit {
            path,
            old_string,
            new_string,
            replace_all: entry
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(edits)
}

/// Applies one edit, returning its replacement count. The error is the
/// plain message this edit's outcome entry reports — the same text the
/// tool produced when it took a single edit per call.
fn apply_one(
    tool_state: &ToolSessionState,
    edit: &Edit<'_>,
    written: &mut HashSet<PathBuf>,
) -> Result<usize, String> {
    let path_arg = edit.path;
    let resolved = resolve_path(tool_state, path_arg).map_err(|error| message_of(&error))?;

    if !resolved.is_file() {
        return Err(format!(
            "`{path_arg}` does not exist as a file — use fs.write to create it"
        ));
    }

    // Skipped for a path this same call already wrote: the recorded mtime
    // does track that write, but the gate exists to catch *other* writers,
    // and this call's own evolving content must not be able to trip it.
    if !written.contains(&resolved) {
        check_staleness(tool_state, &resolved, path_arg).map_err(|error| message_of(&error))?;
    }

    let content = fs::read_to_string(&resolved)
        .map_err(|error| format!("cannot read `{path_arg}` as UTF-8 text: {error}"))?;

    let match_count = content.matches(edit.old_string).count();
    if match_count == 0 {
        return Err(format!(
            "`old_string` not found in `{path_arg}` — check the exact text (including whitespace) and try again"
        ));
    }
    if match_count > 1 && !edit.replace_all {
        return Err(format!(
            "found {match_count} matches for `old_string` in `{path_arg}` — include more surrounding context to make it unique, or set `replace_all: true` to replace every occurrence"
        ));
    }

    let updated = if edit.replace_all {
        content.replace(edit.old_string, edit.new_string)
    } else {
        content.replacen(edit.old_string, edit.new_string, 1)
    };
    fs::write(&resolved, &updated)
        .map_err(|error| format!("failed to write `{path_arg}`: {error}"))?;

    if let Ok(mtime) = fs::metadata(&resolved).and_then(|metadata| metadata.modified()) {
        tool_state.record_mtime(resolved.clone(), mtime);
    }
    written.insert(resolved);

    Ok(match_count)
}

/// Unwraps the message out of an [`error_output`]-shaped value, so a helper
/// that reports call-level errors can contribute a per-edit outcome instead.
fn message_of(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error")
        .to_string()
}
