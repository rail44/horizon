mod edit;
mod glob;
mod grep;
mod locks;
mod patch;
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
pub fn execute_auto(tool_state: &ToolSessionState, tool_id: &str, input: &Value) -> Option<Value> {
    match tool_id {
        "fs.read" => Some(read::execute(tool_state, input)),
        "fs.glob" => Some(glob::execute(tool_state, input)),
        "fs.grep" => Some(grep::execute(tool_state, input)),
        _ => None,
    }
}

/// Executes a Horizon-approved (`RequireApproval`) file tool once the user
/// has approved it. Callers should only reach this for
/// `fs.write`/`fs.edit`/`fs.patch`
/// (see `agent::tools::approval::is_horizon_executed_tool`); any other id
/// falls back to an `is_error` result rather than panicking.
pub fn execute_approved(tool_state: &ToolSessionState, tool_id: &str, input: &Value) -> Value {
    match tool_id {
        "fs.write" => write::execute(tool_state, input),
        "fs.edit" => edit::execute(tool_state, input),
        "fs.patch" => patch::execute(tool_state, input),
        _ => error_output(format!("tool `{tool_id}` has no Horizon-side execution")),
    }
}

fn error_output(message: impl Into<String>) -> Value {
    serde_json::json!({ "is_error": true, "message": message.into() })
}
