use super::completion::approval_is_unresolved;
use super::input::PreparedCall;
use super::transition::ToolUpdate;
use serde_json::Value;

use crate::contract::SessionId;
use crate::contract::{
    ApprovalKind, Command, ToolCallId, ToolCallIdentity, ToolCallRequest, ToolCallResult,
};
use crate::frame::AgentFrame;
use crate::judge::ApprovalCandidate;
use crate::tools::bash;
use crate::tools::bash::{ApprovalSource, HostExecutionApproval, SandboxedApprovalOrigin};
use crate::tools::error_output;
use crate::tools::state::{session_runtime, SessionRuntime, ToolSessionState};

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
/// `fs.read`/`fs.glob`/`fs.grep` join here because an out-of-workspace-root
/// read routes through the approval gate (see `execution::execute_agent_tool`'s
/// `call_escapes_root` check); `config.write` joins for the same reason -- all
/// are "runs to completion synchronously" shape -- see
/// [`resolve_synchronous_tool`].
fn is_horizon_executed_tool(tool_id: &str) -> bool {
    matches!(
        tool_id,
        "fs.read"
            | "fs.glob"
            | "fs.grep"
            | "fs.write"
            | "fs.edit"
            | "bash"
            | "config.write"
            | "web_fetch"
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
    if is_horizon_executed_tool(&request.tool_id) {
        if let Some(runtime) = session_runtime(session_id) {
            return dispatch_approval(
                session_id,
                &runtime,
                request,
                &decision,
                approval.kind.clone(),
                ApprovalSource::Human,
            );
        }
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
    if !is_horizon_executed_tool(&request.tool_id) {
        return ApprovalOutcome::Forward(Command::ApproveToolCall {
            identity: request.identity(),
        });
    }
    let Some(runtime) = session_runtime(session_id) else {
        return ApprovalOutcome::Forward(Command::ApproveToolCall {
            identity: request.identity(),
        });
    };

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
/// been folded into the session's live frame yet — the daemon's
/// synchronous approval gate. [`refuse_unattended`] is the same refusal for
/// a request already in the frame.
pub fn unattended_refusal_result(
    tool_state: &ToolSessionState,
    request: &ToolCallRequest,
) -> Option<ToolCallResult> {
    let message = unattended_refusal_message(tool_state, request)?;
    Some(ToolCallResult::new(
        request.call_id.clone(),
        request.occurrence_id.clone(),
        error_output(message),
    ))
}

/// [`unattended_refusal_result`] folded into the session's live frame and
/// paired with the provider command, for a request already recorded there
/// — what the judge's escalation verdict resolves to in an unattended
/// session. `None` when the session is attended or has no registered
/// runtime.
pub fn refuse_unattended(
    session_id: SessionId,
    request: &ToolCallRequest,
) -> Option<ApprovalOutcome> {
    let runtime = session_runtime(session_id)?;
    let result = unattended_refusal_result(&runtime.tool_state, request)?;
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
        _ => resolve_synchronous_tool(runtime, request, decision),
    }
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
        return declined_result(runtime, &request.call_id, denied_output());
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

/// `fs.write`/`fs.edit`/`config.write` and the approved-out-of-root read
/// tools (`fs.read`/`fs.glob`/`fs.grep`): all run to completion synchronously,
/// so their approve/deny always resolves to `Executed`. Dispatches through
/// `tools::execute_approved`, which picks the owning module by tool id.
fn resolve_synchronous_tool(
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
) -> ApprovalOutcome {
    match decision {
        ApprovalDecision::Approve => {
            ApprovalOutcome::from(ToolUpdate::execute(&runtime.live_state, request, || {
                crate::tools::execute_approved(&runtime.tool_state, &request.input)
            }))
        }
        ApprovalDecision::Deny { .. } => {
            declined_result(runtime, &request.call_id, denied_output())
        }
    }
}

/// `bash`: a deny short-circuits synchronously exactly like the fs tools,
/// but an approve only *starts* the command — see `ToolUpdate::Started`. Domain-denial retries, filesystem-denial retries, and
/// pre-execution [`ApprovalKind::GitOperation`] grants all keep the rerun
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
        ApprovalKind::DomainDenialRetry {
            domains,
            prior_result,
        } => resolve_domain_denial_retry(
            session_id,
            runtime,
            request,
            decision,
            domains,
            prior_result,
        ),
        ApprovalKind::FilesystemDenialRetry {
            denials,
            grants,
            prior_result,
        } => resolve_filesystem_denial_retry(
            session_id,
            runtime,
            request,
            decision,
            denials,
            grants,
            prior_result,
            approval_source,
        ),
        ApprovalKind::MachServiceGrant {
            services,
            prior_result,
        } => resolve_mach_service_grant(
            session_id,
            runtime,
            request,
            decision,
            services,
            prior_result,
        ),
        ApprovalKind::GitOperation { writable_roots } => {
            resolve_git_operation(session_id, runtime, request, decision, writable_roots)
        }
        ApprovalKind::DomainGrant { .. } => declined_result(
            runtime,
            &request.call_id,
            error_output("A host-side domain grant cannot authorize a bash command."),
        ),
        ApprovalKind::Standard => {
            resolve_standard_bash(session_id, runtime, request, decision, approval_source)
        }
    }
}

fn resolve_git_operation(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
    writable_roots: Vec<std::path::PathBuf>,
) -> ApprovalOutcome {
    if matches!(decision, ApprovalDecision::Deny { .. }) {
        return declined_result(runtime, &request.call_id, denied_output());
    }
    if !request.input.requires_metadata_write() {
        return unstarted_error(
            runtime,
            &request.call_id,
            "Git operation approval no longer matches a metadata-writing Git command",
        );
    }
    let Some(workspace_root) = runtime.tool_state.workspace_root() else {
        return unstarted_error(
            runtime,
            &request.call_id,
            "Git operation approval has no isolated workspace root",
        );
    };
    match bash::metadata_writable_roots(workspace_root) {
        Ok(current) if !writable_roots.is_empty() && current == writable_roots => {}
        Ok(_) => {
            return unstarted_error(
                runtime,
                &request.call_id,
                "Git metadata roots changed after the approval was displayed",
            )
        }
        Err(error) => {
            return unstarted_error(
                runtime,
                &request.call_id,
                &format!("Git metadata roots could not be revalidated: {error}"),
            )
        }
    }

    let outcome = begin_execution(runtime, request, None);
    if matches!(outcome, ApprovalOutcome::PersistenceFailed(_)) {
        return outcome;
    }
    bash::spawn_sandboxed(
        bash::BashJob::new(
            session_id,
            request,
            &runtime.tool_state,
            runtime.async_results.clone(),
        ),
        bash::SandboxedRun::new(
            &runtime.tool_state,
            workspace_root,
            SandboxedApprovalOrigin::ManualGitOperation,
            Some(writable_roots),
        ),
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
            declined_result(runtime, &request.call_id, denied_output())
        }
    }
}

/// A sandboxed `bash` call was refused paths outside its workspace
/// (`docs/containment-denial-narrow-grants-design.md`'s 2026-07-26
/// decision). Like a domain-denial retry, the call already ran to
/// completion, so a deny simply forwards `prior_result` -- there is nothing
/// left to execute.
///
/// An approve adds `grants` (the shaped suggestion; see
/// [`ApprovalKind::FilesystemDenialRetry`]) to this session and reruns the
/// same call **still sandboxed**. This replaced the 2026-07-24 answer of
/// running the call once with the host process's full authority: for the
/// case that actually drove approvals -- a build toolchain reaching its
/// caches -- the narrow grant converges while whole-call host execution
/// bought strictly more authority than the grant it declined to make.
///
/// Fails closed both ways: an approval carrying no grants (an old
/// host-execution-era request replayed against this build) and one whose
/// grants no longer revalidate both forward the prior result rather than
/// running anything.
#[allow(clippy::too_many_arguments)]
fn resolve_filesystem_denial_retry(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
    denials: Vec<horizon_sandbox::FilesystemDenial>,
    grants: Vec<horizon_sandbox::FilesystemGrant>,
    prior_result: ToolCallResult,
    approval_source: ApprovalSource,
) -> ApprovalOutcome {
    if matches!(decision, ApprovalDecision::Deny { .. }) {
        return forward_prior_result(runtime, prior_result);
    }
    if grants.is_empty() {
        return unstarted_error(
            runtime,
            &request.call_id,
            "This filesystem approval names no grant to retry with; refusing to run the call \
             without one.",
        );
    }
    // Re-resolved here, at approval application, and again by the sandbox
    // itself immediately before the queued process spawns.
    if let Err(error) = grants
        .iter()
        .try_for_each(horizon_sandbox::revalidate_grant)
    {
        return unstarted_error(
            runtime,
            &request.call_id,
            &format!("Approved filesystem grants could not be revalidated: {error}"),
        );
    }
    let Some(workspace_root) = runtime.tool_state.workspace_root() else {
        return forward_prior_result(runtime, prior_result);
    };
    // The saved start describes the intended authority. Installing it in
    // the live session still waits for that record's acknowledgement.
    let mut planned_grants = runtime.tool_state.filesystem_grants_snapshot();
    for grant in &grants {
        if !planned_grants.contains(grant) {
            planned_grants.push(grant.clone());
        }
    }
    runtime.live_state.record_filesystem_grants(&planned_grants);
    let started = match ToolUpdate::start(&runtime.live_state, request, Some(&prior_result)) {
        Ok(started) => started,
        Err(message) => return ApprovalOutcome::PersistenceFailed(message),
    };
    if let Err(error) = runtime.tool_state.approve_filesystem_grants(&grants) {
        return ApprovalOutcome::from(started.complete(
            &runtime.live_state,
            request.identity().result(error_output(format!(
                "Approved filesystem grants could not be revalidated: {error}"
            ))),
            Vec::new(),
        ));
    }
    runtime
        .live_state
        .record_filesystem_grants(&runtime.tool_state.filesystem_grants_snapshot());
    bash::spawn_sandboxed(
        bash::BashJob::new(
            session_id,
            request,
            &runtime.tool_state,
            runtime.async_results.clone(),
        ),
        bash::SandboxedRun::new(
            &runtime.tool_state,
            workspace_root,
            SandboxedApprovalOrigin::FilesystemGrant {
                source: approval_source,
                grants,
                trigger_paths: denials
                    .iter()
                    .map(|denial| denial.attempted_path.clone())
                    .collect(),
            },
            None,
        ),
    );
    ApprovalOutcome::Applied(started)
}

/// Fold the start before enqueueing a job. A retry closes its abandoned
/// attempt first, preserving the old and new occurrence identities.
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

/// The deny half of the denial-retry pair (see
/// [`ToolCallResult::superseded_by_retry`] for the approve half): the parked first
/// attempt's genuine outcome becomes that occurrence's terminal result and
/// the provider's answer, since the attempt that ran is what the decision
/// settled on. Unchanged by backlog 55 -- it was already the path that
/// closed the first row.
fn forward_prior_result(runtime: &SessionRuntime, prior_result: ToolCallResult) -> ApprovalOutcome {
    ApprovalOutcome::from(ToolUpdate::decline_retry(&runtime.live_state, prior_result))
}

/// A tier-1 sandboxed `bash` call's network egress was refused for
/// `domains` (`docs/agent-approval-design.md` leg 4b). Unlike an ordinary
/// bash approval, the call already ran to completion -- `prior_result` is a
/// genuine, already-computed outcome, not just a denial-shaped reason
/// string -- so a deny simply forwards it as-is (the real attempt already
/// happened and its own output already reflects the denial; there is
/// nothing to execute now, unlike a fresh tool-call deny). An approve adds
/// `domains` to this session's own network-proxy allowlist and reruns the
/// SAME call, still sandboxed (`bash::spawn_sandboxed`, not the plain
/// unsandboxed `bash::spawn` an ordinary/sandbox-denial-retry approve
/// uses) -- both the network mutation and the rerun are session-scoped:
/// only this call's own session's `SessionNetworkProxy` and workspace root
/// are touched.
fn resolve_domain_denial_retry(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
    domains: Vec<String>,
    prior_result: ToolCallResult,
) -> ApprovalOutcome {
    let git_metadata_roots = bash::approved_metadata_roots(&prior_result.output);
    match decision {
        ApprovalDecision::Deny { .. } => forward_prior_result(runtime, prior_result),
        ApprovalDecision::Approve => {
            // Both should be impossible here -- a `DomainDenialRetry` is
            // only ever produced by a tier-1 sandboxed call, which requires
            // both -- but this stays defensive (forwarding the prior,
            // already-computed result) rather than silently dropping the
            // approval if either is somehow missing.
            let (Some(network), Some(workspace_root)) = (
                runtime.tool_state.network_proxy(),
                runtime.tool_state.workspace_root(),
            ) else {
                return forward_prior_result(runtime, prior_result);
            };
            let outcome = begin_execution(runtime, request, Some(&prior_result));
            if matches!(outcome, ApprovalOutcome::PersistenceFailed(_)) {
                return outcome;
            }

            for domain in &domains {
                network.allow_domain(domain.clone());
            }

            bash::spawn_sandboxed(
                bash::BashJob::new(
                    session_id,
                    request,
                    &runtime.tool_state,
                    runtime.async_results.clone(),
                ),
                bash::SandboxedRun::new(
                    &runtime.tool_state,
                    workspace_root,
                    SandboxedApprovalOrigin::ManualDomainRetry { domains },
                    git_metadata_roots,
                ),
            );

            outcome
        }
    }
}

/// A sandboxed `bash` call was refused mach-lookup to macOS security
/// services (`docs/macos-containment-denial-reporting-design.md`) -- the
/// macOS counterpart of [`resolve_domain_denial_retry`]: the call already
/// ran, a deny forwards `prior_result` unchanged, and an approve records
/// the service set for this session (additive, session-persistent) and
/// reruns the SAME call still sandboxed. The rerun's grant assembly picks
/// up the enforcement mapping via
/// [`ToolSessionState::effective_sandbox_grants`]
/// (`horizon_sandbox::security_service_grants`).
fn resolve_mach_service_grant(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &PreparedCall<'_>,
    decision: &ApprovalDecision,
    services: Vec<String>,
    prior_result: ToolCallResult,
) -> ApprovalOutcome {
    let git_metadata_roots = bash::approved_metadata_roots(&prior_result.output);
    match decision {
        ApprovalDecision::Deny { .. } => forward_prior_result(runtime, prior_result),
        ApprovalDecision::Approve => {
            // Both should be impossible here -- a mach service grant is
            // only ever produced by a tier-1 sandboxed call, which requires
            // a workspace root -- but this stays defensive (forwarding the
            // prior, already-computed result), mirroring
            // [`resolve_domain_denial_retry`].
            let Some(workspace_root) = runtime.tool_state.workspace_root() else {
                return forward_prior_result(runtime, prior_result);
            };
            let outcome = begin_execution(runtime, request, Some(&prior_result));
            if matches!(outcome, ApprovalOutcome::PersistenceFailed(_)) {
                return outcome;
            }

            runtime.tool_state.approve_mach_services(&services);

            bash::spawn_sandboxed(
                bash::BashJob::new(
                    session_id,
                    request,
                    &runtime.tool_state,
                    runtime.async_results.clone(),
                ),
                bash::SandboxedRun::new(
                    &runtime.tool_state,
                    workspace_root,
                    SandboxedApprovalOrigin::MachServiceGrant { services },
                    git_metadata_roots,
                ),
            );

            outcome
        }
    }
}

fn unstarted_error(
    runtime: &SessionRuntime,
    call_id: &ToolCallId,
    message: &str,
) -> ApprovalOutcome {
    // Look up the originating `ToolCallRequest`'s `occurrence_id` so the
    // transcript's `build_tool_call_views` (which matches
    // `ToolCallFinished` by `occurrence_id` first) attributes this denial
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
    let result = identity.result(error_output(message));
    forward_prior_result(runtime, result)
}

fn denied_output() -> Value {
    error_output("denied by user")
}

/// Settle an offer that did not execute, including denied or invalid grants.
fn declined_result(
    runtime: &SessionRuntime,
    call_id: &ToolCallId,
    output: Value,
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
    let result = ToolCallResult::denied(call_id.clone(), identity.occurrence_id, output);
    ApprovalOutcome::from(ToolUpdate::finish(&runtime.live_state, result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{ApprovalRequest, Event, OccurrenceId};
    use crate::live::LiveState;
    use crate::persistence::event_log::{WriterHandle, WriterInit};
    use crate::tools::{register_session_runtime, unregister_session_runtime};

    #[test]
    fn failed_approved_starts_do_not_enlarge_session_grants() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let prior = crate::contract::ToolCallIdentity {
            call_id: ToolCallId("call".into()),
            occurrence_id: OccurrenceId::new(),
        }
        .result(serde_json::json!({"is_error":true}));
        let grant = horizon_sandbox::FilesystemGrant {
            path: outside.canonicalize().unwrap(),
            access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
            scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
            excluded_subpaths: Vec::new(),
        };
        for (tool_id, kind) in [
            (
                "web_fetch",
                ApprovalKind::DomainGrant {
                    domains: vec!["example.com".into()],
                },
            ),
            (
                "bash",
                ApprovalKind::FilesystemDenialRetry {
                    denials: Vec::new(),
                    grants: vec![grant],
                    prior_result: prior.clone(),
                },
            ),
            (
                "bash",
                ApprovalKind::MachServiceGrant {
                    services: vec!["com.apple.securityd".into()],
                    prior_result: prior.clone(),
                },
            ),
        ] {
            let session = SessionId::new();
            let state = ToolSessionState::new(workspace.clone());
            let request = ToolCallRequest {
                call_id: prior.call_id.clone(),
                occurrence_id: OccurrenceId::new(),
                tool_id: tool_id.into(),
                input: serde_json::json!({"command":"true", "url":"https://example.com"}).into(),
            };
            let history = vec![
                Event::ToolCallRequested(request.clone()),
                Event::ApprovalRequested(ApprovalRequest {
                    call_id: request.call_id.clone(),
                    occurrence_id: request.occurrence_id.clone(),
                    reason: "grant".into(),
                    kind,
                }),
            ];
            let (writer, ready) = WriterHandle::open(dir.path());
            assert!(matches!(ready.recv().unwrap(), WriterInit::Failed(_)));
            let live =
                LiveState::with_event_log_and_history(session, None, None, writer, history.clone());
            let grants = state.filesystem_grants_snapshot();
            let services = state.mach_services();
            let (tx, rx) = crossbeam_channel::unbounded();
            register_session_runtime(session, state.clone(), live.clone(), tx);
            assert!(matches!(
                resolve_approval(
                    &live.frame(),
                    session,
                    request.identity(),
                    ApprovalDecision::Approve
                ),
                ApprovalOutcome::PersistenceFailed(_)
            ));
            assert_eq!(live.events(), history);
            assert_eq!(state.filesystem_grants_snapshot(), grants);
            assert_eq!(state.mach_services(), services);
            assert!(!state.is_domain_allowed("example.com"));
            assert!(rx.try_recv().is_err());
            unregister_session_runtime(session);
        }
    }
}
