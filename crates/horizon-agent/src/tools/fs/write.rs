use std::fs;

use crate::tools::output::*;

use super::error_output;
use super::locks::FileLocks;
use super::safety::resolve_path;
use super::staleness::check_staleness;
use crate::tools::state::ToolSessionState;

pub(in crate::tools) fn execute(
    tool_state: &ToolSessionState,
    input: &crate::tools::input::WriteFile,
) -> Response {
    let path_arg = input.path.as_str();
    let content = input.content.as_str();

    let resolved = match resolve_path(tool_state, path_arg, false) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let locks = FileLocks::acquire([resolved.clone()]);
    let _guards = locks.hold();

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

    Response::succeeded(FileWritten {
        path: path_arg.into(),
        bytes_written: content.len(),
        created: !existed,
    })
}
