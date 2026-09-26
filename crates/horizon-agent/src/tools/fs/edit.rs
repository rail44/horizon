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

use crate::tools::output::*;

use super::locks::FileLocks;
use super::safety::resolve_path;
use super::staleness::check_staleness;
use crate::tools::state::ToolSessionState;

use crate::tools::input::{Edit, EditFiles};

pub(in crate::tools) fn execute(tool_state: &ToolSessionState, input: &EditFiles) -> Response {
    let edits = &input.edits;

    // Every resolvable target is locked for the whole call, in the lexical
    // order `FileLocks` imposes, so an overlapping concurrent mutation
    // can't interleave between two edits of this list. A path that fails
    // to resolve is not locked here; the apply loop reports it as that
    // edit's own failure.
    let lock_paths = edits
        .iter()
        .filter_map(|edit| resolve_path(tool_state, &edit.path, false).ok())
        .collect::<Vec<_>>();
    let locks = FileLocks::acquire(lock_paths);
    let _guards = locks.hold();

    let mut written: HashSet<PathBuf> = HashSet::new();
    let mut applied_paths: Vec<String> = Vec::new();
    let mut outcomes = Vec::with_capacity(edits.len());
    let mut failure: Option<(usize, String)> = None;

    for (index, edit) in edits.iter().enumerate() {
        let outcome = if failure.is_some() {
            EditOutcome::NotAttempted
        } else {
            match apply_one(tool_state, edit, &mut written) {
                Ok(occurrences) => {
                    if !applied_paths.contains(&edit.path) {
                        applied_paths.push(edit.path.clone());
                    }
                    EditOutcome::Applied { occurrences }
                }
                Err(message) => {
                    failure = Some((index, message.clone()));
                    EditOutcome::Failed { message }
                }
            }
        };
        outcomes.push(EditReceipt {
            index,
            path: edit.path.clone(),
            outcome,
        });
    }
    let applied_count = outcomes
        .iter()
        .filter(|edit| matches!(edit.outcome, EditOutcome::Applied { .. }))
        .count();
    let failed_index = failure.as_ref().map(|(index, _)| *index);
    let message = failure.map(|(index, message)| {
        let not_attempted = edits.len() - index - 1;
        format!(
            "edit at index {index} failed: {message}. {applied_count} earlier edit(s) \
            were applied and remain on disk; {not_attempted} later edit(s) were not \
            attempted — re-read the affected files and resend from index {index}."
        )
    });
    let output = FileEdits {
        edits: outcomes,
        applied_count,
        file_count: applied_paths.len(),
        failed_index,
        message,
    };
    if output.failed_index.is_some() {
        Response::failed(output)
    } else {
        Response::succeeded(output)
    }
}

fn apply_one(
    tool_state: &ToolSessionState,
    edit: &Edit,
    written: &mut HashSet<PathBuf>,
) -> Result<usize, String> {
    let path_arg = edit.path.as_str();
    let resolved =
        resolve_path(tool_state, path_arg, false).map_err(|error| error.message().to_owned())?;

    if !resolved.is_file() {
        return Err(format!(
            "`{path_arg}` does not exist as a file — use fs.write to create it"
        ));
    }

    // Skipped for a path this same call already wrote: the recorded mtime
    // does track that write, but the gate exists to catch *other* writers,
    // and this call's own evolving content must not be able to trip it.
    if !written.contains(&resolved) {
        check_staleness(tool_state, &resolved, path_arg)
            .map_err(|error| error.message().to_owned())?;
    }

    let content = fs::read_to_string(&resolved)
        .map_err(|error| format!("cannot read `{path_arg}` as UTF-8 text: {error}"))?;

    let match_count = content.matches(edit.old_string.as_str()).count();
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
        content.replace(edit.old_string.as_str(), &edit.new_string)
    } else {
        content.replacen(edit.old_string.as_str(), &edit.new_string, 1)
    };
    fs::write(&resolved, &updated)
        .map_err(|error| format!("failed to write `{path_arg}`: {error}"))?;

    if let Ok(mtime) = fs::metadata(&resolved).and_then(|metadata| metadata.modified()) {
        tool_state.record_mtime(resolved.clone(), mtime);
    }
    written.insert(resolved);

    Ok(match_count)
}
