use crate::tools::output::*;
use globset::Glob;

use super::error_output;
use super::safety::resolve_read_path;
use super::traverse;
use crate::tools::state::ToolSessionState;

pub(in crate::tools) fn execute(
    tool_state: &ToolSessionState,
    input: &crate::tools::input::Glob,
    allow_out_of_root: bool,
) -> Response {
    let base_arg = input.base_path.as_str();
    let pattern = input.pattern.as_str();
    let limit = usize::try_from(input.limit.get()).unwrap_or(usize::MAX);

    let base = match resolve_read_path(tool_state, base_arg, allow_out_of_root) {
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
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
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

    Response::succeeded(Matches {
        base_path: base_arg.into(),
        pattern: pattern.into(),
        returned_count: matches.len(),
        total_matches,
        truncated: total_matches > matches.len(),
        matches,
        note: scan_truncated.then(|| traverse::scan_truncated_note(visited)),
    })
}
