mod approval;
mod bash;
mod catalog;
mod config;
mod execution;
mod fs;
mod network;
mod processing;
mod recall;
mod state;

pub use approval::{resolve_approval, ApprovalDecision, ApprovalOutcome};
pub use bash::{should_fold_completion, BashCompletion};
pub(crate) use catalog::{definitions, permission_for_tool, Definition};
// `execute_agent_tool`/`Execution` are re-exported fully `pub` (not
// `pub(crate)`) specifically so `tests/tier1_network_containment.rs` --
// an integration test, hence external to this crate -- can drive the real
// tier-1 dispatch path end to end (`docs/agent-approval-design.md`'s leg
// 4a containment proof). Kept to exactly these two items: everything else
// this module owns stays crate-local per the usual convention.
pub(crate) use execution::tool_result_message;
pub use execution::{cancelled_tool_call_result, execute_agent_tool, Execution, HostTools};
// `SessionNetworkProxy` is `pub` (not `pub(crate)`) for the same reason as
// `execute_agent_tool`/`Execution` above: the leg 4b containment tests in
// `tests/tier1_network_containment.rs` construct one directly to wire up a
// real per-session proxy/bridge pair the same way `horizon-sessiond`'s
// `session::run_session` does.
pub use network::SessionNetworkProxy;
pub use processing::process_agent_provider_event;
pub use state::{
    register_session_runtime, unregister_session_runtime, RecallContext, ToolSessionState,
};
// Narrow, crate-internal-only read for `judge::maybe_fire_shadow_judge` --
// see `state::live_frame_for_session`'s own doc comment for why this stays
// `pub(crate)` rather than exposing `SessionRuntime` itself.
pub(crate) use state::live_frame_for_session;

/// Fires the existing shadow judge for a trusted, supervisor-derived
/// filesystem grant request. It remains diagnostic only; the human approval
/// path is unchanged.
pub fn maybe_fire_shadow_filesystem_judge(
    session_id: crate::contract::SessionId,
    request: &crate::contract::ToolCallRequest,
    denials: &[horizon_sandbox::FilesystemDenial],
) {
    let Some(runtime) = state::session_runtime(session_id) else {
        return;
    };
    crate::judge::maybe_fire_shadow_filesystem_judge(
        &runtime.tool_state,
        session_id,
        request,
        denials,
    );
}

/// Executes a Horizon-approved (`RequireApproval`) tool once the user has
/// approved it -- `tools::approval`'s single entry point for the tools this
/// crate itself executes (as opposed to `bash`, which runs on its own
/// background thread, and `Provider::Forward`-ed tools like
/// `mock.approval_required`). Dispatches by tool id prefix to whichever
/// module owns that tool's execution; `fs`/`config` each cover their own
/// small id set.
pub(crate) fn execute_approved(
    tool_state: &ToolSessionState,
    tool_id: &str,
    input: &serde_json::Value,
) -> serde_json::Value {
    if tool_id == "config.write" {
        config::execute_approved(tool_state, tool_id, input)
    } else {
        fs::execute_approved(tool_state, tool_id, input)
    }
}

#[cfg(test)]
mod tests;
