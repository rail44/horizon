//! Folding an asynchronous tool completion: the judge's verdict, a
//! finished bash/web call, and the denial outcomes that reissue an approval
//! instead of returning a result to the provider.

use std::sync::Arc;

use crossbeam_channel::Sender;

use horizon_agent::contract::{Command, Event, SessionId, SessionState, ToolCallResult};
use horizon_agent::live::LiveState;
use horizon_agent::tools::{
    refuse_unattended, resolve_auto_approval, should_fold_completion, JudgeDecision, ToolCompletion,
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

/// The async-execution analogue of `run::handle_provider_event`'s fold, for a
/// bash or host-side web call whose result has now arrived on its own
/// channel -- the same shape the deleted
/// in-process agent runtime's `fold_bash_completion` used to have,
/// forwarding the same events over the wire instead of updating a local
/// `Frames` signal, except the trailing `StateChanged` is no longer
/// unconditional (see below).
///
/// Bash and web tools complete asynchronously here; fs/config tools resolve synchronously
/// inside `agent::tools::approval::resolve_synchronous_tool` (folded
/// straight into `dispatch_inbound_command`'s `resolve_and_forward`) -- so
/// this is the one place a completion can land after *other* tool-call
/// approvals from the same turn are still outstanding.
pub(super) fn fold_tool_completion(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    completion: ToolCompletion,
) {
    if !completion.matches_live_request(&live_state.frame()) {
        return;
    }
    match completion {
        ToolCompletion::ApprovalJudged(judgment) => {
            fold_approval_judgment(state, live_state, commands_tx, session_id, judgment)
        }
        ToolCompletion::Finished(result) => {
            fold_finished_bash_result(state, live_state, commands_tx, session_id, result)
        }
        ToolCompletion::DomainDenied {
            call_id,
            domains,
            result,
        } => fold_domain_denied(state, live_state, session_id, call_id, domains, result),
        ToolCompletion::DomainGrantRequired {
            call_id, domains, ..
        } => fold_domain_grant_required(state, live_state, session_id, call_id, domains),
        ToolCompletion::FilesystemDenied {
            call_id,
            denials,
            result,
        } => fold_filesystem_denied(state, live_state, session_id, call_id, denials, result),
        ToolCompletion::MachServiceDenied {
            call_id,
            services,
            result,
        } => fold_mach_service_denied(state, live_state, session_id, call_id, services, result),
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
    if !should_fold_completion(&frame, &judgment.candidate.request.call_id)
        || frame.has_tool_call_started(&judgment.candidate.request.call_id)
    {
        return;
    }
    if frame
        .actionable_pending_approval_call_ids()
        .contains(&judgment.candidate.request.call_id)
    {
        // A duplicate/stale verdict must not duplicate a prompt or overturn
        // a verdict that has already escalated to the human.
        return;
    }
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

/// The ordinary case: a bash call actually finished (successfully or not).
/// Unchanged behavior from before [`BashCompletion`] grew a second variant.
fn fold_finished_bash_result(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    result: ToolCallResult,
) {
    let frame = live_state.frame();
    if !should_fold_completion(&frame, &result.call_id) {
        return;
    }

    // New workers carry their dispatch identity. Only legacy/synthetic
    // completions need the current request's occurrence filled in here.
    let result = ToolCallResult {
        occurrence_id: result.occurrence_id.clone().or_else(|| {
            frame
                .tool_call_request(&result.call_id)
                .and_then(|request| request.occurrence_id.clone())
        }),
        ..result
    };

    // Honest trailing state: a second approval-gated call from the same
    // turn (another `bash` approved earlier, or a sibling fs/config
    // request still awaiting a decision) can still be outstanding when
    // this one finishes -- reporting `WaitingForUser` then is exactly the
    // backlog #34 bug (status line blanks, stop button vanishes, while a
    // decision is still actionable). `actionable_pending_approval_call_ids`
    // (not the plain `pending_approval_call_ids`) is the right reader here
    // for the same reason it's the required one on every dispatch path
    // (see its doc comment): it excludes a *ghost* request whose own turn
    // already ended, which no live daemon-side gate can ever answer, so a
    // ghost alone must never hold the reported state at `WaitingForApproval`
    // forever. `result.call_id` itself is still in that list at this point
    // -- only a *folded* `ToolCallFinished` clears an id, and this call's
    // hasn't been folded yet -- so it's excluded explicitly rather than
    // re-reading the frame after folding.
    //
    // If nothing else is actionable, the turn is still running: the result
    // is about to be handed back to the provider via `commands_tx.send`,
    // which will run another completion. Reporting `WaitingForUser` here
    // would tell observers the turn finished and would make the persistence
    // turn tracker close the turn prematurely. `Running` keeps the stop
    // button enabled and the composer in the "running" placeholder until
    // the provider itself emits `TurnEnded` and the real `WaitingForUser`
    // at the turn boundary.
    let approval_still_pending = frame
        .actionable_pending_approval_call_ids()
        .into_iter()
        .any(|id| id != result.call_id);
    let trailing_state = if approval_still_pending {
        SessionState::WaitingForApproval
    } else {
        SessionState::Running
    };

    let events = vec![
        Event::ToolCallFinished(result.clone()),
        Event::StateChanged(trailing_state),
    ];
    let _ = live_state.extend_provider_events(events.clone().into_iter().map(Into::into));
    for event in events {
        send_session_event(state, session_id, AgentWireEvent::Event(event));
    }

    let _ = commands_tx.send(Command::ToolCallResult(result));
}

#[cfg(test)]
mod tests;
