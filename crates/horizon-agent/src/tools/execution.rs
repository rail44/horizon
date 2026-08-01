use serde_json::{json, Value};

use crate::contract::{
    Error, Event, Message, MessageRole, SessionId, SessionState, ToolCallId, ToolCallRequest,
    ToolCallResult, ToolPermission,
};
use crate::policy::{
    annotate_auto_approval, boundary_disposition, classify_call, BoundaryDisposition,
    Classification,
};
use crate::tools::config;
use crate::tools::fs;
use crate::tools::recall;
use crate::tools::state::{session_runtime, ToolSessionState};
use crate::tools::{definitions, permission_for_tool};

/// Seam for tools this crate doesn't implement itself because they need
/// Horizon-side state this crate can't depend on — currently just
/// `workspace.snapshot`, which reads Horizon's `Workspace`. Horizon
/// implements this trait (see `agent::host_tools::WorkspaceHostTools`) and
/// passes it in at every call into [`execute_agent_tool`]/
/// `processing::process_agent_provider_event`, keeping the tool catalog's
/// shape otherwise unchanged: an unrecognized `tool_id` here just falls
/// through to this crate's own auto-allow tools (`tools::fs`). See
/// `docs/agent-runtime-split-design.md`'s "Tools execute in the child"
/// guardrail — this is the seam that will grow into the host-tool channel
/// once tool execution moves into `horizon-agentd`.
pub trait HostTools {
    /// Executes a host-owned auto-allow tool, returning `None` if `tool_id`
    /// isn't one this implementation handles.
    fn execute_auto(&self, tool_id: &str, input: &serde_json::Value) -> Option<serde_json::Value>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Execution {
    Auto(Vec<Event>),
    /// A call that moved to background execution (`bash` via
    /// `horizon_sandbox`, or a host-side web request) instead of finishing synchronously
    /// like [`Execution::Auto`] -- mirrors `tools::approval::
    /// ApprovalOutcome::Started`'s split for the same reason (a command can
    /// run for up to its timeout). `events` are the `ToolRunning`/
    /// `ToolCallStarted` pair already folded by the caller; the eventual
    /// result arrives later on the session's `async_results` channel exactly
    /// like a manually approved bash call's does.
    Started(Vec<Event>),
    RequiresApproval,
    Denied(Vec<Event>),
    Unknown(Vec<Event>),
}

pub fn execute_agent_tool(
    host: &dyn HostTools,
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    match permission_for_tool(&request.tool_id) {
        // `task`/`task_output` are auto-allowed like every other read tool
        // but need the requesting session's id, which `execute_auto_tool`'s
        // "produce a `Value` from input alone" shape cannot supply: a launch
        // is registered against its requester and a fetch is ownership-
        // checked against it (`tools::explore`). Both still resolve right
        // here -- a launch is non-blocking since the 2026-07-28
        // asynchronous cutover (`docs/agent-async-task-design.md`), so
        // neither takes `bash`'s `Execution::Started` path any more.
        Some(ToolPermission::AutoAllowRead) if request.tool_id == crate::tools::TASK_TOOL_ID => {
            crate::tools::explore::start(tool_state, session_id, request)
        }
        Some(ToolPermission::AutoAllowRead)
            if request.tool_id == crate::tools::TASK_OUTPUT_TOOL_ID =>
        {
            crate::tools::explore::output(session_id, request)
        }
        Some(ToolPermission::AutoAllowRead | ToolPermission::AutoAllowUi) => {
            Execution::Auto(execute_auto_tool(host, tool_state, request))
        }
        Some(ToolPermission::RequireApproval) => {
            let classification = classify_call(
                &request.tool_id,
                &request.input,
                tool_state.is_isolated_worktree(),
                horizon_sandbox::is_available(),
            );
            match classification {
                Classification::Contained => execute_tier1(tool_state, session_id, request),
                Classification::BoundaryCrossing => {
                    if boundary_disposition(tool_state, &request.tool_id, &request.input)
                        == BoundaryDisposition::Auto
                    {
                        execute_boundary_tool(tool_state, session_id, request)
                    } else {
                        Execution::RequiresApproval
                    }
                }
                Classification::AlwaysAsk => Execution::RequiresApproval,
            }
        }
        Some(ToolPermission::Deny) => Execution::Denied(vec![Event::Error(Error {
            message: format!("Tool `{}` is denied by Horizon policy.", request.tool_id),
        })]),
        // An unrecognized tool id (not in `tools::catalog::definitions`)
        // must resolve to a `ToolCallFinished` error result, not a bare
        // session `Event::Error` -- the latter is never routed back to the
        // provider as a tool outcome (see `tools::processing::
        // process_agent_provider_event`'s `Command::ToolCallResult`
        // forwarding), so the model would never see why its call failed and
        // the turn would stall waiting on a result that never arrives. The
        // available id list is included so the model can self-correct
        // (e.g. `write` -> `fs.write`) without another round trip to ask.
        None => Execution::Unknown(vec![Event::ToolCallFinished(ToolCallResult::new(
            request.call_id.clone(),
            request.occurrence_id.clone(),
            unknown_tool_output(&request.tool_id),
        ))]),
    }
}

fn execute_boundary_tool(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    if !matches!(request.tool_id.as_str(), "web_search" | "web_fetch") {
        return Execution::RequiresApproval;
    }
    let Some(runtime) = session_runtime(session_id) else {
        return Execution::Auto(vec![Event::ToolCallFinished(ToolCallResult::new(
            request.call_id.clone(),
            request.occurrence_id.clone(),
            json!({
                "is_error": true,
                "message": format!("{} has no registered session runtime", request.tool_id),
            }),
        ))]);
    };
    let events = vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
    ];
    crate::tools::web::spawn(
        session_id,
        request.call_id.clone(),
        &request.tool_id,
        request.input.0.clone(),
        tool_state.domain_allowlist(),
        crate::tools::web::WebApprovalOrigin::Auto,
        runtime.async_results,
    );
    Execution::Started(events)
}

fn unknown_tool_output(tool_id: &str) -> serde_json::Value {
    let available = definitions()
        .into_iter()
        .map(|definition| definition.id)
        .collect::<Vec<_>>()
        .join(", ");
    json!({
        "is_error": true,
        "message": format!("Unknown tool `{tool_id}`; available: {available}."),
    })
}

/// Auto-executes a tier-1-`Contained` `RequireApproval` call -- the
/// approval-skipping half of `docs/agent-approval-design.md`'s tier 1.
/// Dispatches by tool id; `classify_call` never returns `Contained` for any
/// id not handled below, but this still falls back to the ordinary approval
/// gate rather than panicking on a future mismatch between the two.
fn execute_tier1(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    match request.tool_id.as_str() {
        "fs.write" | "fs.edit" => execute_tier1_fs(tool_state, request),
        "bash" => execute_tier1_bash(tool_state, session_id, request),
        _ => Execution::RequiresApproval,
    }
}

/// `fs.write`/`fs.edit`: run to completion synchronously right now, reusing
/// the exact same execution path a manual approval would (`tools::
/// execute_approved`), just skipping the approval round trip. The audit
/// marker (tier + reason) is added to the result the same way a real
/// `ToolCallResult` gets built anywhere else in this crate.
fn execute_tier1_fs(tool_state: &ToolSessionState, request: &ToolCallRequest) -> Execution {
    let mut output = crate::tools::execute_approved(tool_state, &request.tool_id, &request.input);
    annotate_auto_approval(&mut output, "contained", "isolated worktree session");

    Execution::Auto(vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
        Event::ToolCallFinished(ToolCallResult::new(
            request.call_id.clone(),
            request.occurrence_id.clone(),
            output,
        )),
    ])
}

/// `bash`: starts a sandboxed run on the bash background thread, exactly
/// like `tools::approval::resolve_bash`'s approve path except the sandbox
/// engages (writable root = this session's isolated workspace root) and
/// nothing folds `ToolRunning`/`ToolCallStarted` here -- the caller
/// (`tools::processing::process_agent_provider_event`) folds
/// `Execution::Started`'s events itself, the same way it already folds
/// `Execution::Auto`'s. Falls back to the ordinary approval gate (never
/// silently drops the call) if this session has no registered runtime or no
/// workspace root -- both should be impossible whenever `classify_call`
/// returned `Contained`, but this stays defensive rather than panicking.
///
/// Network (`docs/agent-approval-design.md` leg 4b): when this session has
/// its own running network proxy (`tool_state.network_proxy()`), the sandbox
/// gets `NetworkPolicy::Proxied` for that exact TCP endpoint instead of
/// `NetworkPolicy::Disabled` -- see `bash::exec::run_sandboxed`'s doc
/// comment for the denial attribution this enables. `None` falls back to
/// `Disabled`.
fn execute_tier1_bash(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Execution {
    let Some(runtime) = session_runtime(session_id) else {
        return Execution::RequiresApproval;
    };
    let Some(workspace_root) = tool_state.workspace_root() else {
        return Execution::RequiresApproval;
    };
    let network = tool_state.network_proxy();

    let call_id = request.call_id.clone();

    // Same-command re-filter short-circuit (`tools/bash/recent.rs`): if this
    // command's base (the part before the first pipe) matches a prior bash
    // run whose full output was spilled to a temp file that still exists,
    // and no file-modifying tool call (`fs.write`/`fs.edit`, or a `bash`
    // call with a *different* base) has run in between, skip execution and
    // return a guidance result pointing at the existing spill file instead.
    // The model can re-filter with `fs.read`/`fs.grep` without re-running the
    // command — the "same command, different `| tail`/`| grep` filter" pattern
    // that burned ~1.2M input tokens in a single session
    // (`docs/research/agent-editing-phase-analysis-2026-07-28.md`).
    //
    // This runs after `classify_call` returned `Contained` (the sandbox
    // verdict, line 78-83) and before `spawn_sandboxed`, so neither the
    // sandbox nor the approval gate is affected. A stale or missing spill
    // file falls through to a real run, and an intervening modification
    // blocks the short-circuit so genuinely changed code always gets a
    // fresh execution.
    if let Some(command) = request.input.get("command").and_then(Value::as_str) {
        let frame = runtime.live_state.frame();
        if let Some(prior) = crate::tools::bash::find_reusable_output(&frame, command) {
            let mut output = crate::tools::bash::guidance_output(command, &prior);
            annotate_auto_approval(&mut output, "contained", "isolated worktree session");
            return Execution::Auto(vec![
                Event::StateChanged(SessionState::ToolRunning),
                Event::ToolCallStarted(call_id.clone()),
                Event::ToolCallFinished(ToolCallResult::new(
                    call_id,
                    request.occurrence_id.clone(),
                    output,
                )),
            ]);
        }
    }

    let events = vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(call_id.clone()),
    ];

    crate::tools::bash::spawn_sandboxed(
        session_id,
        call_id,
        request.input.0.clone(),
        tool_state.bash_cwd_handle(),
        tool_state.bash_config(),
        workspace_root.to_path_buf(),
        network,
        crate::tools::bash::SandboxedApprovalOrigin::Tier1Auto,
        tool_state.filesystem_grants_snapshot(),
        None,
        runtime.async_results.clone(),
    );

    Execution::Started(events)
}

fn execute_auto_tool(
    host: &dyn HostTools,
    tool_state: &ToolSessionState,
    request: &ToolCallRequest,
) -> Vec<Event> {
    let output = host
        .execute_auto(&request.tool_id, &request.input)
        .or_else(|| fs::execute_auto(tool_state, &request.tool_id, &request.input))
        .or_else(|| config::execute_auto(tool_state, &request.tool_id, &request.input))
        .or_else(|| recall::execute_auto(tool_state, &request.tool_id, &request.input));
    let output = match output {
        Some(output) => output,
        None => {
            return vec![Event::Error(Error {
                message: format!(
                    "Tool `{}` cannot be executed automatically.",
                    request.tool_id
                ),
            })]
        }
    };

    vec![
        Event::StateChanged(SessionState::ToolRunning),
        Event::ToolCallStarted(request.call_id.clone()),
        Event::ToolCallFinished(ToolCallResult::new(
            request.call_id.clone(),
            request.occurrence_id.clone(),
            output,
        )),
        // No `StateChanged(WaitingForUser)` here: this call is only one
        // member of whatever batch the originating completion requested (a
        // single completion can request several parallel tool calls — see
        // `providers::rig::session::fold_batched_tool_result`), and this
        // executor has no visibility into whether sibling calls are still
        // outstanding or a turn is still in flight. The session loop owns
        // turn-level state and already emits its own accurate
        // `WaitingForUser` once the whole batch has resolved and no
        // follow-up turn is running; emitting it here too, per call, raced
        // ahead of that (see the production incident this fix responds to)
        // and could flip `AgentFrame::is_turn_in_flight` to `false` —
        // disabling Cancel — while more results are still outstanding.
    ]
}

pub(crate) fn tool_result_message(result: &ToolCallResult) -> Event {
    Event::MessageCommitted(Message {
        role: MessageRole::Assistant,
        text: format!("Tool result received for {}.", result.call_id.0),
    })
}

/// A synthetic tool result marking a pending tool call as cancelled, so a
/// pending approval belonging to a cancelled turn resolves to a terminal
/// (non-error) outcome instead of hanging forever. `occurrence_id` is `None`
/// because a cancellation is a session-wide event with no per-occurrence
/// reference available at the call site -- and unlike the asynchronous
/// executors' results, this one never passes through the agentd's
/// `fold_finished_bash_result` (the session loop synthesizes it directly),
/// so nothing stamps it later either. Consumers fall back to call_id
/// matching for `None`, the same legacy path replayed pre-feature logs
/// already take.
pub fn cancelled_tool_call_result(call_id: ToolCallId) -> ToolCallResult {
    ToolCallResult::new(call_id, None, json!({ "cancelled": true }))
}
