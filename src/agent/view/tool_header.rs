//! The tool block's one-line header (`docs/agent-output-ui-design.md`
//! decision 2): status glyph + verb + target, plus a result summary once
//! finished -- e.g. `✓ Edit src/agent/view/mod.rs · +2 -1`. Pure string
//! formatting only; the reactive re-derivation from the live frame and the
//! header's color/click behavior live in `tool_view`.

use serde_json::Value;

use super::diff;
use super::transcript::{ToolBlock, ToolStatus};

/// How much of a path/pattern/command argument is shown before truncating
/// -- keeps the one-line header from wrapping in typical pane widths.
const MAX_TARGET_CHARS: usize = 72;

pub(super) fn header_line(tool: &ToolBlock) -> String {
    let glyph = status_glyph(tool);
    let body = verb_and_target(tool);
    match result_summary(tool) {
        Some(summary) => format!("{glyph} {body} · {summary}"),
        None => format!("{glyph} {body}"),
    }
}

fn status_glyph(tool: &ToolBlock) -> &'static str {
    match &tool.status {
        ToolStatus::Preparing { .. } | ToolStatus::Requested => "○",
        ToolStatus::Started => "●",
        ToolStatus::Finished { .. } if tool.is_error() => "✗",
        ToolStatus::Finished { .. } => "✓",
    }
}

fn verb_and_target(tool: &ToolBlock) -> String {
    if let ToolStatus::Preparing { bytes } = &tool.status {
        return match &tool.tool_id {
            Some(tool_id) => format!("Preparing {tool_id}… ({bytes}B)"),
            None => format!("Preparing a tool call… ({bytes}B)"),
        };
    }

    let Some(tool_id) = tool.tool_id.as_deref() else {
        return "Tool call".to_string();
    };
    let input = tool.input.as_ref();

    match tool_id {
        "fs.edit" => format!("Edit {}", path_arg(input)),
        "fs.write" => format!("{} {}", write_verb(tool), path_arg(input)),
        "fs.read" => format!("Read {}", path_arg(input)),
        "fs.glob" => format!("Find {}", pattern_arg(input)),
        "fs.grep" => format!("Grep {}", pattern_arg(input)),
        "bash" => format!("Run {}", command_arg(input)),
        "workspace.snapshot" => "Snapshot workspace".to_string(),
        other => format!("{other} {}", unknown_input_summary(input)),
    }
}

/// `fs.write` only learns whether it created vs. overwrote a file once it
/// has actually run (the request input never says) -- see `tools/fs/
/// write.rs`'s `created` output field.
fn write_verb(tool: &ToolBlock) -> &'static str {
    if let ToolStatus::Finished { output } = &tool.status {
        if output.get("created").and_then(Value::as_bool) == Some(false) {
            return "Overwrite";
        }
    }
    "Write"
}

fn path_arg(input: Option<&Value>) -> String {
    string_arg(input, "path")
}

fn pattern_arg(input: Option<&Value>) -> String {
    string_arg(input, "pattern")
}

fn command_arg(input: Option<&Value>) -> String {
    string_arg(input, "command")
}

fn string_arg(input: Option<&Value>, key: &str) -> String {
    input
        .and_then(|input| input.get(key))
        .and_then(Value::as_str)
        .map(truncate_inline)
        .unwrap_or_else(|| "?".to_string())
}

fn unknown_input_summary(input: Option<&Value>) -> String {
    match input {
        Some(Value::Object(map)) if !map.is_empty() => {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort();
            format!("({})", keys.join(", "))
        }
        _ => String::new(),
    }
}

/// Collapses a (possibly multi-line) argument to one line and caps its
/// length -- long `bash` commands and file paths must not blow out the
/// one-line header (`docs/agent-output-ui-design.md` decision 2).
fn truncate_inline(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TARGET_CHARS {
        return collapsed;
    }
    let mut truncated: String = collapsed.chars().take(MAX_TARGET_CHARS).collect();
    truncated.push('…');
    truncated
}

fn result_summary(tool: &ToolBlock) -> Option<String> {
    let ToolStatus::Finished { output } = &tool.status else {
        return None;
    };

    if tool.is_error() {
        let message = output
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("failed");
        return Some(format!("failed: {}", truncate_inline(message)));
    }

    Some(match tool.tool_id.as_deref() {
        Some("fs.edit") => edit_result_summary(tool),
        Some("fs.write") => write_result_summary(output),
        Some("fs.read") => read_result_summary(output),
        Some("fs.glob") | Some("fs.grep") => match_result_summary(output),
        Some("bash") => bash_result_summary(output),
        Some("workspace.snapshot") => snapshot_result_summary(output),
        _ => unknown_result_summary(output),
    })
}

fn edit_result_summary(tool: &ToolBlock) -> String {
    match tool.edit_strings() {
        Some((old, new)) => {
            let stat = diff::diff_stat(&diff::line_diff(old, new));
            format!("+{} -{}", stat.added, stat.removed)
        }
        None => "done".to_string(),
    }
}

fn write_result_summary(output: &Value) -> String {
    match output.get("bytes_written").and_then(Value::as_u64) {
        Some(bytes) => format!("{bytes} bytes"),
        None => "done".to_string(),
    }
}

fn read_result_summary(output: &Value) -> String {
    let start = output.get("start_line").and_then(Value::as_u64);
    let end = output.get("end_line").and_then(Value::as_u64);
    let total = output.get("total_lines").and_then(Value::as_u64);
    let truncated = output
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    match (start, end, total) {
        (Some(start), Some(end), Some(total)) => {
            let count = end.saturating_sub(start).saturating_add(1);
            if truncated {
                format!("{count} of {total} lines")
            } else {
                format!("{count} lines")
            }
        }
        _ => "done".to_string(),
    }
}

fn match_result_summary(output: &Value) -> String {
    let returned = output
        .get("returned_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = output
        .get("total_matches")
        .and_then(Value::as_u64)
        .unwrap_or(returned);
    let truncated = output
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if truncated && total > returned {
        format!("{returned} of {total} matches")
    } else {
        format!("{returned} match{}", if returned == 1 { "" } else { "es" })
    }
}

fn bash_result_summary(output: &Value) -> String {
    let exit_code = output.get("exit_code").and_then(Value::as_i64);
    let lines = output
        .get("output")
        .and_then(Value::as_str)
        .map(|text| text.lines().count())
        .unwrap_or(0);
    let line_word = if lines == 1 { "line" } else { "lines" };

    match exit_code {
        Some(code) => format!("exit {code} · {lines} {line_word}"),
        None => format!("{lines} {line_word}"),
    }
}

fn snapshot_result_summary(output: &Value) -> String {
    let tabs = output.get("tab_count").and_then(Value::as_u64).unwrap_or(0);
    format!("{tabs} tab{}", if tabs == 1 { "" } else { "s" })
}

/// Fallback for any tool id this module doesn't special-case -- a terse
/// shape summary, never the raw JSON dump (that's the body's fallback
/// only, see `tool_view::append_unknown_body`).
fn unknown_result_summary(output: &Value) -> String {
    match output {
        Value::Object(map) => format!(
            "{} field{}",
            map.len(),
            if map.len() == 1 { "" } else { "s" }
        ),
        Value::Array(values) => format!(
            "{} item{}",
            values.len(),
            if values.len() == 1 { "" } else { "s" }
        ),
        _ => "done".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::ToolCallId;
    use serde_json::json;

    fn tool(tool_id: &str, input: Value, status: ToolStatus) -> ToolBlock {
        ToolBlock {
            call_id: Some(ToolCallId("call-1".to_string())),
            tool_id: Some(tool_id.to_string()),
            input: Some(input),
            status,
        }
    }

    #[test]
    fn pending_edit_header_shows_verb_and_target_without_a_summary() {
        let block = tool(
            "fs.edit",
            json!({ "path": "src/lib.rs", "old_string": "a", "new_string": "b" }),
            ToolStatus::Requested,
        );

        assert_eq!(header_line(&block), "○ Edit src/lib.rs");
    }

    #[test]
    fn finished_edit_header_includes_the_diffstat() {
        let block = tool(
            "fs.edit",
            json!({
                "path": "src/lib.rs",
                "old_string": "one\ntwo\n",
                "new_string": "one\ntwo\nthree\nfour\n",
            }),
            ToolStatus::Finished {
                output: json!({ "path": "src/lib.rs", "replaced": true }),
            },
        );

        assert_eq!(header_line(&block), "✓ Edit src/lib.rs · +2 -0");
    }

    #[test]
    fn write_header_switches_verb_once_finished_as_an_overwrite() {
        let pending = tool(
            "fs.write",
            json!({ "path": "src/new.rs", "content": "fn main() {}\n" }),
            ToolStatus::Requested,
        );
        assert_eq!(header_line(&pending), "○ Write src/new.rs");

        let overwritten = tool(
            "fs.write",
            json!({ "path": "src/new.rs", "content": "fn main() {}\n" }),
            ToolStatus::Finished {
                output: json!({ "path": "src/new.rs", "bytes_written": 13, "created": false }),
            },
        );
        assert_eq!(
            header_line(&overwritten),
            "✓ Overwrite src/new.rs · 13 bytes"
        );
    }

    #[test]
    fn bash_header_reports_exit_code_and_output_line_count() {
        let running = tool(
            "bash",
            json!({ "command": "cargo test" }),
            ToolStatus::Started,
        );
        assert_eq!(header_line(&running), "● Run cargo test");

        let finished = tool(
            "bash",
            json!({ "command": "cargo test" }),
            ToolStatus::Finished {
                output: json!({ "exit_code": 0, "output": "ok\nall good\n", "truncated": false }),
            },
        );
        assert_eq!(
            header_line(&finished),
            "✓ Run cargo test · exit 0 · 2 lines"
        );
    }

    #[test]
    fn a_failed_tool_call_shows_the_danger_glyph_and_message() {
        let block = tool(
            "bash",
            json!({ "command": "false" }),
            ToolStatus::Finished {
                output: json!({ "is_error": true, "message": "bash command was terminated" }),
            },
        );

        assert_eq!(
            header_line(&block),
            "✗ Run false · failed: bash command was terminated"
        );
    }

    #[test]
    fn glob_header_reports_returned_vs_total_matches_when_truncated() {
        let block = tool(
            "fs.glob",
            json!({ "base_path": ".", "pattern": "**/*.rs" }),
            ToolStatus::Finished {
                output: json!({
                    "matches": ["a.rs", "b.rs"],
                    "returned_count": 2,
                    "total_matches": 5,
                    "truncated": true,
                }),
            },
        );

        assert_eq!(header_line(&block), "✓ Find **/*.rs · 2 of 5 matches");
    }

    #[test]
    fn workspace_snapshot_header_has_a_fixed_verb_and_target() {
        let block = tool(
            "workspace.snapshot",
            json!({}),
            ToolStatus::Finished {
                output: json!({ "tab_count": 3 }),
            },
        );

        assert_eq!(header_line(&block), "✓ Snapshot workspace · 3 tabs");
    }

    #[test]
    fn unknown_tool_header_falls_back_to_the_tool_id_and_input_keys() {
        let block = tool(
            "mock.custom",
            json!({ "foo": 1, "bar": 2 }),
            ToolStatus::Requested,
        );

        assert_eq!(header_line(&block), "○ mock.custom (bar, foo)");
    }

    #[test]
    fn preparing_header_shows_the_tool_id_once_known() {
        let block = ToolBlock {
            call_id: None,
            tool_id: Some("bash".to_string()),
            input: None,
            status: ToolStatus::Preparing { bytes: 12 },
        };

        assert_eq!(header_line(&block), "○ Preparing bash… (12B)");
    }
}
