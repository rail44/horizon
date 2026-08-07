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

/// Whether an fs read tool call's path escapes the workspace root —
/// used by the execution and event layers to route the call to the
/// approval gate instead of auto-executing it.
pub(crate) fn call_escapes_root(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> bool {
    let path_arg = match tool_id {
        "fs.read" => input.get("path").and_then(Value::as_str),
        "fs.glob" | "fs.grep" => input.get("base_path").and_then(Value::as_str),
        _ => return false,
    };
    let Some(path_arg) = path_arg else {
        return false;
    };
    safety::escapes_root(tool_state, path_arg)
}

use super::error_output;
