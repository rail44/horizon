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

pub(super) fn read(state: &ToolSessionState, input: &Value) -> Value {
    with_main_root(state, input, crate::knowledge::execute_read)
}

pub(super) fn write(state: &ToolSessionState, input: &Value) -> Value {
    with_main_root(state, input, crate::knowledge::execute_write)
}

fn with_main_root(
    state: &ToolSessionState,
    input: &Value,
    execute: impl FnOnce(&std::path::Path, &Value) -> Value,
) -> Value {
    let Some(root) = state.workspace_root() else {
        return crate::tools::error_output("knowledge tools require a workspace root");
    };
    let main_root = crate::knowledge::main_root(root).unwrap_or(root.to_path_buf());
    execute(&main_root, input)
}
