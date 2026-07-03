use std::fs;

use serde_json::{json, Value};

use super::error_output;
use super::safety::resolve_path;
use super::staleness::check_staleness;
use crate::agent::tools::state::ToolSessionState;

pub(super) fn execute(tool_state: &ToolSessionState, input: &Value) -> Value {
    let Some(path_arg) = input.get("path").and_then(Value::as_str) else {
        return error_output("fs.edit requires a `path` string argument");
    };
    let Some(old_string) = input.get("old_string").and_then(Value::as_str) else {
        return error_output("fs.edit requires an `old_string` string argument");
    };
    let Some(new_string) = input.get("new_string").and_then(Value::as_str) else {
        return error_output("fs.edit requires a `new_string` string argument");
    };
    if old_string.is_empty() {
        return error_output("`old_string` must not be empty");
    }
    if old_string == new_string {
        return error_output("`old_string` and `new_string` are identical — nothing to edit");
    }

    let resolved = match resolve_path(tool_state, path_arg) {
        Ok(path) => path,
        Err(error) => return error,
    };

    if !resolved.is_file() {
        return error_output(format!(
            "`{path_arg}` does not exist as a file — use fs.write to create it"
        ));
    }

    if let Err(error) = check_staleness(tool_state, &resolved, path_arg) {
        return error;
    }

    let content = match fs::read_to_string(&resolved) {
        Ok(content) => content,
        Err(error) => {
            return error_output(format!("cannot read `{path_arg}` as UTF-8 text: {error}"))
        }
    };

    let match_count = content.matches(old_string).count();
    if match_count == 0 {
        return error_output(format!(
            "`old_string` not found in `{path_arg}` — check the exact text (including whitespace) and try again"
        ));
    }
    if match_count > 1 {
        return error_output(format!(
            "found {match_count} matches for `old_string` in `{path_arg}` — include more surrounding context to make it unique"
        ));
    }

    let updated = content.replacen(old_string, new_string, 1);
    if let Err(error) = fs::write(&resolved, &updated) {
        return error_output(format!("failed to write `{path_arg}`: {error}"));
    }

    if let Ok(mtime) = fs::metadata(&resolved).and_then(|metadata| metadata.modified()) {
        tool_state.record_mtime(resolved.clone(), mtime);
    }

    json!({
        "path": path_arg,
        "replaced": true,
    })
}
