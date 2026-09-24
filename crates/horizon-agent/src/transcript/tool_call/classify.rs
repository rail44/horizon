use serde_json::Value;

use super::super::file_name;
use super::files::{distinct_edit_paths, edit_entries};
use super::util::{command_head, line_diffstat, str_field};
use super::view::ToolCallKind;

/// Display data plus an explicit recognition result. Consumers must not infer
/// whether a tool is known from its human-facing verb or summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallClassification {
    pub known: bool,
    pub verb: String,
    pub target: Option<String>,
    pub summary: Option<String>,
    pub kind: ToolCallKind,
}

/// Maps a tool id to its display verb, target, (would-be) result
/// summary, and any tool-specific structured data -- the one place that
/// knows the exact input/output JSON shape each tool in
/// `crate::tools` uses (see that module's `tools/fs`, `tools/bash`
/// submodules). Unknown tool ids fall back to the raw id as the verb with
/// no target/summary, so a future tool renders *something* sane rather
/// than nothing.
///
/// Public (not just crate-internal) because `src/agent/turns`'s own
/// `terse_summary` -- a wording function that stayed behind in the
/// `horizon` binary crate -- reuses this classifier's verb/target/summary
/// for every tool id it doesn't special-case itself (see `transcript`'s
/// module doc for why this one didn't cleanly split).
pub fn classify(tool_id: &str, input: &Value, output: Option<&Value>) -> ToolCallClassification {
    let classified = classify_tool(tool_id, input, output);
    let known = classified.is_some();
    let (verb, target, summary, kind) =
        classified.unwrap_or_else(|| (tool_id.to_string(), None, None, ToolCallKind::Generic));
    ToolCallClassification {
        known,
        verb,
        target,
        summary,
        kind,
    }
}

fn classify_tool(
    tool_id: &str,
    input: &Value,
    output: Option<&Value>,
) -> Option<(String, Option<String>, Option<String>, ToolCallKind)> {
    Some(match tool_id {
        "fs.edit" => {
            let edits = edit_entries(input);
            let paths = distinct_edit_paths(&edits);
            let diffstat = edits.iter().fold((0, 0), |(added, removed), edit| {
                let (edit_added, edit_removed) = line_diffstat(edit.old_string, edit.new_string);
                (added + edit_added, removed + edit_removed)
            });
            // One edit still reads as the file it touches; a batch reads as
            // its own cardinality, since no single path represents it.
            let target = match (edits.len(), paths.len()) {
                (0, _) => None,
                (1, _) => Some(edits[0].path.to_string()),
                (edit_count, file_count) => Some(format!(
                    "{edit_count} edits in {file_count} {}",
                    if file_count == 1 { "file" } else { "files" }
                )),
            };
            let kind = match paths.as_slice() {
                [path] => ToolCallKind::File {
                    file_name: file_name(path),
                    diffstat: Some(diffstat),
                },
                _ => ToolCallKind::Generic,
            };
            let (added, removed) = diffstat;
            (
                "Edit".to_string(),
                target,
                Some(format!("+{added} -{removed}")),
                kind,
            )
        }
        "fs.write" => {
            let path = str_field(input, "path").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("created"))
                .and_then(Value::as_bool)
                .map(|created| {
                    if created {
                        "created".to_string()
                    } else {
                        "overwritten".to_string()
                    }
                });
            (
                "Write".to_string(),
                Some(path.clone()),
                summary,
                ToolCallKind::File {
                    file_name: file_name(&path),
                    diffstat: None,
                },
            )
        }
        "bash" => {
            let command = str_field(input, "command").unwrap_or_default();
            let head = command_head(command);
            let summary = output
                .and_then(|output| output.get("exit_code"))
                .and_then(Value::as_i64)
                .map(|code| format!("exit {code}"));
            (
                "Bash".to_string(),
                Some(head.clone()),
                summary,
                ToolCallKind::Bash { command_head: head },
            )
        }
        "fs.read" => {
            let path = str_field(input, "path").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("total_lines"))
                .and_then(Value::as_u64)
                .map(|lines| format!("{lines} lines"));
            (
                "Read".to_string(),
                Some(path),
                summary,
                ToolCallKind::Generic,
            )
        }
        "fs.grep" | "fs.glob" => {
            let pattern = str_field(input, "pattern").unwrap_or_default().to_string();
            let summary = output
                .and_then(|output| output.get("returned_count"))
                .and_then(Value::as_u64)
                .map(|count| format!("{count} matches"));
            let verb = if tool_id == "fs.grep" { "Grep" } else { "Glob" };
            (
                verb.to_string(),
                Some(pattern),
                summary,
                ToolCallKind::Generic,
            )
        }
        "workspace.snapshot" => ("Snapshot".to_string(), None, None, ToolCallKind::Generic),
        "config.read" => ("Config Read".to_string(), None, None, ToolCallKind::Generic),
        "config.write" => (
            "Config Write".to_string(),
            None,
            None,
            ToolCallKind::Generic,
        ),
        "recall.search" => (
            "Recall Search".to_string(),
            None,
            None,
            ToolCallKind::Generic,
        ),
        "recall.read" => ("Recall Read".to_string(), None, None, ToolCallKind::Generic),
        "skill.read" => {
            let id = str_field(input, "id").unwrap_or_default().to_string();
            ("Skill".to_string(), Some(id), None, ToolCallKind::Generic)
        }
        // `task`'s `description` input is a short label the model writes
        // for exactly this row (`tools::catalog`) -- it is the only place a
        // delegated task announces what it is doing while it runs, since
        // the session it spawns is deliberately kept out of the
        // client-visible session list (`roles::is_exploration`).
        //
        // Since the 2026-07-28 asynchronous cutover the call itself only
        // *launches* the task, so the honest summary is the launch
        // receipt's own `status` ("started"). The completed report is not
        // this call's result at all -- it arrives later as a
        // `MessageRole::TaskNotification` message in the transcript, and
        // `task_output`'s own row (below) reports "running" vs "finished"
        // for a task looked up afterwards.
        "task" => {
            let description = str_field(input, "description")
                .unwrap_or_default()
                .to_string();
            let summary = output.and_then(|output| str_field(output, "status").map(str::to_string));
            (
                "Task".to_string(),
                Some(description),
                summary,
                ToolCallKind::Generic,
            )
        }
        "task_output" => {
            // The label the launch recorded, echoed back by the fetch so
            // this row reads like the launch row rather than a bare uuid.
            let target = output
                .and_then(|output| str_field(output, "description"))
                .or_else(|| str_field(input, "session_id"))
                .unwrap_or_default()
                .to_string();
            let summary = output.and_then(|output| str_field(output, "status").map(str::to_string));
            (
                "Task Output".to_string(),
                Some(target),
                summary,
                ToolCallKind::Generic,
            )
        }
        _ => return None,
    })
}
