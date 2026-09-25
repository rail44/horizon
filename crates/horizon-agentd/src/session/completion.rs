//! Folding an asynchronous tool completion: the judge's verdict, a
//! finished bash/web call, and the denial outcomes that reissue an approval
//! instead of returning a result to the provider.

use std::sync::Arc;

use crossbeam_channel::Sender;

use horizon_agent::contract::{Command, SessionId};
use horizon_agent::live::LiveState;
use horizon_agent::tools::{
    refuse_unattended, resolve_auto_approval, JudgeDecision, ToolCompletion, ToolUpdate,
};
use horizon_agent::wire::AgentWireEvent;

use super::approval::{emit_human_approval, forward_approval_outcome};
use super::events::send_session_event;
use super::state::AgentdState;

mod retry;
use retry::{
    fold_domain_denied, fold_domain_grant_required, fold_filesystem_denied,
    fold_mach_service_denied,
};

/// Accept the exact live attempt before dispatch. Workers do not mutate the
/// frame; retry handlers receive the accepted request and cannot reselect an
/// occurrence by its provider call ID.
pub(super) fn fold_tool_completion(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    completion: ToolCompletion,
) {
    if !super::events::execution_available(state, live_state, session_id) {
        return;
    }
    let frame = live_state.frame();
    let Some(request) = completion.live_request(&frame).cloned() else {
        return;
    };
    match completion {
        ToolCompletion::ApprovalJudged(judgment) => {
            fold_approval_judgment(state, live_state, commands_tx, session_id, judgment)
        }
        ToolCompletion::Finished(result) => publish_tool_update(
            state,
            commands_tx,
            session_id,
            ToolUpdate::finish(live_state, result),
        ),
        ToolCompletion::DomainDenied { domains, result } => fold_domain_denied(
            state,
            live_state,
            commands_tx,
            session_id,
            request,
            domains,
            result,
        ),
        ToolCompletion::DomainGrantRequired { domains, .. } => {
            fold_domain_grant_required(state, live_state, commands_tx, session_id, request, domains)
        }
        ToolCompletion::FilesystemDenied { denials, result } => fold_filesystem_denied(
            state,
            live_state,
            commands_tx,
            session_id,
            request,
            denials,
            result,
        ),
        ToolCompletion::MachServiceDenied { services, result } => fold_mach_service_denied(
            state,
            live_state,
            commands_tx,
            session_id,
            request,
            services,
            result,
        ),
    }
}

fn fold_approval_judgment(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    judgment: horizon_agent::tools::ApprovalJudgment,
) {
    let frame = live_state.frame();
    match judgment.decision {
        JudgeDecision::AutoApprove => {
            let logged_call_id = judgment.candidate.request.call_id.clone();
            let outcome = resolve_auto_approval(&frame, session_id, &judgment.candidate);
            forward_approval_outcome(state, commands_tx, session_id, logged_call_id, outcome);
        }
        JudgeDecision::Escalate => {
            // A session nobody can approve for gets the refusal instead of
            // a prompt that would never be answered.
            if let Some(outcome) = refuse_unattended(session_id, &judgment.candidate.request) {
                let call_id = judgment.candidate.request.call_id.clone();
                forward_approval_outcome(state, commands_tx, session_id, call_id, outcome);
                return;
            }
            emit_human_approval(state, live_state, session_id, judgment.candidate.approval);
        }
    }
}

#[cfg(test)]
fn fold_bash_completion(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    completion: ToolCompletion,
) {
    fold_tool_completion(state, live_state, commands_tx, session_id, completion);
}

/// Both synchronous approvals and asynchronous completions use the same
/// publication order. The update was applied to LiveState before a worker was started or
/// before this provider result became deliverable.
pub(super) fn publish_tool_update(
    state: &Arc<AgentdState>,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    update: Result<ToolUpdate, String>,
) {
    let Ok(update) = update else {
        let _ = commands_tx.send(Command::Shutdown);
        return;
    };
    let (events, result) = match update {
        ToolUpdate::Started { events } => (events, None),
        ToolUpdate::Finished { events, result } => (events, Some(result)),
    };
    for event in events {
        send_session_event(state, session_id, AgentWireEvent::Event(event));
    }
    if let Some(result) = result {
        let _ = commands_tx.send(Command::ToolCallResult(result));
    }
}

#[cfg(test)]
mod tests;
