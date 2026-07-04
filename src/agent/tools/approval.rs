use serde_json::{json, Value};

use crate::agent::contract::{
    Command, Event, SessionState, ToolCallId, ToolCallRequest, ToolCallResult,
};
use crate::agent::frame::AgentFrame;
use crate::agent::tools::bash;
use crate::agent::tools::fs;
use crate::agent::tools::state::{session_runtime, SessionRuntime};
use crate::session::SessionId;

/// The user's decision on a pending `ApprovalRequested` tool call.
#[derive(Clone, Debug)]
pub(crate) enum ApprovalDecision {
    Approve,
    Deny { reason: Option<String> },
}

/// What the caller (the approve/deny UI) should do next after
/// `resolve_approval` runs.
#[derive(Debug)]
pub(crate) enum ApprovalOutcome {
    /// Horizon executed (or, for a deny, short-circuited) the tool
    /// app-side, synchronously. `frame` is the session's updated live
    /// frame — already folded through the session's `LiveState`, so it's
    /// ready to publish via `Frames::update_agent_frame` — and `command` is
    /// the `Command::ToolCallResult` to forward to the provider.
    Executed { frame: AgentFrame, command: Command },
    /// Horizon started executing the tool app-side, but off the UI thread —
    /// currently just `bash` on approve, since a command can run for up to
    /// its timeout (`docs/agent-tools-design.md`, "Bash Semantics"). `frame`
    /// already has the running-state events (`ToolRunning`/
    /// `ToolCallStarted`) folded in, ready to publish via
    /// `Frames::update_agent_frame` — but there is no `command` yet. The
    /// eventual result arrives later on `SessionRuntime::bash_results` and
    /// is folded (and forwarded to the provider) by the effect
    /// `app/runtime/agent.rs::spawn_agent_session` sets up for it, not by
    /// this call.
    Started { frame: AgentFrame },
    /// Not a tool Horizon executes on approval (or no runtime is registered
    /// for the session) — forward the original `ApproveToolCall`/
    /// `DenyToolCall` command to the provider, exactly as before this
    /// feature existed. This is `mock.approval_required`'s path today.
    Forward(Command),
    /// A Horizon-executed tool call that already has a `ToolCallFinished`
    /// in the frame (double-click, or a click racing the first result's
    /// round-trip). Do nothing: re-running the tool would repeat its side
    /// effects, and forwarding would emit a second `ToolCallResult`.
    AlreadyResolved,
}

/// Tool ids Horizon executes itself once approved, rather than notifying
/// the provider via `ApproveToolCall`/`DenyToolCall` and waiting for it to
/// report a result. See `docs/agent-tools-design.md`, "Approval Wiring".
fn is_horizon_executed_tool(tool_id: &str) -> bool {
    matches!(tool_id, "fs.write" | "fs.edit" | "bash")
}

/// Resolves a user's approve/deny decision for the tool call pending in
/// `frame` under `call_id`.
pub(crate) fn resolve_approval(
    frame: &AgentFrame,
    session_id: SessionId,
    call_id: ToolCallId,
    decision: ApprovalDecision,
) -> ApprovalOutcome {
    if let Some(outcome) = try_execute(frame, session_id, &call_id, &decision) {
        return outcome;
    }

    ApprovalOutcome::Forward(match decision {
        ApprovalDecision::Approve => Command::ApproveToolCall { call_id },
        ApprovalDecision::Deny { reason } => Command::DenyToolCall { call_id, reason },
    })
}

fn try_execute(
    frame: &AgentFrame,
    session_id: SessionId,
    call_id: &ToolCallId,
    decision: &ApprovalDecision,
) -> Option<ApprovalOutcome> {
    let request = frame.tool_call_request(call_id)?;
    if !is_horizon_executed_tool(&request.tool_id) {
        return None;
    }
    if frame.has_tool_call_finished(call_id) {
        return Some(ApprovalOutcome::AlreadyResolved);
    }
    let runtime = session_runtime(session_id)?;

    Some(if request.tool_id == "bash" {
        resolve_bash(&runtime, request, decision)
    } else {
        resolve_fs_tool(&runtime, request, decision)
    })
}

/// `fs.write`/`fs.edit`: both run to completion synchronously, so their
/// approve/deny always resolves to `Executed`.
fn resolve_fs_tool(
    runtime: &SessionRuntime,
    request: &ToolCallRequest,
    decision: &ApprovalDecision,
) -> ApprovalOutcome {
    match decision {
        ApprovalDecision::Approve => {
            let output =
                fs::execute_approved(&runtime.tool_state, &request.tool_id, &request.input);
            synchronous_result(runtime, &request.call_id, output, true)
        }
        ApprovalDecision::Deny { .. } => {
            synchronous_result(runtime, &request.call_id, denied_output(), false)
        }
    }
}

/// `bash`: a deny short-circuits synchronously exactly like the fs tools,
/// but an approve only *starts* the command — see `ApprovalOutcome::
/// Started`.
fn resolve_bash(
    runtime: &SessionRuntime,
    request: &ToolCallRequest,
    decision: &ApprovalDecision,
) -> ApprovalOutcome {
    match decision {
        ApprovalDecision::Approve => {
            let call_id = request.call_id.clone();
            let events = [
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_id.clone()),
            ];
            let frame = runtime
                .live_state
                .extend_provider_events(events.into_iter().map(Into::into));

            bash::spawn(
                call_id,
                request.input.clone(),
                runtime.tool_state.bash_cwd_handle(),
                runtime.tool_state.bash_config(),
                runtime.bash_results.clone(),
            );

            ApprovalOutcome::Started { frame }
        }
        ApprovalDecision::Deny { .. } => {
            synchronous_result(runtime, &request.call_id, denied_output(), false)
        }
    }
}

fn denied_output() -> Value {
    json!({ "is_error": true, "message": "denied by user" })
}

/// Folds a synchronous tool result into the session's live frame — the
/// `ToolRunning`/`ToolCallStarted` pair too if `ran` (an approve that
/// actually executed the tool, as opposed to a deny that short-circuited it
/// without ever starting it) — and pairs it with the `Command::
/// ToolCallResult` to forward to the provider.
fn synchronous_result(
    runtime: &SessionRuntime,
    call_id: &ToolCallId,
    output: Value,
    ran: bool,
) -> ApprovalOutcome {
    let result = ToolCallResult {
        call_id: call_id.clone(),
        output,
    };

    let mut events = Vec::new();
    if ran {
        events.push(Event::StateChanged(SessionState::ToolRunning));
        events.push(Event::ToolCallStarted(call_id.clone()));
    }
    events.push(Event::ToolCallFinished(result.clone()));
    events.push(Event::StateChanged(SessionState::WaitingForUser));

    let frame = runtime
        .live_state
        .extend_provider_events(events.into_iter().map(Into::into));

    ApprovalOutcome::Executed {
        frame,
        command: Command::ToolCallResult(result),
    }
}
