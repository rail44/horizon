use serde_json::{json, Value};

use crate::contract::SessionId;
use crate::contract::{Command, Event, SessionState, ToolCallId, ToolCallRequest, ToolCallResult};
use crate::frame::AgentFrame;
use crate::tools::bash;
use crate::tools::state::{session_runtime, SessionRuntime};

/// The user's decision on a pending `ApprovalRequested` tool call.
#[derive(Clone, Debug)]
pub enum ApprovalDecision {
    Approve,
    Deny { reason: Option<String> },
}

/// What the caller (the approve/deny UI) should do next after
/// `resolve_approval` runs.
#[derive(Debug)]
pub enum ApprovalOutcome {
    /// Horizon executed (or, for a deny, short-circuited) the tool
    /// app-side, synchronously. `events` are exactly the events that were
    /// just folded into the session's `LiveState` (in order) — this is
    /// what the one production caller uses: `horizon-sessiond` (the only
    /// place agent sessions run today — see `crate::client`'s module doc,
    /// there is no in-process fallback) forwards `events` over the wire to
    /// Horizon, since a whole-frame snapshot isn't the wire's
    /// event-envelope shape (`resolve_and_forward` in
    /// `crates/horizon-sessiond/src/session.rs`, which discards `frame`
    /// via `..`). `frame` — the session's updated live frame, already
    /// folded through the session's `LiveState` — is kept for a caller
    /// that wants the whole updated frame directly instead of replaying
    /// events (this crate's own tests use it to assert fold correctness).
    /// `command` is the `Command::ToolCallResult` to forward to the
    /// provider.
    Executed {
        events: Vec<Event>,
        frame: AgentFrame,
        command: Command,
    },
    /// Horizon started executing the tool app-side, but off the UI thread —
    /// currently just `bash` on approve, since a command can run for up to
    /// its timeout (`docs/agent-tools-design.md`, "Bash Semantics"). `events`/
    /// `frame` are the running-state events (`ToolRunning`/`ToolCallStarted`)
    /// already folded in — see `Executed`'s doc comment for why both are
    /// exposed — but there is no `command` yet. The eventual result arrives
    /// later on the per-session `bash_results` channel registered by
    /// `register_session_runtime` and is folded (and forwarded to the
    /// provider) by `fold_bash_completion` in `horizon-sessiond`'s session
    /// loop (`crates/horizon-sessiond/src/session.rs`), not by this call.
    Started {
        events: Vec<Event>,
        frame: AgentFrame,
    },
    /// Not a tool Horizon executes on approval (or no runtime is registered
    /// for the session) — forward the original `ApproveToolCall`/
    /// `DenyToolCall` command to the provider, exactly as before this
    /// feature existed. This is `mock.approval_required`'s path today.
    Forward(Command),
    /// A Horizon-executed tool call that has already been resolved (a
    /// `ToolCallFinished` in the frame) or is already running (a
    /// `ToolCallStarted` with no `ToolCallFinished` yet — see
    /// `AgentFrame::has_tool_call_started`'s doc comment for why `bash`
    /// needs this half too): a double-click, a click racing the first
    /// result's round trip, or a duplicate Approve/Deny for a call that's
    /// still executing. Do nothing: re-running the tool would repeat its
    /// side effects (or, for `bash`, spawn a second concurrent process for
    /// the same call), and forwarding would emit a second `ToolCallResult`.
    /// Every caller that reaches this logs the drop rather than silently
    /// swallowing it — see `horizon-sessiond`'s `session::resolve_and_forward`.
    AlreadyResolved,
}

/// Tool ids Horizon executes itself once approved, rather than notifying
/// the provider via `ApproveToolCall`/`DenyToolCall` and waiting for it to
/// report a result. See `docs/agent-tools-design.md`, "Approval Wiring".
/// `config.write` (`tools::config`) joins the fs tools here since it's the
/// same "runs to completion synchronously" shape -- see
/// [`resolve_synchronous_tool`].
fn is_horizon_executed_tool(tool_id: &str) -> bool {
    matches!(tool_id, "fs.write" | "fs.edit" | "bash" | "config.write")
}

/// Resolves a user's approve/deny decision for the tool call pending in
/// `frame` under `call_id`.
pub fn resolve_approval(
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
    // The pending -> resolved transition's atomic guard: once a call has
    // *started* (bash) or *finished* (any of the three), every later
    // Approve/Deny for the same call_id must be a no-op. Checked against
    // `frame` at the top of this call, before anything else runs, so there
    // is exactly one moment this can flip from "not yet decided" to
    // "decided" per call_id -- see `AgentFrame::has_tool_call_started`'s
    // doc comment for why `has_tool_call_finished` alone isn't enough for
    // `bash`.
    if frame.has_tool_call_finished(call_id) || frame.has_tool_call_started(call_id) {
        return Some(ApprovalOutcome::AlreadyResolved);
    }
    let runtime = session_runtime(session_id)?;

    Some(if request.tool_id == "bash" {
        resolve_bash(session_id, &runtime, request, decision)
    } else {
        resolve_synchronous_tool(&runtime, request, decision)
    })
}

/// `fs.write`/`fs.edit`/`config.write`: all run to completion synchronously,
/// so their approve/deny always resolves to `Executed`. Dispatches through
/// `tools::execute_approved`, which picks the owning module by tool id.
fn resolve_synchronous_tool(
    runtime: &SessionRuntime,
    request: &ToolCallRequest,
    decision: &ApprovalDecision,
) -> ApprovalOutcome {
    match decision {
        ApprovalDecision::Approve => {
            let output = crate::tools::execute_approved(
                &runtime.tool_state,
                &request.tool_id,
                &request.input,
            );
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
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &ToolCallRequest,
    decision: &ApprovalDecision,
) -> ApprovalOutcome {
    match decision {
        ApprovalDecision::Approve => {
            let call_id = request.call_id.clone();
            let events = vec![
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_id.clone()),
            ];
            let frame = runtime
                .live_state
                .extend_provider_events(events.clone().into_iter().map(Into::into));

            bash::spawn(
                session_id,
                call_id,
                request.input.clone(),
                runtime.tool_state.bash_cwd_handle(),
                runtime.tool_state.bash_config(),
                runtime.bash_results.clone(),
            );

            ApprovalOutcome::Started { events, frame }
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
/// ToolCallResult` to forward to the provider. `ran` doubles as the source
/// of `ToolCallResult::denied`'s contract marker: the only reason a
/// Horizon-executed tool's approval resolves synchronously without ever
/// running is a deny (both call sites above pass `ran = false` alongside
/// `denied_output()`) — an approve always passes `ran = true`, even when
/// the tool goes on to fail for its own reasons.
fn synchronous_result(
    runtime: &SessionRuntime,
    call_id: &ToolCallId,
    output: Value,
    ran: bool,
) -> ApprovalOutcome {
    let result = if ran {
        ToolCallResult::new(call_id.clone(), output)
    } else {
        ToolCallResult::denied(call_id.clone(), output)
    };

    let mut events = Vec::new();
    if ran {
        events.push(Event::StateChanged(SessionState::ToolRunning));
        events.push(Event::ToolCallStarted(call_id.clone()));
    }
    events.push(Event::ToolCallFinished(result.clone()));
    // No `StateChanged(WaitingForUser)` here: like `execution::
    // execute_auto_tool`'s equivalent removal, this call may be only one
    // member of a batch the originating completion requested, and this
    // approve/deny path has no visibility into whether sibling calls are
    // still outstanding or a turn is in flight. The session loop owns
    // turn-level state and emits its own accurate `WaitingForUser` once the
    // batch is fully resolved.

    let frame = runtime
        .live_state
        .extend_provider_events(events.clone().into_iter().map(Into::into));

    ApprovalOutcome::Executed {
        events,
        frame,
        command: Command::ToolCallResult(result),
    }
}
