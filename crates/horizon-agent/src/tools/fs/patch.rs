use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::error_output;
use super::locks::FileLocks;
use super::safety::resolve_path;
use super::staleness::check_staleness;
use crate::tools::state::ToolSessionState;

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const ADD: &str = "*** Add File:";
const DELETE: &str = "*** Delete File:";
const UPDATE: &str = "*** Update File:";
const MOVE_TO: &str = "*** Move to:";
const END_OF_FILE: &str = "*** End of File";
const MAX_CHANGES: usize = 100;
const MAX_PATCH_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Operation {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<Chunk>,
    },
}

impl Operation {
    fn source_path(&self) -> &str {
        match self {
            Self::Add { path, .. } | Self::Delete { path } | Self::Update { path, .. } => path,
        }
    }

    fn move_path(&self) -> Option<&str> {
        match self {
            Self::Update { move_to, .. } => move_to.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Chunk {
    context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    added: u64,
    removed: u64,
    end_of_file: bool,
}

#[derive(Debug)]
enum PlannedKind {
    Add,
    Update,
    Delete,
    Move,
}

#[derive(Debug)]
struct PlannedChange {
    source_arg: String,
    source: PathBuf,
    target_arg: String,
    target: PathBuf,
    kind: PlannedKind,
    new_content: Option<String>,
    added: u64,
    removed: u64,
}

pub(super) fn execute(tool_state: &ToolSessionState, input: &Value) -> Value {
    let Some(patch_text) = input.get("patch").and_then(Value::as_str) else {
        return error_output("fs.patch requires a `patch` string argument");
    };
    if patch_text.len() > MAX_PATCH_BYTES {
        return error_output(format!(
            "fs.patch input exceeds the {MAX_PATCH_BYTES}-byte limit"
        ));
    }
    let operations = match parse_patch(patch_text) {
        Ok(operations) => operations,
        Err(error) => return error_output(format!("invalid fs.patch input: {error}")),
    };

    // Resolve every path before taking locks. `resolve_path` canonicalizes
    // the nearest existing ancestor, so two spellings of the same target
    // converge on the same process-wide lock key.
    let mut resolved = Vec::new();
    for operation in &operations {
        match resolve_path(tool_state, operation.source_path()) {
            Ok(path) => resolved.push(path),
            Err(error) => return error,
        }
        if let Some(move_to) = operation.move_path() {
            match resolve_path(tool_state, move_to) {
                Ok(path) => resolved.push(path),
                Err(error) => return error,
            }
        }
    }
    let locks = FileLocks::acquire(resolved);
    let _guards = locks.hold();

    let plan = match build_plan(tool_state, operations) {
        Ok(plan) => plan,
        Err(error) => return error,
    };
    if let Err(error) = apply_plan(&plan) {
        return error_output(error);
    }

    for change in &plan {
        if matches!(change.kind, PlannedKind::Delete | PlannedKind::Move) {
            tool_state.forget_mtime(&change.source);
        }
        if !matches!(change.kind, PlannedKind::Delete) {
            if let Ok(mtime) = fs::metadata(&change.target).and_then(|metadata| metadata.modified())
            {
                tool_state.record_mtime(change.target.clone(), mtime);
            }
        }
    }

    let files = plan
        .iter()
        .map(|change| {
            json!({
                "path": change.source_arg,
                "target_path": change.target_arg,
                "operation": match change.kind {
                    PlannedKind::Add => "added",
                    PlannedKind::Update => "updated",
                    PlannedKind::Delete => "deleted",
                    PlannedKind::Move => "moved",
                },
                "added": change.added,
                "removed": change.removed,
            })
        })
        .collect::<Vec<_>>();
    let added = plan.iter().map(|change| change.added).sum::<u64>();
    let removed = plan.iter().map(|change| change.removed).sum::<u64>();
    json!({
        "applied": true,
        "file_count": files.len(),
        "added": added,
        "removed": removed,
        "files": files,
    })
}

fn parse_patch(input: &str) -> Result<Vec<Operation>, String> {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized.lines().collect::<Vec<_>>();
    if lines.first().copied() != Some(BEGIN) || lines.last().copied() != Some(END) {
        return Err(format!(
            "patch must start with `{BEGIN}` and end with `{END}`"
        ));
    }

    let mut operations = Vec::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        if operations.len() >= MAX_CHANGES {
            return Err(format!(
                "patch contains more than {MAX_CHANGES} file changes"
            ));
        }
        let line = lines[index];
        if let Some(path) = header_path(line, ADD) {
            index += 1;
            let mut content = Vec::new();
            while index + 1 < lines.len() && !is_file_header(lines[index]) {
                let Some(line) = lines[index].strip_prefix('+') else {
                    return Err(format!(
                        "added file `{path}` contains a line without a `+` prefix"
                    ));
                };
                content.push(line);
                index += 1;
            }
            let mut content = content.join("\n");
            if !content.is_empty() {
                content.push('\n');
            }
            operations.push(Operation::Add {
                path: path.to_string(),
                content,
            });
            continue;
        }
        if let Some(path) = header_path(line, DELETE) {
            operations.push(Operation::Delete {
                path: path.to_string(),
            });
            index += 1;
            continue;
        }
        if let Some(path) = header_path(line, UPDATE) {
            index += 1;
            let move_to = if index + 1 < lines.len() {
                header_path(lines[index], MOVE_TO).map(|path| {
                    index += 1;
                    path.to_string()
                })
            } else {
                None
            };
            let mut chunks = Vec::new();
            while index + 1 < lines.len() && !is_file_header(lines[index]) {
                let Some(header) = lines[index].strip_prefix("@@") else {
                    return Err(format!("update for `{path}` expected an `@@` chunk header"));
                };
                let context = (!header.trim().is_empty()).then(|| header.trim().to_string());
                index += 1;
                let mut old_lines = Vec::new();
                let mut new_lines = Vec::new();
                let mut changed = false;
                let mut added = 0;
                let mut removed = 0;
                let mut end_of_file = false;
                while index + 1 < lines.len()
                    && !lines[index].starts_with("@@")
                    && !is_file_header(lines[index])
                {
                    let line = lines[index];
                    if line == END_OF_FILE {
                        end_of_file = true;
                        index += 1;
                        break;
                    }
                    let Some((prefix, content)) = line.split_at_checked(1) else {
                        return Err(format!(
                            "update for `{path}` contains an empty unprefixed line"
                        ));
                    };
                    match prefix {
                        " " => {
                            old_lines.push(content.to_string());
                            new_lines.push(content.to_string());
                        }
                        "-" => {
                            old_lines.push(content.to_string());
                            removed += 1;
                            changed = true;
                        }
                        "+" => {
                            new_lines.push(content.to_string());
                            added += 1;
                            changed = true;
                        }
                        _ => {
                            return Err(format!(
                                "update for `{path}` contains a line without a space, `+`, or `-` prefix"
                            ));
                        }
                    }
                    index += 1;
                }
                if !changed {
                    return Err(format!("update chunk for `{path}` contains no changes"));
                }
                chunks.push(Chunk {
                    context,
                    old_lines,
                    new_lines,
                    added,
                    removed,
                    end_of_file,
                });
            }
            if chunks.is_empty() && move_to.is_none() {
                return Err(format!("update for `{path}` contains no chunks"));
            }
            operations.push(Operation::Update {
                path: path.to_string(),
                move_to,
                chunks,
            });
            continue;
        }
        return Err(format!("unexpected patch line `{line}`"));
    }
    if operations.is_empty() {
        return Err("patch contains no file changes".to_string());
    }
    Ok(operations)
}

fn header_path<'a>(line: &'a str, header: &str) -> Option<&'a str> {
    let path = line.strip_prefix(header)?.trim();
    (!path.is_empty()).then_some(path)
}

fn is_file_header(line: &str) -> bool {
    line == END || line.starts_with(ADD) || line.starts_with(DELETE) || line.starts_with(UPDATE)
}

fn build_plan(
    tool_state: &ToolSessionState,
    operations: Vec<Operation>,
) -> Result<Vec<PlannedChange>, Value> {
    let mut occupied = HashSet::new();
    let mut plan = Vec::with_capacity(operations.len());
    for operation in operations {
        let source_arg = operation.source_path().to_string();
        let source = resolve_path(tool_state, &source_arg)?;
        if !occupied.insert(source.clone()) {
            return Err(error_output(format!(
                "patch touches `{source_arg}` more than once — combine its chunks under one file header"
            )));
        }
        match operation {
            Operation::Add { content, .. } => {
                if source.exists() {
                    return Err(error_output(format!(
                        "cannot add `{source_arg}` because it already exists"
                    )));
                }
                let added = line_count(&content);
                plan.push(PlannedChange {
                    source_arg: source_arg.clone(),
                    source: source.clone(),
                    target_arg: source_arg,
                    target: source,
                    kind: PlannedKind::Add,
                    new_content: Some(content),
                    added,
                    removed: 0,
                });
            }
            Operation::Delete { .. } => {
                let old_content = read_existing(tool_state, &source, &source_arg)?;
                let removed = line_count(&old_content);
                plan.push(PlannedChange {
                    source_arg: source_arg.clone(),
                    source: source.clone(),
                    target_arg: source_arg,
                    target: source,
                    kind: PlannedKind::Delete,
                    new_content: None,
                    added: 0,
                    removed,
                });
            }
            Operation::Update {
                move_to, chunks, ..
            } => {
                let old_content = read_existing(tool_state, &source, &source_arg)?;
                let new_content = apply_chunks(&source_arg, &old_content, &chunks)?;
                let (target_arg, target, kind) = if let Some(move_to) = move_to {
                    let target = resolve_path(tool_state, &move_to)?;
                    if target == source {
                        return Err(error_output(format!(
                            "move target for `{source_arg}` is unchanged"
                        )));
                    }
                    if target.exists() {
                        return Err(error_output(format!(
                            "cannot move `{source_arg}` to `{move_to}` because the target exists"
                        )));
                    }
                    if !occupied.insert(target.clone()) {
                        return Err(error_output(format!(
                            "patch touches `{move_to}` more than once"
                        )));
                    }
                    (move_to, target, PlannedKind::Move)
                } else {
                    (source_arg.clone(), source.clone(), PlannedKind::Update)
                };
                let (added, removed) = changed_line_counts(&chunks);
                plan.push(PlannedChange {
                    source_arg,
                    source,
                    target_arg,
                    target,
                    kind,
                    new_content: Some(new_content),
                    added,
                    removed,
                });
            }
        }
    }
    Ok(plan)
}

fn read_existing(
    tool_state: &ToolSessionState,
    path: &Path,
    display_path: &str,
) -> Result<String, Value> {
    if !path.is_file() {
        return Err(error_output(format!(
            "`{display_path}` does not exist as a file"
        )));
    }
    check_staleness(tool_state, path, display_path)?;
    fs::read_to_string(path).map_err(|error| {
        error_output(format!(
            "cannot read `{display_path}` as UTF-8 text: {error}"
        ))
    })
}

fn apply_chunks(path: &str, content: &str, chunks: &[Chunk]) -> Result<String, Value> {
    if chunks.is_empty() {
        return Ok(content.to_string());
    }
    let trailing_newline = content.ends_with('\n');
    let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    let mut replacements = Vec::new();
    let mut cursor = 0;
    for chunk in chunks {
        if let Some(context) = &chunk.context {
            let matches = matching_offsets(&lines, std::slice::from_ref(context), cursor);
            if matches.len() != 1 {
                return Err(error_output(format!(
                    "patch context `{context}` in `{path}` matched {} times after line {}",
                    matches.len(),
                    cursor + 1
                )));
            }
            cursor = matches[0] + 1;
        }
        if chunk.old_lines.is_empty() {
            let at = if chunk.end_of_file || chunk.context.is_none() {
                lines.len()
            } else {
                cursor
            };
            replacements.push((at, 0, chunk.new_lines.clone()));
            cursor = at;
            continue;
        }
        let matches = matching_offsets(&lines, &chunk.old_lines, cursor);
        let at = if chunk.end_of_file {
            let expected = lines.len().saturating_sub(chunk.old_lines.len());
            matches
                .into_iter()
                .filter(|at| *at == expected)
                .collect::<Vec<_>>()
        } else {
            matches
        };
        if at.len() != 1 {
            return Err(error_output(format!(
                "expected lines in `{path}` matched {} times after line {} — add more context",
                at.len(),
                cursor + 1
            )));
        }
        let at = at[0];
        replacements.push((at, chunk.old_lines.len(), chunk.new_lines.clone()));
        cursor = at + chunk.old_lines.len();
    }
    for (at, old_len, replacement) in replacements.into_iter().rev() {
        lines.splice(at..at + old_len, replacement);
    }
    let mut updated = lines.join("\n");
    if trailing_newline {
        updated.push('\n');
    }
    Ok(updated)
}

fn matching_offsets(lines: &[String], pattern: &[String], start: usize) -> Vec<usize> {
    if pattern.is_empty() || pattern.len() > lines.len() {
        return Vec::new();
    }
    (start..=lines.len() - pattern.len())
        .filter(|at| lines[*at..*at + pattern.len()] == *pattern)
        .collect()
}

fn apply_plan(plan: &[PlannedChange]) -> Result<(), String> {
    for change in plan {
        if matches!(change.kind, PlannedKind::Add | PlannedKind::Move) {
            if let Some(parent) = change.target.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "failed to create parent directories for `{}`: {error}",
                        change.target_arg
                    )
                })?;
            }
        }
    }

    // OpenCode similarly validates the entire change set before this point.
    // Writes are then ordered so new content exists before destructive
    // removals; a filesystem cannot provide a portable multi-file atomic
    // transaction, so an I/O failure can still leave a partial change set.
    for change in plan {
        match change.kind {
            PlannedKind::Add | PlannedKind::Update => fs::write(
                &change.target,
                change.new_content.as_deref().unwrap_or_default(),
            )
            .map_err(|error| format!("failed to write `{}`: {error}", change.target_arg))?,
            PlannedKind::Move => {
                fs::write(
                    &change.target,
                    change.new_content.as_deref().unwrap_or_default(),
                )
                .map_err(|error| format!("failed to write `{}`: {error}", change.target_arg))?;
            }
            PlannedKind::Delete => {}
        }
    }
    for change in plan {
        if matches!(change.kind, PlannedKind::Delete | PlannedKind::Move) {
            fs::remove_file(&change.source)
                .map_err(|error| format!("failed to remove `{}`: {error}", change.source_arg))?;
        }
    }
    Ok(())
}

fn changed_line_counts(chunks: &[Chunk]) -> (u64, u64) {
    let added = chunks.iter().map(|chunk| chunk.added).sum();
    let removed = chunks.iter().map(|chunk| chunk.removed).sum();
    (added, removed)
}

fn line_count(content: &str) -> u64 {
    content.lines().count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_file_operations() {
        let operations = parse_patch(
            "*** Begin Patch\n*** Add File: /tmp/new\n+hello\n*** Update File: /tmp/old\n@@\n-before\n+after\n*** Delete File: /tmp/gone\n*** End Patch",
        )
        .unwrap();
        assert_eq!(operations.len(), 3);
    }

    #[test]
    fn applies_several_chunks_to_one_snapshot() {
        let chunks = vec![
            Chunk {
                context: None,
                old_lines: vec!["one".into()],
                new_lines: vec!["ONE".into()],
                added: 1,
                removed: 1,
                end_of_file: false,
            },
            Chunk {
                context: None,
                old_lines: vec!["three".into()],
                new_lines: vec!["THREE".into()],
                added: 1,
                removed: 1,
                end_of_file: false,
            },
        ];
        assert_eq!(
            apply_chunks("/tmp/file", "one\ntwo\nthree\n", &chunks).unwrap(),
            "ONE\ntwo\nTHREE\n"
        );
    }

    #[test]
    fn rejects_ambiguous_chunks() {
        let chunk = Chunk {
            context: None,
            old_lines: vec!["same".into()],
            new_lines: vec!["changed".into()],
            added: 1,
            removed: 1,
            end_of_file: false,
        };
        assert!(apply_chunks("/tmp/file", "same\nsame\n", &[chunk]).is_err());
    }
}
