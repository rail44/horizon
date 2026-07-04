mod approval;
mod bash;
mod catalog;
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

#[cfg(test)]
mod tests;
