use globset::Glob;
use serde_json::{json, Value};

use super::error_output;
use super::safety::resolve_path;
use super::traverse;
use crate::tools::state::ToolSessionState;

pub(super) fn execute(tool_state: &ToolSessionState, input: &Value) -> Value {
    let Some(base_arg) = input.get("base_path").and_then(Value::as_str) else {
        return error_output("fs.glob requires a `base_path` string argument");
    };
    let Some(pattern) = input.get("pattern").and_then(Value::as_str) else {
        return error_output("fs.glob requires a `pattern` string argument");
    };
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| limit as usize)
        .unwrap_or(tool_state.tools_config().fs.glob_result_limit)
        .max(1);

    let base = match resolve_path(tool_state, base_arg) {
        Ok(path) => path,
        Err(error) => return error,
    };
    if !base.is_dir() {
        return error_output(format!("`{base_arg}` is not a directory"));
    }

    let matcher = match Glob::new(pattern) {
        Ok(glob) => glob.compile_matcher(),
        Err(error) => return error_output(format!("invalid glob pattern `{pattern}`: {error}")),
    };

    let traversal_max_files = tool_state.tools_config().fs.traversal_max_files;
    let mut matches = Vec::new();
    let mut total_matches = 0usize;
    let mut visited = 0usize;
    let mut scan_truncated = false;
    for entry in traverse::walk(&base) {
        if !entry.file_type().is_file() {
            continue;
        }
        if visited >= traversal_max_files {
            scan_truncated = true;
            break;
        }
        visited += 1;
        let relative = entry.path().strip_prefix(&base).unwrap_or(entry.path());
        if !matcher.is_match(relative) {
            continue;
        }
        total_matches += 1;
        if matches.len() < limit {
            matches.push(entry.path().display().to_string());
        }
    }

    let mut output = json!({
        "base_path": base_arg,
        "pattern": pattern,
        "matches": matches,
        "returned_count": matches.len(),
        "total_matches": total_matches,
        "truncated": total_matches > matches.len(),
    });
    if scan_truncated {
        output["note"] = json!(traverse::scan_truncated_note(visited));
    }
    output
}
