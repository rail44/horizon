//! Display wording over the frame's structural reading: the per-tool call
//! bodies (`tool_call`) and the receipt summary and elapsed-time text
//! (`receipt`). The structural reading itself -- turn/burst grouping,
//! tool-call views, receipt aggregation -- lives in `crate::agent_model`
//! and is re-exported here, so every `turns::X` call site reads one
//! namespace.

mod receipt;
mod tool_call;

pub use receipt::*;
pub use tool_call::*;

pub use crate::agent_model::{
    ToolCallView, aggregate_receipt, build_tool_call_views, group_into_turns, segment_bursts,
    tool_call_source,
};

/// `1 {singular}` / `{count} {plural}`.
pub fn pluralize(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// Caps `lines` to its first `max_lines`, and says how many went.
pub fn cap_lines_head<T>(mut lines: Vec<T>, max_lines: usize) -> (Vec<T>, usize) {
    let omitted = lines.len().saturating_sub(max_lines);
    lines.truncate(max_lines);
    (lines, omitted)
}

/// Caps `lines` to its last `max_lines`, and says how many went.
pub fn cap_lines_tail(mut lines: Vec<String>, max_lines: usize) -> (Vec<String>, usize) {
    let omitted = lines.len().saturating_sub(max_lines);
    let kept = lines.split_off(omitted);
    (kept, omitted)
}

#[cfg(test)]
pub mod test_support {
    use serde_json::Value;

    use super::{DiffLine, DiffLineKind};
    use crate::agent_model::{
        AgentFrameItem, Message, MessageRole, Permission, Thought, ToolCall, ToolCallIdentity,
        ToolCallStatus, ToolContent, ToolKind,
    };

    pub fn user_message(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            id: format!("user:{text}"),
            role: MessageRole::User,
            text: text.to_string(),
        })
    }

    pub fn assistant_delta(text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            id: "assistant".to_string(),
            role: MessageRole::Assistant,
            text: text.to_string(),
        })
    }

    pub fn reasoning_delta(text: &str) -> AgentFrameItem {
        AgentFrameItem::Thought(Thought {
            id: "thought".to_string(),
            text: text.to_string(),
        })
    }

    fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
        value.get(key).and_then(Value::as_str)
    }

    /// A call as an agent describes the tools these tests name: its title,
    /// kind and files, the way Horizon's agent words them.
    fn call(call_id: &str, tool_id: &str, input: Value) -> ToolCall {
        let path = str_field(&input, "path").unwrap_or_default().to_string();
        let edit_paths: Vec<String> = (input.get("edits").and_then(Value::as_array))
            .into_iter()
            .flatten()
            .filter_map(|edit| str_field(edit, "path"))
            .map(str::to_string)
            .collect();
        let (kind, title, locations) = match tool_id {
            "fs_read" => (ToolKind::Read, format!("Read {path}"), vec![path]),
            "fs_write" => (ToolKind::Edit, format!("Write {path}"), vec![path]),
            "fs_edit" => (
                ToolKind::Edit,
                format!("Edit {}", edit_paths.join(", ")),
                edit_paths,
            ),
            "bash" => {
                let command = str_field(&input, "command").unwrap_or_default();
                let head = command.lines().next().unwrap_or_default();
                (ToolKind::Execute, format!("Run {head}"), Vec::new())
            }
            "fs_grep" | "fs_glob" => {
                let pattern = str_field(&input, "pattern").unwrap_or_default();
                (ToolKind::Search, format!("Search {pattern}"), Vec::new())
            }
            _ => (ToolKind::Other, tool_id.to_string(), Vec::new()),
        };
        ToolCall {
            occurrence_id: call_id.to_string(),
            tool_id: tool_id.to_string(),
            title,
            kind,
            locations,
            content: Vec::new(),
            status: ToolCallStatus::Pending,
            input,
            output: None,
        }
    }

    /// What such an agent gives for a finished call of these tools.
    fn content_of(tool_id: &str, input: &Value, output: &Value) -> Vec<ToolContent> {
        let text = |text: String| vec![ToolContent::Text(text)];
        match tool_id {
            "bash" => text(str_field(output, "output").unwrap_or_default().to_string()),
            "fs_read" => text(format!(
                "{} · {} lines",
                str_field(input, "path").unwrap_or_default(),
                output
                    .get("total_lines")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            )),
            "fs_grep" | "fs_glob" => text(format!(
                "{} · {} matches",
                str_field(input, "pattern").unwrap_or_default(),
                output
                    .get("returned_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            )),
            "fs_edit" => {
                let edits = input
                    .get("edits")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut patch = String::new();
                let mut changes = Vec::new();
                for edit in &edits {
                    let path = str_field(edit, "path").unwrap_or_default();
                    let old: Vec<&str> = str_field(edit, "old_string")
                        .unwrap_or_default()
                        .lines()
                        .collect();
                    let new: Vec<&str> = str_field(edit, "new_string")
                        .unwrap_or_default()
                        .lines()
                        .collect();
                    patch.push_str(&format!(
                        "diff --git {path} {path}\n--- {path}\n+++ {path}\n@@ -1,{} +1,{} @@\n",
                        old.len(),
                        new.len()
                    ));
                    old.iter()
                        .for_each(|line| patch.push_str(&format!("-{line}\n")));
                    new.iter()
                        .for_each(|line| patch.push_str(&format!("+{line}\n")));
                    changes.push((path.to_string(), false));
                }
                vec![ToolContent::Diff {
                    changes,
                    patch: Some(patch),
                }]
            }
            "fs_write" => {
                let path = str_field(input, "path").unwrap_or_default();
                let lines: Vec<&str> = str_field(input, "content")
                    .unwrap_or_default()
                    .lines()
                    .collect();
                let mut patch = format!(
                    "diff --git {path} {path}\nnew file mode 100644\n--- /dev/null\n+++ {path}\n@@ -0,0 +1,{} @@\n",
                    lines.len()
                );
                lines
                    .iter()
                    .for_each(|line| patch.push_str(&format!("+{line}\n")));
                vec![ToolContent::Diff {
                    changes: vec![(path.to_string(), true)],
                    patch: Some(patch),
                }]
            }
            _ => Vec::new(),
        }
    }

    /// A requested call that has not started.
    pub fn tool_requested(call_id: &str, tool_id: &str, input: Value) -> AgentFrameItem {
        AgentFrameItem::ToolCall(Box::new(call(call_id, tool_id, input)))
    }

    /// A finished call; an output with `is_error: true` reads as failed and
    /// gives nothing to draw.
    pub fn tool_finished(
        call_id: &str,
        tool_id: &str,
        input: Value,
        output: Value,
    ) -> AgentFrameItem {
        let failed = output.get("is_error").and_then(Value::as_bool) == Some(true);
        let mut call = call(call_id, tool_id, input);
        call.status = if failed {
            ToolCallStatus::Failed
        } else {
            ToolCallStatus::Completed
        };
        if !failed {
            call.content = content_of(tool_id, &call.input, &output);
        }
        call.output = Some(output);
        AgentFrameItem::ToolCall(Box::new(call))
    }

    /// An edit's output, which these tests do not read.
    pub fn edit_result(_path: &str) -> Value {
        serde_json::json!({"applied_count": 1})
    }

    pub fn approval_requested(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::Permission(Permission {
            identity: ToolCallIdentity {
                occurrence_id: call_id.to_string(),
            },
            reason: "writes a file".to_string(),
            decision: None,
            task_session_id: None,
        })
    }

    /// A pending approval one of the session's `task` children asks for.
    pub fn task_approval_requested(task_session_id: &str, occurrence: &str) -> AgentFrameItem {
        AgentFrameItem::Permission(Permission {
            identity: ToolCallIdentity {
                occurrence_id: occurrence.to_string(),
            },
            reason: "reads outside the workspace".to_string(),
            decision: None,
            task_session_id: Some(task_session_id.to_string()),
        })
    }

    pub fn diff_texts(lines: &[DiffLine]) -> Vec<(DiffLineKind, &str)> {
        lines
            .iter()
            .map(|line| (line.kind, line.text.as_str()))
            .collect()
    }
}
