use super::completion::approval_is_unresolved;
use super::transition::ToolUpdate;
use serde_json::Value;

use crate::contract::SessionId;
use crate::contract::{ApprovalKind, Command, ToolCallId, ToolCallRequest, ToolCallResult};
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
    /// swallowing it — see `horizon-agentd`'s `session::resolve_and_forward`.
    AlreadyResolved,
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

/// Resolves a user's approve/deny decision for the tool call pending in
/// `frame` under `call_id`.
pub fn resolve_approval(
    frame: &AgentFrame,
    session_id: SessionId,
    call_id: ToolCallId,
    decision: ApprovalDecision,
) -> ApprovalOutcome {
    if let Some(outcome) = try_execute(
        frame,
        session_id,
        &call_id,
        &decision,
        ApprovalSource::Human,
    ) {
        return outcome;
    }

    ApprovalOutcome::Forward(match decision {
        ApprovalDecision::Approve => Command::ApproveToolCall { call_id },
        ApprovalDecision::Deny { reason } => Command::DenyToolCall { call_id, reason },
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
    if !approval_is_unresolved(frame, &candidate.request) {
        return ApprovalOutcome::AlreadyResolved;
    }
    if !is_horizon_executed_tool(&request.tool_id) {
        return ApprovalOutcome::Forward(Command::ApproveToolCall {
            call_id: call_id.clone(),
        });
    }
    let Some(runtime) = session_runtime(session_id) else {
        return ApprovalOutcome::Forward(Command::ApproveToolCall {
            call_id: call_id.clone(),
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
    Some(ApprovalOutcome::Applied(ToolUpdate::finish(
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

fn try_execute(
    frame: &AgentFrame,
    session_id: SessionId,
    call_id: &ToolCallId,
    decision: &ApprovalDecision,
    approval_source: ApprovalSource,
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
    if !approval_is_unresolved(frame, request) {
        return Some(ApprovalOutcome::AlreadyResolved);
    }
    let runtime = session_runtime(session_id)?;

    // Human approvals read the displayed kind; automatic approvals carry
    // the judge's candidate after checking the exact current request.
    Some(dispatch_approval(
        session_id,
        &runtime,
        request,
        decision,
        frame.approval_kind(call_id).unwrap_or_default(),
        approval_source,
    ))
}

fn dispatch_approval(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &ToolCallRequest,
    decision: &ApprovalDecision,
    kind: ApprovalKind,
    source: ApprovalSource,
) -> ApprovalOutcome {
    match request.tool_id.as_str() {
        "bash" => resolve_bash(session_id, runtime, request, decision, kind, source),
        "web_fetch" => resolve_web_fetch(session_id, runtime, request, decision, kind),
        _ => resolve_synchronous_tool(runtime, request, decision),
    }
}

fn resolve_web_fetch(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &ToolCallRequest,
    decision: &ApprovalDecision,
    kind: ApprovalKind,
) -> ApprovalOutcome {
    if matches!(decision, ApprovalDecision::Deny { .. }) {
        crate::tools::web::clear_approved_domains(session_id, &request.call_id);
        return synchronous_result(runtime, &request.call_id, denied_output(), false);
    }
    let ApprovalKind::DomainGrant { domains } = kind else {
        crate::tools::web::clear_approved_domains(session_id, &request.call_id);
        return synchronous_result(
            runtime,
            &request.call_id,
            error_output("web_fetch approval did not carry a supported domain grant"),
            false,
        );
    };
    if domains.is_empty() {
        crate::tools::web::clear_approved_domains(session_id, &request.call_id);
        return synchronous_result(
            runtime,
            &request.call_id,
            error_output("web_fetch domain grant was empty"),
            false,
        );
    }
    let validated = domains
        .iter()
        .map(|domain| crate::tools::web::validate_domain_grant(domain))
        .collect::<Result<Vec<_>, _>>();
    let Ok(validated) = validated else {
        crate::tools::web::clear_approved_domains(session_id, &request.call_id);
        return synchronous_result(
            runtime,
            &request.call_id,
            error_output("web_fetch domain grant failed revalidation"),
            false,
        );
    };
    for domain in &validated {
        runtime.tool_state.allow_domain(domain.clone());
    }
    let approved_domains =
        crate::tools::web::record_approved_domains(session_id, &request.call_id, &validated);

    let outcome = begin_execution(runtime, request, None);
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
/// but an approve only *starts* the command — see `ToolUpdate::Started`. Domain-denial retries, filesystem-denial retries, and
/// pre-execution [`ApprovalKind::GitOperation`] grants all keep the rerun
/// sandboxed — an approval buys scoped, contained access, never an
/// unconfined execution. Only [`ApprovalKind::Standard`] still runs on the
/// host, which is what that kind has always meant.
fn resolve_bash(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &ToolCallRequest,
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
        ApprovalKind::DomainGrant { .. } => synchronous_result(
            runtime,
            &request.call_id,
            error_output("A host-side domain grant cannot authorize a bash command."),
            false,
        ),
        ApprovalKind::Standard => {
            resolve_standard_bash(session_id, runtime, request, decision, approval_source)
        }
    }
}

fn resolve_git_operation(
    session_id: SessionId,
    runtime: &SessionRuntime,
    request: &ToolCallRequest,
    decision: &ApprovalDecision,
    writable_roots: Vec<std::path::PathBuf>,
) -> ApprovalOutcome {
    if matches!(decision, ApprovalDecision::Deny { .. }) {
        return synchronous_result(runtime, &request.call_id, denied_output(), false);
    }
    if !bash::requires_metadata_write(&request.input) {
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
    request: &ToolCallRequest,
    decision: &ApprovalDecision,
    approval_source: ApprovalSource,
) -> ApprovalOutcome {
    match decision {
        ApprovalDecision::Approve => {
            let outcome = begin_execution(runtime, request, None);

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
            synchronous_result(runtime, &request.call_id, denied_output(), false)
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
    request: &ToolCallRequest,
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
    if let Err(error) = runtime.tool_state.approve_filesystem_grants(&grants) {
        return unstarted_error(
            runtime,
            &request.call_id,
            &format!("Approved filesystem grants could not be revalidated: {error}"),
        );
    }
    let Some(workspace_root) = runtime.tool_state.workspace_root() else {
        return forward_prior_result(runtime, prior_result);
    };
    // The audit answer to "what filesystem authority did this session run
    // with": every grant it holds now, config-injected and approved alike.
    runtime
        .live_state
        .record_filesystem_grants(&runtime.tool_state.filesystem_grants_snapshot());

    let outcome = begin_execution(runtime, request, Some(&prior_result));
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
    outcome
}

/// Fold the start before enqueueing a job. A retry closes its abandoned
/// attempt first, preserving the old and new occurrence identities.
fn begin_execution(
    runtime: &SessionRuntime,
    request: &ToolCallRequest,
    prior_result: Option<&ToolCallResult>,
) -> ApprovalOutcome {
    ApprovalOutcome::Applied(ToolUpdate::start(
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
    ApprovalOutcome::Applied(ToolUpdate::decline_retry(&runtime.live_state, prior_result))
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
    request: &ToolCallRequest,
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
            for domain in &domains {
                network.allow_domain(domain.clone());
            }

            let outcome = begin_execution(runtime, request, Some(&prior_result));

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
    request: &ToolCallRequest,
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
            runtime.tool_state.approve_mach_services(&services);

            let outcome = begin_execution(runtime, request, Some(&prior_result));

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
    let result = if ran {
        identity.result(output)
    } else {
        ToolCallResult::denied(call_id.clone(), identity.occurrence_id.clone(), output)
    };

    ApprovalOutcome::Applied(if ran {
        ToolUpdate::executed(&runtime.live_state, result, identity)
    } else {
        ToolUpdate::finish(&runtime.live_state, result)
    })
}
