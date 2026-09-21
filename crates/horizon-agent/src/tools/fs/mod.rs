mod edit;
mod glob;
mod grep;
mod locks;
mod read;
mod safety;
mod staleness;
mod traverse;
mod write;

use serde_json::Value;

use super::state::ToolSessionState;

/// Executes an auto-allowed (`AutoAllowRead`) file tool. Returns `None` for
/// tool ids this module doesn't own (e.g. `workspace.snapshot`), so the
/// caller can dispatch elsewhere.
///
/// Out-of-root paths are rejected here (`allow_out_of_root = false`) —
/// the caller (`execution::execute_agent_tool`) routes those to the
/// approval gate instead of reaching this function.
pub(crate) fn execute_auto(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Option<Value> {
    match tool_id {
        "fs.read" => Some(read::execute(tool_state, input, false)),
        "fs.glob" => Some(glob::execute(tool_state, input, false)),
        "fs.grep" => Some(grep::execute(tool_state, input, false)),
        _ => None,
    }
}

/// Executes a Horizon-approved (`RequireApproval`) file tool once the
/// judge or user has approved it. `fs.write`/`fs.edit` always pass
/// `allow_out_of_root = false` (writes never escape the workspace, even
/// with approval); `fs.read`/`fs.glob`/`fs.grep` pass `true` (the
/// approval gate is what authorizes the out-of-root read).
pub(crate) fn execute_approved(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Value {
    match tool_id {
        "fs.read" => read::execute(tool_state, input, true),
        "fs.glob" => glob::execute(tool_state, input, true),
        "fs.grep" => grep::execute(tool_state, input, true),
        "fs.write" => write::execute(tool_state, input),
        "fs.edit" => edit::execute(tool_state, input),
        _ => error_output(format!("tool `{tool_id}` has no Horizon-side execution")),
    }
}

/// Whether an fs read tool call's path escapes what this session may read
/// without approval — used by the execution and event layers to route the
/// call to the approval gate instead of auto-executing it.
pub(crate) fn call_escapes_root(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> bool {
    let Some(path_arg) = read_path_arg(tool_id, input) else {
        return false;
    };
    safety::escapes_root(tool_state, path_arg)
}

/// The message an out-of-root read is refused with when no human can
/// approve it: which root this session may read, plus the equivalent
/// in-workspace path when the request named the same relative path in
/// another working tree of this repository. `None` for a call this module
/// does not own, or one that names no path.
pub(crate) fn out_of_root_refusal(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Option<String> {
    let path_arg = read_path_arg(tool_id, input)?;
    let workspace_root = tool_state.workspace_root()?;
    let verb = match tool_id {
        "fs.grep" => "search",
        "fs.glob" => "list",
        _ => "read",
    };
    let mut message = format!(
        "`{tool_id}` cannot {verb} `{path_arg}`: it is outside this session's workspace root \
         `{}`, and this session has nobody who can approve access outside it.",
        workspace_root.display()
    );
    match safety::in_workspace_equivalent(tool_state, path_arg) {
        Some(equivalent) => message.push_str(&format!(
            " That path is in another checkout of the same repository; the same path in this \
             workspace is `{}`. Use that one.",
            equivalent.display()
        )),
        None => message.push_str(&format!(
            " Work from paths under `{}` instead.",
            workspace_root.display()
        )),
    }
    Some(message)
}

fn read_path_arg<'a>(tool_id: &str, input: &'a Value) -> Option<&'a str> {
    match tool_id {
        "fs.read" => input.get("path").and_then(Value::as_str),
        "fs.glob" | "fs.grep" => input.get("base_path").and_then(Value::as_str),
        _ => None,
    }
}

use super::error_output;
