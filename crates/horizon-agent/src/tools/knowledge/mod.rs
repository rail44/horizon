//! `knowledge.read` / `knowledge.write` tool dispatch — the thin
//! adapter between the auto-allow execution chain (`tools::execution::
//! execute_auto_tool`) and `crate::knowledge`'s handlers. Each handler
//! takes the session's project main root (resolved from
//! `ToolSessionState::workspace_root` via `knowledge::main_root`) so
//! the store path is keyed by the same project root the prompt index
//! uses.
//!
//! Both tools are `AutoAllowRead` (no approval — the design's audit
//! trail is the tool-event recording) and are filtered out of the
//! advertised catalog for untrusted sessions by `rig_tool_definitions`
//! (gated on `RigAgentConfig::trusted_project`).

use serde_json::Value;

use crate::tools::state::ToolSessionState;

/// Executes an auto-allowed tool from this module's catalog entries.
/// Returns `None` for any other tool id, so the caller can try
/// elsewhere — same contract as `tools::config::execute_auto`.
pub(crate) fn execute_auto(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &Value,
) -> Option<Value> {
    let root = tool_state.workspace_root()?;
    // Resolve the project's main root (the --git-common-dir parent) so
    // every worktree of one project shares the same store. `None` when
    // `root` is not in a git repo — the tools surface an actionable
    // error rather than guessing a path.
    let main_root = crate::knowledge::main_root(root).unwrap_or(root.to_path_buf());
    match tool_id {
        "knowledge.read" => Some(crate::knowledge::execute_read(&main_root, input)),
        "knowledge.write" => Some(crate::knowledge::execute_write(&main_root, input)),
        _ => None,
    }
}
