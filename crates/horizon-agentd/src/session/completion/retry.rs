//! Convert containment evidence into a new approval while retaining the completed attempt.

use crate::session::approval::begin_reissued_approval;
use crate::session::state::AgentdState;
use horizon_agent::contract::{
    ApprovalKind, Command, SessionId, ToolCallId, ToolCallRequest, ToolCallResult,
};
use horizon_agent::live::LiveState;
use horizon_agent::tools::should_fold_completion;
use std::path::PathBuf;
use std::sync::Arc;

/// Finished/cancelled occurrences and requests absent from the frame cannot
/// create another approval. The live request owns the completed attempt's ID.
fn pending_request(live: &LiveState, call_id: &ToolCallId) -> Option<ToolCallRequest> {
    let frame = live.frame();
    if !should_fold_completion(&frame, call_id) {
        return None;
    }
    frame.tool_call_request(call_id).cloned()
}

/// A sandboxed `bash` call was refused mach-lookup to macOS security
/// services (`docs/macos-containment-denial-reporting-design.md`) -- the
/// macOS counterpart of [`fold_domain_denied`]: the call already ran to
/// completion (evidence is the kernel's own unified-log denial record), so
/// the reissued request carries the same retry shape --
/// [`ApprovalKind::MachServiceGrant`] with `prior_result` -- so a later
/// deny can forward it as-is (`tools::approval::resolve_mach_service_grant`).
///
/// The reason text states the enforcement granularity honestly: approving
/// opens nono's whole security-service group (all-or-nothing -- the
/// seatbelt profile has no per-service granularity), which includes the
/// keychain. The service names themselves stay the primitive
/// (`mach-lookup` targets); "keychain" appears only as this explanation.
pub(super) fn fold_mach_service_denied(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &crossbeam_channel::Sender<Command>,
    session_id: SessionId,
    services: Vec<String>,
    result: ToolCallResult,
) {
    let Some(original_request) = pending_request(live_state, &result.call_id) else {
        // Should be unreachable (this call_id was necessarily requested to
        // have gotten this far) -- nothing sane to reissue against.
        return;
    };

    let service_list = services.join(", ");
    let reason = format!(
        "`{}` tried to reach macOS security services ({service_list}) -- typically the \
         keychain -- and the sandbox refused. Approving allows the macOS security/keychain \
         service group for this session (all-or-nothing: the whole group, not just the named \
         services) and retries the same call, still sandboxed. The grant lasts for this \
         session only.",
        original_request.tool_id
    );
    begin_reissued_approval(
        state,
        live_state,
        session_id,
        original_request.clone(),
        ApprovalKind::MachServiceGrant {
            services,
            prior_result: result,
        },
        reason,
        commands_tx,
    );
}

/// A tier-1 sandboxed `bash` call's network egress was refused for one or
/// more `domains` (`docs/agent-approval-design.md` leg 4b) -- surface a
/// fresh, differently-named approval offer ("allow domain X for this
/// session and retry") instead of handing `result` straight to the
/// provider. It folds a fresh `ToolCallRequested` right before the
/// `ApprovalRequested`, so the eventual Approve/Deny is not misclassified
/// as `AlreadyResolved`. `result` is the genuine completed outcome, carried
/// on the pending request's own [`ApprovalKind::DomainDenialRetry`] so a
/// later deny can forward it as-is (`tools::approval::
/// resolve_domain_denial_retry`).
pub(super) fn fold_domain_denied(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &crossbeam_channel::Sender<Command>,
    session_id: SessionId,
    domains: Vec<String>,
    result: ToolCallResult,
) {
    let Some(original_request) = pending_request(live_state, &result.call_id) else {
        // Should be unreachable (this call_id was necessarily requested to
        // have gotten this far) -- nothing sane to reissue against.
        return;
    };

    let domain_list = domains.join(", ");
    let reason = format!(
        "`{}` tried to reach {domain_list}, but it isn't allowed \
         for this session yet. Allow {} for this session and retry?",
        original_request.tool_id,
        if domains.len() == 1 { "it" } else { "them" }
    );
    begin_reissued_approval(
        state,
        live_state,
        session_id,
        original_request.clone(),
        ApprovalKind::DomainDenialRetry {
            domains,
            prior_result: result,
        },
        reason,
        commands_tx,
    );
}

pub(super) fn fold_domain_grant_required(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &crossbeam_channel::Sender<Command>,
    session_id: SessionId,
    call_id: ToolCallId,
    domains: Vec<String>,
) {
    let Some(original_request) = pending_request(live_state, &call_id) else {
        return;
    };
    let domain_list = domains.join(", ");
    let reason = format!(
        "`{}` needs to contact {domain_list}, but no request was sent to that domain. Allow {} \
         for this session and retry from the original URL?",
        original_request.tool_id,
        if domains.len() == 1 { "it" } else { "them" }
    );
    begin_reissued_approval(
        state,
        live_state,
        session_id,
        original_request,
        ApprovalKind::DomainGrant { domains },
        reason,
        commands_tx,
    );
}

pub(super) fn fold_filesystem_denied(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &crossbeam_channel::Sender<Command>,
    session_id: SessionId,
    denials: Vec<horizon_sandbox::FilesystemDenial>,
    result: ToolCallResult,
) {
    let Some(original_request) = pending_request(live_state, &result.call_id) else {
        return;
    };
    let attempted = denials
        .iter()
        .map(|denial| denial.attempted_path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    // The shaping is generic over path structure only -- see
    // `horizon_sandbox::suggest_grants`. Agentd supplies the two facts it
    // owns (this session's workspace root, this account's `$HOME`) and
    // nothing about what command was run.
    let grants = horizon_sandbox::suggest_grants(
        &denials,
        session_workspace_root(state, session_id).as_deref(),
        horizon_sandbox::home_dir().as_deref(),
    );
    let offered = grants
        .iter()
        .map(|grant| {
            format!(
                "{:?} {:?} access to {}",
                grant.access,
                grant.scope,
                grant.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    // The prompt states what approval actually buys. It used to offer whole
    // -call host authority; it now offers exactly these grants, and the
    // command still runs sandboxed with them (`docs/containment-denial-
    // narrow-grants-design.md`'s 2026-07-26 decision).
    let reason = format!(
        "`bash` was refused access outside its workspace: attempted {attempted}. \
         Grant {offered} to this session and retry the same call, still sandboxed? \
         The grant lasts for this session only."
    );
    begin_reissued_approval(
        state,
        live_state,
        session_id,
        original_request.clone(),
        ApprovalKind::FilesystemDenialRetry {
            denials,
            grants,
            // Same prior_result fixup as `fold_domain_denied` --
            // bash constructed the result without an in-scope
            // request, so stamp the original request's
            // `occurrence_id` onto it now so the transcript and
            // analytics attribute it to the right occurrence.
            prior_result: result,
        },
        reason,
        commands_tx,
    );
}

/// This session's confinement root, as recorded when it started -- the
/// workspace half of the suggestion shaping's input. `None` for a session
/// with no root (or one agentd no longer tracks), in which case every
/// attempt is treated as outside, which is the conservative reading.
fn session_workspace_root(state: &Arc<AgentdState>, session_id: SessionId) -> Option<PathBuf> {
    let root = state
        .sessions
        .lock()
        .unwrap()
        .get(&session_id)
        .and_then(|entry| entry.workspace_root.clone())?;
    // Canonical on both sides or the containment test is meaningless: the
    // supervisor reports resolved paths, and this entry holds whatever the
    // spawn was given.
    std::fs::canonicalize(root).ok()
}
