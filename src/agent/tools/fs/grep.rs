use std::fs;

use globset::Glob;
use regex::Regex;
use serde_json::{json, Value};
use walkdir::WalkDir;

use super::error_output;
use super::safety::resolve_path;
use crate::agent::tools::state::ToolSessionState;

/// Default number of matches returned when the caller doesn't pass `limit`.
const DEFAULT_LIMIT: usize = 100;

pub(super) fn execute(tool_state: &ToolSessionState, input: &Value) -> Value {
    let Some(base_arg) = input.get("base_path").and_then(Value::as_str) else {
        return error_output("fs.grep requires a `base_path` string argument");
    };
    let Some(pattern) = input.get("pattern").and_then(Value::as_str) else {
        return error_output("fs.grep requires a `pattern` regex string argument");
    };
    let glob_filter = input.get("glob").and_then(Value::as_str);
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| limit as usize)
        .unwrap_or(DEFAULT_LIMIT)
        .max(1);

    let base = match resolve_path(tool_state, base_arg) {
        Ok(path) => path,
        Err(error) => return error,
    };
    if !base.is_dir() {
        return error_output(format!("`{base_arg}` is not a directory"));
    }

    let regex = match Regex::new(pattern) {
        Ok(regex) => regex,
        Err(error) => return error_output(format!("invalid regex `{pattern}`: {error}")),
    };

    let matcher = match glob_filter {
        Some(glob_pattern) => match Glob::new(glob_pattern) {
            Ok(glob) => Some(glob.compile_matcher()),
            Err(error) => {
                return error_output(format!("invalid glob pattern `{glob_pattern}`: {error}"))
            }
        },
        None => None,
    };

    let mut matches = Vec::new();
    let mut total_matches = 0usize;
    for entry in WalkDir::new(&base)
        .into_iter()
        .filter_entry(|entry| entry.file_name().to_str() != Some(".git"))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(&base).unwrap_or(entry.path());
        if let Some(matcher) = &matcher {
            if !matcher.is_match(relative) {
                continue;
            }
        }
        let Ok(content) = fs::read_to_string(entry.path()) else {
            continue; // Skip binary/non-UTF-8 files rather than erroring.
        };
        for (line_number, line) in content.lines().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            total_matches += 1;
            if matches.len() < limit {
                matches.push(json!({
                    "path": entry.path().display().to_string(),
                    "line_number": line_number + 1,
                    "line": line,
                }));
            }
        }
    }

    json!({
        "base_path": base_arg,
        "pattern": pattern,
        "matches": matches,
        "returned_count": matches.len(),
        "total_matches": total_matches,
        "truncated": total_matches > matches.len(),
    })
}
