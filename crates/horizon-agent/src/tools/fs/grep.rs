use std::fs;
use std::path::Path;

use globset::Glob;
use regex::Regex;
use serde_json::{json, Value};

use super::error_output;
use super::safety::resolve_path;
use super::traverse;
use crate::tools::state::ToolSessionState;

/// A match is reported as a location, never as content: `fs.grep` answers
/// "where is this?" and `fs.read` answers "what does it say?". Returning
/// the matching line (let alone surrounding context) makes the first
/// question's answer cost as much as the second's — measured on this
/// repository, one `WaitingForUser` search returns 5,008 bytes as
/// locations, 12,948 with the matching lines, and 67,423 with three lines
/// of context each. SWE-agent reached the same conclusion for a different
/// reason: "showing the model more context about each match proved to be
/// too confusing for the model" (`docs/research/`, 2026-07-25 survey).
const MAX_OUTPUT_CHARS: usize = 50_000;

struct GrepResults {
    matches: Vec<Value>,
    total_matches: usize,
    bytes_read: u64,
    rendered_chars: usize,
    output_capped: bool,
}

fn render_location(path: &Path, line_number: usize) -> Value {
    json!({
        "path": path.display().to_string(),
        "line_number": line_number,
    })
}

fn scan_file(path: &Path, regex: &Regex, limit: usize, results: &mut GrepResults) {
    let Ok(content) = fs::read_to_string(path) else {
        return; // Skip binary/non-UTF-8 files rather than erroring.
    };
    results.bytes_read += content.len() as u64;
    for (index, line) in content.lines().enumerate() {
        if !regex.is_match(line) {
            continue;
        }
        results.total_matches += 1;
        if results.matches.len() >= limit || results.output_capped {
            continue;
        }
        let candidate = render_location(path, index + 1);
        let candidate_chars = candidate.to_string().chars().count();
        if results.rendered_chars.saturating_add(candidate_chars) > MAX_OUTPUT_CHARS {
            results.output_capped = true;
            continue;
        }
        results.rendered_chars += candidate_chars;
        results.matches.push(candidate);
    }
}

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
        .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
        .unwrap_or(tool_state.tools_config().fs.grep_result_limit)
        .max(1);

    let base = match resolve_path(tool_state, base_arg) {
        Ok(path) => path,
        Err(error) => return error,
    };
    if !base.is_dir() && !base.is_file() {
        return error_output(format!("`{base_arg}` is not a file or directory"));
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

    let fs_config = tool_state.tools_config().fs;
    let mut results = GrepResults {
        matches: Vec::new(),
        total_matches: 0,
        bytes_read: 0,
        rendered_chars: 0,
        output_capped: false,
    };
    let mut visited = 0usize;
    let mut scan_truncated = false;
    if base.is_file() {
        let matches_filter = matcher
            .as_ref()
            .is_none_or(|matcher| matcher.is_match(base.file_name().unwrap_or_default()));
        if matches_filter {
            visited = 1;
            scan_file(&base, &regex, limit, &mut results);
        }
    } else {
        for entry in traverse::walk(&base) {
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
            {
                continue;
            }
            if visited >= fs_config.traversal_max_files
                || results.bytes_read >= fs_config.grep_max_bytes
            {
                scan_truncated = true;
                break;
            }
            visited += 1;
            let relative = entry.path().strip_prefix(&base).unwrap_or(entry.path());
            if let Some(matcher) = &matcher {
                if !matcher.is_match(relative) {
                    continue;
                }
            }
            scan_file(entry.path(), &regex, limit, &mut results);
        }
    }

    let mut notes = Vec::new();
    if scan_truncated {
        notes.push(traverse::scan_truncated_note(visited));
    }
    if results.output_capped {
        notes.push(format!(
            "Returned matches stopped at the {MAX_OUTPUT_CHARS}-character output cap; narrow the path or pattern."
        ));
    }
    if input.get("context").is_some() {
        notes.push(
            "`context` is no longer accepted: fs.grep returns locations only. \
             Use fs.read with offset/limit around a reported line."
                .to_string(),
        );
    }

    // `rendered_chars` bounds match bodies while scanning. Measure the final
    // JSON too so paths, field names, and notes cannot push the actual tool
    // result over the same hard cap.
    let mut matches = results.matches;
    let mut output_capped = results.output_capped;
    loop {
        let returned_count = matches.len();
        let mut output = json!({
            "base_path": base_arg,
            "pattern": pattern,
            "matches": matches,
            "returned_count": returned_count,
            "total_matches": results.total_matches,
            "truncated": results.total_matches > returned_count || output_capped || scan_truncated,
        });
        if !notes.is_empty() {
            output["note"] = json!(notes.join(" "));
        }
        if output.to_string().chars().count() <= MAX_OUTPUT_CHARS || matches.is_empty() {
            return output;
        }
        matches.pop();
        output_capped = true;
        if !notes.iter().any(|note| note.contains("output cap")) {
            notes.push(format!(
                "Returned matches stopped at the {MAX_OUTPUT_CHARS}-character output cap; narrow the path or pattern."
            ));
        }
    }
}
