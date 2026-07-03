use std::fs;

use serde_json::{json, Value};

use super::error_output;
use super::safety::resolve_path;
use super::staleness::check_staleness;
use crate::agent::tools::state::ToolSessionState;

pub(super) fn execute(tool_state: &ToolSessionState, input: &Value) -> Value {
    let Some(path_arg) = input.get("path").and_then(Value::as_str) else {
        return error_output("fs.write requires a `path` string argument");
    };
    let Some(content) = input.get("content").and_then(Value::as_str) else {
        return error_output("fs.write requires a `content` string argument");
    };

    let resolved = match resolve_path(tool_state, path_arg) {
        Ok(path) => path,
        Err(error) => return error,
    };

    let existed = resolved.exists();
    if existed {
        if resolved.is_dir() {
            return error_output(format!("`{path_arg}` is a directory, not a file"));
        }
        if let Err(error) = check_staleness(tool_state, &resolved, path_arg) {
            return error;
        }
    } else if let Some(parent) = resolved.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            return error_output(format!(
                "failed to create parent directories for `{path_arg}`: {error}"
            ));
        }
    }

    if let Err(error) = fs::write(&resolved, content) {
        return error_output(format!("failed to write `{path_arg}`: {error}"));
    }

    if let Ok(mtime) = fs::metadata(&resolved).and_then(|metadata| metadata.modified()) {
        tool_state.record_mtime(resolved.clone(), mtime);
    }

    json!({
        "path": path_arg,
        "bytes_written": content.len(),
        "created": !existed,
    })
}
