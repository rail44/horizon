use std::fs;

use serde_json::{json, Value};

use super::error_output;
use super::safety::resolve_path;
use crate::agent::tools::state::ToolSessionState;

/// Per-line character cap, independent of `limit`, so one absurdly long
/// line can't blow out the tool result.
const MAX_LINE_LEN: usize = 2000;

pub(super) fn execute(tool_state: &ToolSessionState, input: &Value) -> Value {
    let Some(path_arg) = input.get("path").and_then(Value::as_str) else {
        return error_output("fs.read requires a `path` string argument");
    };

    let resolved = match resolve_path(tool_state, path_arg) {
        Ok(path) => path,
        Err(error) => return error,
    };

    if !resolved.exists() {
        return error_output(format!("`{path_arg}` does not exist"));
    }

    let metadata = match fs::metadata(&resolved) {
        Ok(metadata) => metadata,
        Err(error) => return error_output(format!("cannot read `{path_arg}`: {error}")),
    };
    if metadata.is_dir() {
        return error_output(format!("`{path_arg}` is a directory, not a file"));
    }

    let content = match fs::read_to_string(&resolved) {
        Ok(content) => content,
        Err(error) => {
            return error_output(format!("cannot read `{path_arg}` as UTF-8 text: {error}"))
        }
    };

    let offset = input
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| limit as usize)
        .unwrap_or(tool_state.tools_config().fs.read_line_cap)
        .max(1);

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let start_index = offset.saturating_sub(1).min(total_lines);
    let end_index = start_index.saturating_add(limit).min(total_lines);

    let mut truncated_line_count = 0usize;
    let mut rendered = String::new();
    for (position, line) in lines[start_index..end_index].iter().enumerate() {
        let line_number = start_index + position + 1;
        let char_count = line.chars().count();
        let (text, was_truncated) = if char_count > MAX_LINE_LEN {
            (line.chars().take(MAX_LINE_LEN).collect::<String>(), true)
        } else {
            ((*line).to_string(), false)
        };
        if was_truncated {
            truncated_line_count += 1;
        }
        rendered.push_str(&format!("{line_number:>6}\t{text}"));
        if was_truncated {
            rendered.push_str(" …[line truncated]");
        }
        rendered.push('\n');
    }

    let capped_by_limit = end_index < total_lines;
    let notice = if capped_by_limit {
        Some(format!(
            "Showing lines {}-{end_index} of {total_lines}. Pass a larger `limit` or a higher `offset` to read more.",
            start_index + 1,
        ))
    } else if total_lines == 0 {
        Some("File is empty.".to_string())
    } else if start_index >= total_lines {
        Some(format!(
            "`offset` {offset} is beyond the end of the file ({total_lines} lines)."
        ))
    } else if truncated_line_count > 0 {
        Some(format!(
            "{truncated_line_count} line(s) were longer than {MAX_LINE_LEN} characters and were truncated."
        ))
    } else {
        None
    };

    if let Ok(mtime) = metadata.modified() {
        tool_state.record_mtime(resolved.clone(), mtime);
    }

    json!({
        "path": path_arg,
        "start_line": start_index + 1,
        "end_line": end_index,
        "total_lines": total_lines,
        "truncated": capped_by_limit || truncated_line_count > 0,
        "notice": notice,
        "content": rendered,
    })
}
