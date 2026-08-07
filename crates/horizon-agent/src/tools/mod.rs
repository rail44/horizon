mod approval;
mod bash;
mod board;
mod catalog;
mod config;
mod execution;
pub(crate) mod explore;
mod fs;
mod knowledge;
mod network;
mod processing;
mod recall;
mod state;
pub(crate) mod web;

pub use approval::{resolve_approval, resolve_auto_approval, ApprovalDecision, ApprovalOutcome};
pub(crate) use bash::{
    git_prefilter, metadata_writable_roots, requires_metadata_write, GitPrefilterVerdict,
};
pub use bash::{should_fold_completion, BashCompletion, ToolCompletion};
pub(crate) use catalog::{definitions, permission_for_tool, Definition};
pub(crate) use fs::call_escapes_root;
// The `task` daemon seam (`docs/agent-explore-design.md`): `pub`
// because `horizon-agentd` implements it and installs it on every
// session's `ToolSessionState`, the same way it constructs the network
// proxy and judge handles this module also exposes.
pub use explore::{ExplorationHost, StartedExploration};
// The board daemon seam (`docs/board-keeper-design.md`): `pub` for the
// same reason as `ExplorationHost` — `horizon-agentd` implements
// `BoardHost` and installs it on every session's `ToolSessionState`.
pub use board::BoardHost;
// The model-visible ids of that tool and its companion fetch tool, named
// for the places outside `explore` that have to recognize them: dispatch
// (`execution`), the prompt's delegation-routing block
// (`providers::rig::session::advertises_task_tool`), and the conditional
// advertisement of `task_output` alongside `task`
// (`providers::rig::completion::rig_tool_definitions`).
pub(crate) use explore::OUTPUT_TOOL_ID as TASK_OUTPUT_TOOL_ID;
pub(crate) use explore::TOOL_ID as TASK_TOOL_ID;
// The asynchronous-delivery seam (`docs/agent-async-task-design.md`): the
// rig session loop drains finished children before each provider round and
// wakes on the channel below when one lands with no turn to ride on.
pub(crate) use explore::{notification_event, register_wake, take_notification, unregister_wake};
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
// real per-session proxy the same way `horizon-agentd`'s
// `session::run_session` does.
pub use network::{SessionDomainPolicy, SessionNetworkProxy};
pub use processing::process_agent_provider_event;
pub use state::{
    register_session_runtime, unregister_session_runtime, RecallContext, ToolSessionState,
};
// Narrow, crate-internal-only read for the approval judge --
// see `state::live_frame_for_session`'s own doc comment for why this stays
// `pub(crate)` rather than exposing `SessionRuntime` itself.
pub(crate) use state::live_frame_for_session;

pub use crate::judge::{ApprovalCandidate, ApprovalGate, ApprovalJudgment, JudgeDecision};

/// Starts the enforcing approval gate for a fully-derived candidate.
/// Missing runtime/judge configuration fails closed to the human path.
pub fn start_approval_gate(
    session_id: crate::contract::SessionId,
    candidate: ApprovalCandidate,
) -> ApprovalGate {
    let Some(runtime) = state::session_runtime(session_id) else {
        return ApprovalGate::Human(Box::new(candidate));
    };
    crate::judge::start_approval_gate(
        &runtime.tool_state,
        session_id,
        candidate,
        runtime.async_results.clone(),
    )
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

/// Constructs the wire-visible tool error-output shape
/// `{"is_error": true, "message": ...}`. `ToolCallResult::new`
/// (`contract.rs`) and the DuckDB projection both read `output`'s
/// `"is_error"` key to derive the typed `is_error` field, so every tool
/// result that represents a failure must carry this shape in its `output`
/// JSON. Callers that need additional fields (`output`, `truncated`, etc.)
/// build on the returned base value by inserting into its object map.
pub(crate) fn error_output(message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({ "is_error": true, "message": message.into() })
}

#[cfg(test)]
mod error_output_tests {
    use super::error_output;
    use serde_json::json;

    #[test]
    fn base_shape_is_error_with_message() {
        let value = error_output("something went wrong");
        assert_eq!(
            value,
            json!({ "is_error": true, "message": "something went wrong" })
        );
    }

    #[test]
    fn extra_fields_on_top_of_base() {
        let mut value = error_output("partial failure");
        if let Some(map) = value.as_object_mut() {
            map.insert("output".to_string(), json!("captured text"));
            map.insert("truncated".to_string(), json!(true));
            map.insert("output_file".to_string(), json!("/tmp/spill.txt"));
        }
        assert_eq!(
            value,
            json!({
                "is_error": true,
                "message": "partial failure",
                "output": "captured text",
                "truncated": true,
                "output_file": "/tmp/spill.txt",
            })
        );
    }
}

#[cfg(test)]
mod tests;
