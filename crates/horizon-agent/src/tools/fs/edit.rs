//! `fs_edit`: a list of string replacements applied in one call, in the
//! form pi and eve converge on
//! (`docs/research/agent-frameworks-core-and-extension-2026-11-23.md`
//! section 4).
//!
//! The input is a list and only a list — there is no single-edit top-level
//! shape — so batching several hunks of one file, or edits across several
//! files, is the ordinary path rather than a capability the model has to
//! discover (`docs/research/agent-editing-phase-analysis-2026-07-28.md`).
//!
//! The contract every caller depends on:
//!
//! * Every edit is placed against the file **as it was** before the call:
//!   an edit never sees another's result. Two edits of one file must not
//!   overlap.
//! * **All or nothing**: every edit is placed before any file is written.
//!   If one cannot be placed, nothing is written, and the result names
//!   every edit that could not be; the rest are `not_attempted`.
//! * Per edit: the file must have been read this session and be unchanged
//!   on disk since, and `old_string` must match exactly once unless
//!   `replace_all` is set.
//! * When `old_string` matches nowhere exactly, it is looked for once more
//!   with both sides normalized ([`normalize_line`]: compatibility forms,
//!   trailing whitespace, typographic quotes and dashes, unusual spaces).
//!   A single match there replaces only the lines it covers, written in
//!   their normalized form; every other line keeps its bytes. The receipt
//!   says so (`normalized`). `replace_all` matches exactly only.
//! * A file's line endings (CRLF or LF) and byte-order mark are kept.
//! * A malformed `edits` list is rejected before any file is touched.

use std::{collections::HashMap, fs, ops::Range, path::PathBuf};

use unicode_normalization::UnicodeNormalization;

use crate::tools::output::*;

use super::locks::FileLocks;
use super::safety::resolve_path;
use super::staleness::check_staleness;
use crate::tools::state::ToolSessionState;

use crate::tools::input::{Edit, EditFiles};

/// One file as the call found it, with its edits placed.
struct Target {
    resolved: PathBuf,
    path_arg: String,
    /// The content with `\n` line endings and no byte-order mark: what the
    /// edits are placed against.
    content: String,
    crlf: bool,
    bom: bool,
    /// Each placed edit: its index in the request and the spans it replaces.
    placed: Vec<(usize, Vec<Replacement>)>,
}

struct Replacement {
    span: Range<usize>,
    text: String,
}

/// How one edit was placed.
struct Placement {
    replacements: Vec<Replacement>,
    normalized: bool,
}

pub(in crate::tools) fn execute(tool_state: &ToolSessionState, input: &EditFiles) -> Response {
    let edits = &input.edits;

    // Every resolvable target is locked for the whole call, in the lexical
    // order `FileLocks` imposes, so a concurrent mutation can't slip
    // between placing the edits and writing them.
    let lock_paths = edits
        .iter()
        .filter_map(|edit| resolve_path(tool_state, &edit.path, false).ok())
        .collect::<Vec<_>>();
    let locks = FileLocks::acquire(lock_paths);
    let _guards = locks.hold();

    let mut outcomes: Vec<Option<EditOutcome>> = vec![None; edits.len()];
    let mut targets: Vec<Target> = Vec::new();
    let mut by_path: HashMap<PathBuf, usize> = HashMap::new();
    for (index, edit) in edits.iter().enumerate() {
        let target = match target_for(tool_state, edit, &mut targets, &mut by_path) {
            Ok(target) => target,
            Err(message) => {
                outcomes[index] = Some(EditOutcome::Failed { message });
                continue;
            }
        };
        match place(&target.content, edit) {
            Ok(placement) => {
                let occurrences = placement.replacements.len();
                outcomes[index] = Some(EditOutcome::Applied {
                    occurrences,
                    normalized: placement.normalized,
                });
                target.placed.push((index, placement.replacements));
            }
            Err(message) => outcomes[index] = Some(EditOutcome::Failed { message }),
        }
    }
    for target in &targets {
        if let Some((first, second)) = overlapping(&target.placed) {
            outcomes[second] = Some(EditOutcome::Failed {
                message: format!(
                    "overlaps the edit at index {first} in `{}` — every edit is placed against \
                     the file as it was, so merge the two into one",
                    target.path_arg
                ),
            });
        }
    }

    let failed: Vec<usize> = (outcomes.iter().enumerate())
        .filter(|(_, outcome)| matches!(outcome, Some(EditOutcome::Failed { .. })))
        .map(|(index, _)| index)
        .collect();
    if !failed.is_empty() {
        let reasons: Vec<String> = failed
            .iter()
            .filter_map(|&index| match &outcomes[index] {
                Some(EditOutcome::Failed { message }) => {
                    Some(format!("edit at index {index} failed: {message}"))
                }
                _ => None,
            })
            .collect();
        let message = format!(
            "{}. No file was written — fix the failed edit(s) and resend the whole list.",
            reasons.join("; ")
        );
        return refused(edits, outcomes, failed[0], message);
    }

    let mut file_count = 0;
    let mut versions: Vec<Option<String>> = vec![None; edits.len()];
    for target in &targets {
        let version = match write(tool_state, target) {
            Ok(version) => version,
            Err(message) => {
                // Only a write itself failing gets here, after every edit was
                // placed: the files written before it stay written.
                for (index, _) in &target.placed {
                    outcomes[*index] = Some(EditOutcome::Failed {
                        message: message.clone(),
                    });
                }
                let message = format!(
                    "{message}; {file_count} file(s) before it were written and the rest were \
                 not — re-read them before editing again"
                );
                return refused(edits, outcomes, target.placed[0].0, message);
            }
        };
        for (index, _) in &target.placed {
            versions[*index] = version.clone();
        }
        file_count += 1;
    }
    let mut receipts = receipts(edits, outcomes);
    for (receipt, version) in receipts.iter_mut().zip(versions) {
        receipt.content_version = version;
    }
    // What the call changed, against each file as it was.
    let patch: String = (targets.iter())
        .filter_map(|target| {
            let replacements: Vec<(Range<usize>, &str)> = (target.placed.iter())
                .flat_map(|(_, replacements)| replacements)
                .map(|replacement| (replacement.span.clone(), replacement.text.as_str()))
                .collect();
            super::patch::edited(&target.resolved, &target.content, &replacements)
        })
        .collect();
    Response::succeeded(FileEdits {
        applied_count: receipts.len(),
        edits: receipts,
        file_count,
        failed_index: None,
        message: None,
    })
    .with_patch((!patch.is_empty()).then_some(patch))
}

/// The target `edit` names, read and checked the first time the call meets
/// its path.
fn target_for<'a>(
    tool_state: &ToolSessionState,
    edit: &Edit,
    targets: &'a mut Vec<Target>,
    by_path: &mut HashMap<PathBuf, usize>,
) -> Result<&'a mut Target, String> {
    let path_arg = edit.path.as_str();
    let resolved =
        resolve_path(tool_state, path_arg, false).map_err(|error| error.message().to_owned())?;
    if let Some(&at) = by_path.get(&resolved) {
        return Ok(&mut targets[at]);
    }
    if !resolved.is_file() {
        return Err(format!(
            "`{path_arg}` does not exist as a file — use fs_write to create it"
        ));
    }
    check_staleness(tool_state, &resolved, path_arg).map_err(|error| error.message().to_owned())?;
    let raw = fs::read_to_string(&resolved)
        .map_err(|error| format!("cannot read `{path_arg}` as UTF-8 text: {error}"))?;
    let (bom, raw) = match raw.strip_prefix('\u{feff}') {
        Some(rest) => (true, rest.to_string()),
        None => (false, raw),
    };
    let crlf = raw.contains("\r\n");
    let content = if crlf { raw.replace("\r\n", "\n") } else { raw };
    by_path.insert(resolved.clone(), targets.len());
    targets.push(Target {
        resolved,
        path_arg: path_arg.to_string(),
        content,
        crlf,
        bom,
        placed: Vec::new(),
    });
    Ok(targets.last_mut().expect("just pushed"))
}

/// Where `edit` goes in `content`: its exact matches, or else its one
/// normalized match.
fn place(content: &str, edit: &Edit) -> Result<Placement, String> {
    let path = &edit.path;
    let old = edit.old_string.replace("\r\n", "\n");
    let new = edit.new_string.replace("\r\n", "\n");
    let exact: Vec<usize> = content
        .match_indices(old.as_str())
        .map(|(at, _)| at)
        .collect();
    if exact.len() > 1 && !edit.replace_all {
        return Err(format!(
            "found {} matches for `old_string` in `{path}` — include more surrounding context \
             to make it unique, or set `replace_all: true` to replace every occurrence",
            exact.len()
        ));
    }
    if !exact.is_empty() {
        return Ok(Placement {
            replacements: exact
                .into_iter()
                .map(|at| Replacement {
                    span: at..at + old.len(),
                    text: new.clone(),
                })
                .collect(),
            normalized: false,
        });
    }
    let not_found =
        || format!("`old_string` not found in `{path}` — check the exact text and try again");
    if edit.replace_all {
        return Err(not_found());
    }
    match normalized_match(content, &old, &new) {
        NormalizedMatch::One(replacement) => Ok(Placement {
            replacements: vec![replacement],
            normalized: true,
        }),
        NormalizedMatch::None => Err(not_found()),
        NormalizedMatch::Many(count) => Err(format!(
            "`old_string` matches nowhere exactly in `{path}`, and {count} places once \
             whitespace and punctuation are normalized — include more surrounding context"
        )),
    }
}

enum NormalizedMatch {
    One(Replacement),
    None,
    Many(usize),
}

/// The one place `old` matches `content` once both are normalized line by
/// line: the lines it covers, as a span of `content`, replaced by those
/// lines normalized with `new` in place of the match.
fn normalized_match(content: &str, old: &str, new: &str) -> NormalizedMatch {
    let lines: Vec<&str> = content.split('\n').collect();
    let normalized_lines: Vec<String> = lines.iter().map(|line| normalize_line(line)).collect();
    let normalized = normalized_lines.join("\n");
    let needle = old
        .split('\n')
        .map(normalize_line)
        .collect::<Vec<_>>()
        .join("\n");
    if needle.trim().is_empty() {
        return NormalizedMatch::None;
    }
    let found: Vec<usize> = normalized
        .match_indices(needle.as_str())
        .map(|(at, _)| at)
        .collect();
    let at = match found.as_slice() {
        [] => return NormalizedMatch::None,
        [at] => *at,
        many => return NormalizedMatch::Many(many.len()),
    };
    // The lines the match covers, in the normalized and the original text.
    let first_line = normalized[..at].matches('\n').count();
    let last_line = first_line + needle.matches('\n').count();
    let normalized_start = offset_after(&normalized_lines[..first_line]);
    let normalized_end = offset_after(&normalized_lines[..=last_line]) - 1;
    let mut block = normalized[normalized_start..normalized_end].to_string();
    let within = at - normalized_start;
    block.replace_range(within..within + needle.len(), new);
    let original_start = offset_after(&lines[..first_line]);
    let original_end = offset_after(&lines[..=last_line]) - 1;
    NormalizedMatch::One(Replacement {
        span: original_start..original_end,
        text: block,
    })
}

/// Where the text after `lines` (each followed by `\n`) starts.
fn offset_after(lines: &[impl AsRef<str>]) -> usize {
    lines.iter().map(|line| line.as_ref().len() + 1).sum()
}

/// One line as the normalized match compares it: compatibility forms
/// folded (NFKC), unusual spaces as a plain space, typographic quotes and
/// dashes as their ASCII forms, and trailing whitespace removed.
fn normalize_line(line: &str) -> String {
    let folded: String = line
        .nfkc()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{00a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect();
    folded.trim_end().to_string()
}

/// Two placed edits of one file whose spans overlap: the earlier and the
/// later index.
fn overlapping(placed: &[(usize, Vec<Replacement>)]) -> Option<(usize, usize)> {
    let mut spans: Vec<(usize, &Range<usize>)> = placed
        .iter()
        .flat_map(|(index, replacements)| {
            replacements
                .iter()
                .map(move |replacement| (*index, &replacement.span))
        })
        .collect();
    spans.sort_by_key(|(_, span)| span.start);
    spans.windows(2).find_map(|pair| {
        let ((first, earlier), (second, later)) = (pair[0], pair[1]);
        (later.start < earlier.end && first != second)
            .then(|| (first.min(second), first.max(second)))
    })
}

/// Writes `target` with its edits, from the last span back so earlier
/// offsets stay valid, in its own line endings and byte-order mark.
fn write(tool_state: &ToolSessionState, target: &Target) -> Result<Option<String>, String> {
    let mut replacements: Vec<&Replacement> = target
        .placed
        .iter()
        .flat_map(|(_, replacements)| replacements)
        .collect();
    replacements.sort_by_key(|replacement| std::cmp::Reverse(replacement.span.start));
    let mut content = target.content.clone();
    for replacement in replacements {
        content.replace_range(replacement.span.clone(), &replacement.text);
    }
    if target.crlf {
        content = content.replace('\n', "\r\n");
    }
    if target.bom {
        content.insert(0, '\u{feff}');
    }
    fs::write(&target.resolved, &content)
        .map_err(|error| format!("failed to write `{}`: {error}", target.path_arg))?;
    let metadata = fs::metadata(&target.resolved).ok();
    if let Some(mtime) = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
    {
        tool_state.record_mtime(target.resolved.clone(), mtime);
    }
    Ok(metadata.as_ref().and_then(super::content_version))
}

fn receipts(edits: &[Edit], outcomes: Vec<Option<EditOutcome>>) -> Vec<EditReceipt> {
    (edits.iter().zip(outcomes).enumerate())
        .map(|(index, (edit, outcome))| EditReceipt {
            index,
            path: edit.path.clone(),
            outcome: outcome.unwrap_or(EditOutcome::NotAttempted),
            content_version: None,
        })
        .collect()
}

/// The result of a call that did not apply its edits: those with a
/// failed outcome failed, the others were not attempted.
fn refused(
    edits: &[Edit],
    outcomes: Vec<Option<EditOutcome>>,
    failed_index: usize,
    message: String,
) -> Response {
    let outcomes = outcomes
        .into_iter()
        .map(|outcome| outcome.filter(|outcome| matches!(outcome, EditOutcome::Failed { .. })))
        .collect();
    Response::failed(FileEdits {
        edits: receipts(edits, outcomes),
        applied_count: 0,
        file_count: 0,
        failed_index: Some(failed_index),
        message: Some(message),
    })
}
