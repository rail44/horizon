use std::fs;
use std::time::UNIX_EPOCH;

use crate::tools::output::*;

use super::error_output;
use super::safety::resolve_read_path;
use crate::tools::state::ToolSessionState;

/// Per-line character cap, independent of `limit`, so one absurdly long
/// line can't blow out the tool result.
const MAX_LINE_LEN: usize = 2000;
/// Hard cap on the rendered `content` field. This is deliberately independent
/// of the line window: many ordinary-width lines can still dwarf the useful
/// context budget before the line limit is reached.
const MAX_CONTENT_CHARS: usize = 50_000;

pub(in crate::tools) fn execute(
    tool_state: &ToolSessionState,
    input: &crate::tools::input::ReadFile,
    allow_out_of_root: bool,
) -> Response {
    let path_arg = input.path.as_str();

    let resolved = match resolve_read_path(tool_state, path_arg, allow_out_of_root) {
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

    let mut output = render_content(&content, input);

    let mtime = metadata.modified().ok();
    if let Some(mtime) = mtime {
        tool_state.record_mtime(resolved.clone(), mtime);
    }
    let content_version = mtime
        .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
        .map(|duration| {
            format!(
                "{}.{:09}:{}",
                duration.as_secs(),
                duration.subsec_nanos(),
                metadata.len()
            )
        });

    output.path = path_arg.into();
    output.content_version = content_version;
    Response::succeeded(output)
}

/// Render a bounded line window independently of filesystem access and staleness tracking.
fn render_content(content: &str, input: &crate::tools::input::ReadFile) -> FileRead {
    let offset = usize::try_from(input.offset.get()).unwrap_or(usize::MAX);
    let limit = input.limit.get() as usize;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let start_index = offset.saturating_sub(1).min(total_lines);
    let requested_end_index = start_index.saturating_add(limit).min(total_lines);

    let mut truncated_line_count = 0usize;
    let mut rendered = String::new();
    let mut rendered_chars = 0usize;
    let mut rendered_line_count = 0usize;
    let mut capped_by_chars = false;
    for (position, line) in lines[start_index..requested_end_index].iter().enumerate() {
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
        let mut rendered_line = format!("{line_number:>6}\t{text}");
        if was_truncated {
            rendered_line.push_str(" …[line truncated]");
        }
        rendered_line.push('\n');
        let line_chars = rendered_line.chars().count();
        if rendered_chars.saturating_add(line_chars) > MAX_CONTENT_CHARS {
            capped_by_chars = true;
            if was_truncated {
                truncated_line_count -= 1;
            }
            break;
        }
        rendered.push_str(&rendered_line);
        rendered_chars += line_chars;
        rendered_line_count += 1;
    }

    let end_index = start_index + rendered_line_count;
    let next_offset = (end_index < total_lines).then_some(end_index + 1);
    let mut notices = Vec::new();
    if capped_by_chars {
        notices.push(format!(
            "Output stopped at the {MAX_CONTENT_CHARS}-character cap after lines {}-{end_index} of {total_lines}. Continue with `offset` {}.",
            start_index + 1,
            next_offset.expect("a character-capped read has more lines"),
        ));
    } else if requested_end_index < total_lines {
        notices.push(format!(
            "Showing lines {}-{end_index} of {total_lines}. Continue with `offset` {} or pass a larger `limit`.",
            start_index + 1,
            next_offset.expect("a line-capped read has more lines"),
        ));
    } else if total_lines == 0 {
        notices.push("File is empty.".to_string());
    } else if start_index >= total_lines {
        notices.push(format!(
            "`offset` {offset} is beyond the end of the file ({total_lines} lines)."
        ));
    }
    if truncated_line_count > 0 {
        notices.push(format!(
            "{truncated_line_count} line(s) were longer than {MAX_LINE_LEN} characters and were truncated."
        ));
    }
    let notice = (!notices.is_empty()).then(|| notices.join(" "));

    FileRead {
        path: String::new(),
        content_version: None,
        start_line: start_index + 1,
        end_line: end_index,
        total_lines,
        truncated: next_offset.is_some() || truncated_line_count > 0,
        next_offset,
        content_chars: rendered_chars,
        notice,
        content: rendered,
    }
}
