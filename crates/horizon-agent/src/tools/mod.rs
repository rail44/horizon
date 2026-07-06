mod approval;
mod bash;
mod catalog;
mod config;
mod execution;
mod fs;
mod processing;
mod state;

pub use approval::{resolve_approval, ApprovalDecision, ApprovalOutcome};
pub use bash::{should_fold_completion, BashCompletion};
pub use catalog::{definitions, permission_for_tool, Definition};
pub use execution::{
    cancelled_tool_call_result, execute_agent_tool, tool_result_message, Execution, HostTools,
};
pub use processing::process_agent_provider_event;
pub use state::{register_session_runtime, unregister_session_runtime, ToolSessionState};

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
