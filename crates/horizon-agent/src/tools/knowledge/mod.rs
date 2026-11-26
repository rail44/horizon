//! `knowledge_read` / `knowledge_write` tool dispatch — the thin
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

use crate::tools::output::Response;

use crate::tools::state::ToolSessionState;

pub(super) fn read(state: &ToolSessionState, input: &crate::tools::input::ReadEntry) -> Response {
    with_main_root(state, |data_home, root| {
        crate::knowledge::execute_read(data_home, root, &input.id)
    })
}

pub(super) fn write(
    state: &ToolSessionState,
    input: &crate::tools::input::KnowledgeWrite,
) -> Response {
    with_main_root(state, |data_home, root| {
        crate::knowledge::execute_write(data_home, root, input)
    })
}

fn with_main_root(
    state: &ToolSessionState,
    execute: impl FnOnce(&std::path::Path, &std::path::Path) -> Response,
) -> Response {
    let Some(root) = state.workspace_root() else {
        return crate::tools::output::error("knowledge tools require a workspace root");
    };
    let Some(data_home) = state.data_home() else {
        return crate::tools::output::error("knowledge tools have no store in this session");
    };
    let main_root = crate::knowledge::main_root(root).unwrap_or(root.to_path_buf());
    execute(data_home, &main_root)
}
