use super::completion::approval_is_unresolved;
use super::input::PreparedCall;
use super::transition::ToolUpdate;

use crate::contract::SessionId;
use crate::contract::{
    ApprovalKind, Command, ToolCallId, ToolCallIdentity, ToolCallRequest, ToolCallResult,
};
use crate::frame::AgentFrame;
use crate::judge::ApprovalCandidate;
use crate::tools::bash;
use crate::tools::bash::{ApprovalSource, HostExecutionApproval};
use crate::tools::output::{Response, error as error_output};
use crate::tools::state::{SessionRuntime, ToolSessionState, session_runtime};

/// The user's decision on a pending `ApprovalRequested` tool call.
#[derive(Clone, Debug)]
pub enum ApprovalDecision {
    Approve,
    Deny { reason: Option<String> },
}

/// The coordinator publishes applied updates or forwards provider-owned
/// approvals. Duplicate decisions never execute or return a second result.
#[derive(Debug)]
pub enum ApprovalOutcome {
    Applied(ToolUpdate),
    PersistenceFailed(String),
    /// A validated approval for a provider-owned tool.
    Forward(Command),
    /// Missing, stale, already accepted, running, or completed approval.
    AlreadyResolved,
}

impl From<Result<ToolUpdate, String>> for ApprovalOutcome {
    fn from(update: Result<ToolUpdate, String>) -> Self {
        match update {
            Ok(update) => Self::Applied(update),
            Err(error) => Self::PersistenceFailed(error),
        }
    }
}

/// Tool ids Horizon executes itself once approved, rather than notifying
/// the provider via `ApproveToolCall`/`DenyToolCall` and waiting for it to
/// report a result. See `docs/agent-tools-design.md`, "Approval Wiring".
/// `fs_read`/`fs_glob`/`fs_grep` join here because an out-of-workspace-root
/// read routes through the approval gate (see `execution::execute_agent_tool`'s
/// `call_escapes_root` check) -- all
/// are "runs to completion synchronously" shape -- see
/// [`resolve_synchronous_tool`].
pub(crate) fn is_horizon_executed_tool(tool_id: &str) -> bool {
    matches!(
        tool_id,
        "fs_read" | "fs_glob" | "fs_grep" | "fs_write" | "fs_edit" | "bash" | "web_fetch"
    )
}

/// Resolve only the displayed, still-pending execution occurrence.
pub fn resolve_approval(
    frame: &AgentFrame,
    session_id: SessionId,
    identity: ToolCallIdentity,
    decision: ApprovalDecision,
) -> ApprovalOutcome {
    let Some(approval) = frame.actionable_approval(&identity) else {
        return ApprovalOutcome::AlreadyResolved;
    };
    let request = frame
        .tool_call_request(&identity.call_id)
        .expect("validated approval request");
    if !approval_is_unresolved(frame, request) {
        return ApprovalOutcome::AlreadyResolved;
    }
    super::background::cancel_judgment(session_id, &request.identity());
    if let Some(runtime) = session_runtime(session_id)
        && let Some(tool) = super::tool::find(runtime.tool_state.packages(), &request.tool_id)
        && tool.resolves_approvals()
    {
        return dispatch_approval(
            session_id,
            &runtime,
            request,
            &decision,
            approval.kind.clone(),
            ApprovalSource::Human,
        );
    }
    ApprovalOutcome::Forward(match decision {
        ApprovalDecision::Approve => Command::ApproveToolCall { identity },
        ApprovalDecision::Deny { reason } => Command::DenyToolCall { identity, reason },
    })
}

/// Resolves a judge-approved candidate through the same execution/retry path
/// as a human approval, without requiring a synthetic `ApprovalRequested`
/// event in the frame. The original request must still be the live,
/// unresolved occurrence; stale completions are ignored.
pub fn resolve_auto_approval(
    frame: &AgentFrame,
    session_id: SessionId,
    candidate: &ApprovalCandidate,
) -> ApprovalOutcome {
    let call_id = &candidate.request.call_id;
    let Some(request) = frame.tool_call_request(call_id) else {
        return ApprovalOutcome::AlreadyResolved;
    };
    if candidate.approval.identity() != candidate.request.identity()
        || frame
            .actionable_approval(&candidate.request.identity())
            .is_some()
        || !approval_is_unresolved(frame, &candidate.request)
    {
        return ApprovalOutcome::AlreadyResolved;
    }
    let Some(runtime) = session_runtime(session_id) else {
        return ApprovalOutcome::Forward(Command::ApproveToolCall {
            identity: request.identity(),
        });
    };
    if !super::tool::find(runtime.tool_state.packages(), &request.tool_id)
        .is_some_and(|tool| tool.resolves_approvals())
    {
        return ApprovalOutcome::Forward(Command::ApproveToolCall {
            identity: request.identity(),
        });
    }

    dispatch_approval(
        session_id,
        &runtime,
        request,
        &ApprovalDecision::Approve,
        candidate.approval.kind.clone(),
        ApprovalSource::Judge,
    )
}

/// The result a tool call resolves to when it would otherwise wait for a
/// human in a session that has none (`ToolSessionState::is_unattended`):
/// the call does not run, and the model is told what it may reach instead.
/// `None` in an attended session, whose approval path is unchanged.
///
/// This is the pre-fold shape, for a call whose `ToolCallRequested` has not
/// been folded into the session's live frame yet — the host's
/// synchronous approval gate. [`refuse_unattended`] is the same refusal for
/// a request already in the frame.
pub fn unattended_refusal_result(
    tool_state: &ToolSessionState,
    request: &ToolCallRequest,
) -> Option<ToolCallResult> {
    let message = unattended_refusal_message(tool_state, request)?;
    Some(request.identity().finish(error_output(message)))
}

/// [`unattended_refusal_result`] folded into the session's live frame and
/// paired with the provider command, for a candidate already recorded there
/// — what an approval left to a human resolves to in an unattended session.
/// A candidate offering to retry an attempt that already ran resolves to
/// that attempt's result instead, as a deny would, with a note that the
/// retry needed an approval nobody can give: the model keeps what the
/// command printed. `None` when the session is attended or has no
/// registered runtime.
pub fn refuse_unattended(
    session_id: SessionId,
    candidate: &ApprovalCandidate,
) -> Option<ApprovalOutcome> {
    let runtime = session_runtime(session_id)?;
    if !runtime.tool_state.is_unattended() {
        return None;
    }
    if let Some(prior) = candidate.approval.kind.prior_result() {
        let mut prior = prior.clone();
        if let Some(output) = prior.output.as_object_mut() {
            output.insert(
                "retry_not_approved".into(),
                serde_json::Value::String(format!(
                    "Retrying this call needed an approval ({}), and this session has nobody \
                     who can give it; this is the result of the attempt that ran.",
                    candidate.approval.reason
                )),
            );
        }
        return Some(forward_prior_result(&runtime, prior));
    }
    let result = unattended_refusal_result(&runtime.tool_state, &candidate.request)?;
    Some(ApprovalOutcome::from(ToolUpdate::finish(
        &runtime.live_state,
        result,
    )))
}

fn unattended_refusal_message(
    tool_state: &ToolSessionState,
    request: &ToolCallRequest,
) -> Option<String> {
    if !tool_state.is_unattended() {
        return None;
    }
    Some(
        crate::tools::out_of_root_refusal(tool_state, &request.tool_id, &request.input)
            .unwrap_or_else(|| {
                format!(
                    "`{}` needs approval and this session has nobody who can give it; the call \
                     was not run.",
                    request.tool_id
                )
            }),
    )
}

fn dispatch_approval(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &ToolCallRequest,
    decision: &ApprovalDecision,
    kind: ApprovalKind,
    source: ApprovalSource,
) -> ApprovalOutcome {
    let prepared = match PreparedCall::new(request) {
        Ok(prepared) => prepared,
        Err(message) => return unstarted_error(runtime, &request.call_id, &message),
    };
    let request = &prepared;
    match request.tool_id.as_str() {
        "bash" => resolve_bash(session_id, runtime, request, decision, kind, source),
        "web_fetch" => resolve_web_fetch(session_id, runtime, request, decision, kind),
        tool_id
            if crate::tools::session_tool::is_session_tool(
                runtime.tool_state.packages(),
                tool_id,
            ) =>
        {
            resolve_session_tool(session_id, runtime, request, decision)
        }
        _ => resolve_synchronous_tool(runtime, request, decision),
    }
}

/// A tool the session's client handed it: a deny answers at once; an
/// approve starts the call, whose result comes later as an automatic one's
/// does.
fn resolve_session_tool(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
) -> ApprovalOutcome {
    if matches!(decision, ApprovalDecision::Deny { .. }) {
        return declined_result(runtime, &request.call_id, denied_output(decision));
    }
    let outcome = begin_execution(runtime, request, None);
    if matches!(outcome, ApprovalOutcome::PersistenceFailed(_)) {
        return outcome;
    }
    crate::tools::session_tool::spawn_approved(
        session_id,
        runtime.tool_state.packages(),
        request,
        runtime.async_results.clone(),
    );
    outcome
}

fn resolve_web_fetch(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
    kind: ApprovalKind,
) -> ApprovalOutcome {
    if matches!(decision, ApprovalDecision::Deny { .. }) {
        crate::tools::web::clear_approved_domains(session_id, &request.identity());
        return declined_result(runtime, &request.call_id, denied_output(decision));
    }
    let ApprovalKind::DomainGrant { domains } = kind else {
        crate::tools::web::clear_approved_domains(session_id, &request.identity());
        return declined_result(
            runtime,
            &request.call_id,
            error_output("web_fetch approval did not carry a supported domain grant"),
        );
    };
    if domains.is_empty() {
        crate::tools::web::clear_approved_domains(session_id, &request.identity());
        return declined_result(
            runtime,
            &request.call_id,
            error_output("web_fetch domain grant was empty"),
        );
    }
    let validated = domains
        .iter()
        .map(|domain| crate::tools::web::validate_domain_grant(domain))
        .collect::<Result<Vec<_>, _>>();
    let Ok(validated) = validated else {
        crate::tools::web::clear_approved_domains(session_id, &request.identity());
        return declined_result(
            runtime,
            &request.call_id,
            error_output("web_fetch domain grant failed revalidation"),
        );
    };
    let outcome = begin_execution(runtime, request, None);
    if matches!(outcome, ApprovalOutcome::PersistenceFailed(_)) {
        return outcome;
    }
    for domain in &validated {
        runtime.tool_state.allow_domain(domain.clone());
    }
    let approved_domains =
        crate::tools::web::record_approved_domains(session_id, &request.identity(), &validated);

    crate::tools::web::spawn(
        session_id,
        request,
        runtime.tool_state.domain_allowlist(),
        crate::tools::web::WebApprovalOrigin::ManualDomainGrant {
            domains: approved_domains,
        },
        runtime.async_results.clone(),
    );
    outcome
}

/// `fs_write`/`fs_edit` and the approved-out-of-root read
/// tools (`fs_read`/`fs_glob`/`fs_grep`): all run to completion synchronously,
/// so their approve/deny always resolves to `Executed`. Dispatches through
/// `tools::execute_approved`, which picks the owning module by tool id.
fn resolve_synchronous_tool(
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
) -> ApprovalOutcome {
    match decision {
        ApprovalDecision::Approve => {
            if matches!(request.tool_id.as_str(), "fs_write" | "fs_edit") {
                runtime.tool_state.approve_primary_checkout();
            }
            ApprovalOutcome::from(ToolUpdate::execute(&runtime.live_state, request, || {
                crate::tools::execute_approved(&runtime.tool_state, &request.input)
            }))
        }
        ApprovalDecision::Deny { .. } => {
            declined_result(runtime, &request.call_id, denied_output(decision))
        }
    }
}

/// `bash`: a deny short-circuits synchronously exactly like the fs tools,
/// but an approve only *starts* the command — see `ToolUpdate::Started`.
/// Domain-denial retries and filesystem-denial retries keep the rerun
/// sandboxed — an approval buys scoped, contained access, never an
/// unconfined execution. Only [`ApprovalKind::Standard`] still runs on the
/// host, which is what that kind has always meant.
fn resolve_bash(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
    kind: ApprovalKind,
    approval_source: ApprovalSource,
) -> ApprovalOutcome {
    match kind {
        ApprovalKind::DomainGrant { domains } => resolve_bash_domains(
            session_id,
            runtime,
            request,
            decision,
            domains,
            approval_source,
        ),
        ApprovalKind::Standard => {
            resolve_standard_bash(session_id, runtime, request, decision, approval_source)
        }
    }
}

/// A sandboxed `bash` call that named hosts beyond the session's domains
/// (`docs/sandbox-runtime-design.md`, "The failure path"). An approve runs
/// it sandboxed with those hosts open for this call only: nothing is added
/// to the session.
fn resolve_bash_domains(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
    domains: Vec<String>,
    approval_source: ApprovalSource,
) -> ApprovalOutcome {
    if matches!(decision, ApprovalDecision::Deny { .. }) {
        return declined_result(runtime, &request.call_id, denied_output(decision));
    }
    let Some(workspace_root) = runtime.tool_state.workspace_root() else {
        return unstarted_error(runtime, &request.call_id, "bash requires a workspace root");
    };
    let outcome = begin_execution(runtime, request, None);
    if matches!(outcome, ApprovalOutcome::PersistenceFailed(_)) {
        return outcome;
    }
    bash::spawn_contained(
        session_id,
        request,
        &runtime.tool_state,
        runtime.async_results.clone(),
        workspace_root,
        bash::ContainedOrigin::Domains {
            source: approval_source,
            domains,
        },
    );
    outcome
}

fn resolve_standard_bash(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
    approval_source: ApprovalSource,
) -> ApprovalOutcome {
    match decision {
        ApprovalDecision::Approve => {
            let outcome = begin_execution(runtime, request, None);
            if matches!(outcome, ApprovalOutcome::PersistenceFailed(_)) {
                return outcome;
            }
            bash::spawn_approved_host(
                bash::BashJob::new(
                    session_id,
                    request,
                    &runtime.tool_state,
                    runtime.async_results.clone(),
                ),
                HostExecutionApproval::new(approval_source),
            );

            outcome
        }
        ApprovalDecision::Deny { .. } => {
            declined_result(runtime, &request.call_id, denied_output(decision))
        }
    }
}

fn begin_execution(
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    prior_result: Option<&ToolCallResult>,
) -> ApprovalOutcome {
    ApprovalOutcome::from(ToolUpdate::start(
        &runtime.live_state,
        request,
        prior_result,
    ))
}

/// Settle a retry offer that was declined (the counterpart of
/// [`ToolCallResult::superseded_by_retry`] for the approve half): the parked first
/// attempt's genuine outcome becomes that occurrence's terminal result and
/// the provider's answer, since the attempt that ran is what the decision
/// settled on. Unchanged by backlog 55 -- it was already the path that
/// closed the first row.
fn forward_prior_result(runtime: &SessionRuntime, prior_result: ToolCallResult) -> ApprovalOutcome {
    ApprovalOutcome::from(ToolUpdate::decline_retry(&runtime.live_state, prior_result))
}

fn unstarted_error(
    runtime: &SessionRuntime,
    call_id: &ToolCallId,
    message: &str,
) -> ApprovalOutcome {
    // Look up the originating `ToolCallRequest`'s `occurrence_id` so the
    // readers that match `ToolCallFinished` by `occurrence_id` (the frame's
    // `tool_call_occurrences`, the shell's per-call views) attribute this denial
    // to the right occurrence of the (possibly reused) `call_id`. The frame
    // already has the request -- `unstarted_error` is only called when the
    // approval gate decided to skip the start, which it could only do
    // because the request is sitting in the frame.
    let Some(identity) = runtime
        .live_state
        .frame()
        .tool_call_request(call_id)
        .map(ToolCallRequest::identity)
    else {
        return ApprovalOutcome::AlreadyResolved;
    };
    let result = identity.finish(error_output(message));
    forward_prior_result(runtime, result)
}

/// A person's denial of a call that did not run, with the reason they gave
/// when they gave one: the model reads it as the call's result.
fn denied_output(decision: &ApprovalDecision) -> Response {
    match decision {
        ApprovalDecision::Deny {
            reason: Some(reason),
        } if !reason.trim().is_empty() => {
            error_output(format!("denied by user: {}", reason.trim()))
        }
        _ => error_output("denied by user"),
    }
}

fn declined_result(
    runtime: &SessionRuntime,
    call_id: &ToolCallId,
    output: Response,
) -> ApprovalOutcome {
    // Same `occurrence_id` fixup as `unstarted_error` -- look up the
    // request's `occurrence_id` from the live frame so the transcript and
    // analytics attribute this result to the right occurrence of a
    // possibly-reused `call_id`.
    let Some(identity) = runtime
        .live_state
        .frame()
        .tool_call_request(call_id)
        .map(ToolCallRequest::identity)
    else {
        return ApprovalOutcome::AlreadyResolved;
    };
    let result = ToolCallResult::denied(call_id.clone(), identity.occurrence_id, output.to_json());
    ApprovalOutcome::from(ToolUpdate::finish(&runtime.live_state, result))
}
