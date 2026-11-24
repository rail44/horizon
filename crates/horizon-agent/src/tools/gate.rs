//! The verdict stage's gate (`docs/agent-core-extension-design.md`, "段",
//! 可否): what becomes of a tool call whose plan asks for a verdict.
//!
//! A call arrives here with the approval it needs (an [`ApprovalCandidate`]),
//! from three places: a call the provider just requested ([`open`]), the
//! judge's answer to one ([`judged`]), and a call reissued after its sandbox
//! refused part of what it did ([`reissue`]). Every one goes the same way:
//!
//! 1. The judge is offered the call. If it takes it, the call is pending:
//!    nothing is shown, and the judge's answer comes back later as
//!    `ToolCompletion::ApprovalJudged`.
//! 2. A call the judge does not take, or gives back, is left to a person.
//!    A session nobody can approve for (`ToolSessionState::is_unattended`)
//!    resolves it as a refused result the model can act on; any other
//!    session asks.
//!
//! Every event the gate returns is already in the session's live state
//! (`live`); the host only publishes it and sends what it is told to. The
//! judge and whether the session is attended come from its registered
//! runtime; a session without one has every call put to a person.

use crate::contract::{
    ApprovalKind, ApprovalRequest, Command, Event, OccurrenceId, SessionId, SessionState,
    ToolCallRequest,
};
use crate::judge::{ApprovalCandidate, ApprovalGate, ApprovalJudgment, JudgeDecision};

use super::approval::{ApprovalOutcome, refuse_unattended, resolve_auto_approval};
use crate::live::LiveState;

use super::state::session_runtime;

/// What the gate did with a call.
#[derive(Debug)]
pub enum GateStep {
    /// The judge is deciding; its answer goes to [`judged`].
    Pending,
    /// The session now waits for a person: the prompt's events.
    Asked(Vec<Event>),
    /// Decided without a person.
    Decided(ApprovalOutcome),
}

/// A call the provider requested whose plan asks for a verdict.
pub fn open(session_id: SessionId, live: &LiveState, candidate: ApprovalCandidate) -> GateStep {
    offer(session_id, live, candidate)
}

/// The judge's answer to a call it took.
pub fn judged(session_id: SessionId, live: &LiveState, judgment: ApprovalJudgment) -> GateStep {
    match judgment.decision {
        JudgeDecision::AutoApprove => GateStep::Decided(resolve_auto_approval(
            &live.frame(),
            session_id,
            &judgment.candidate,
        )),
        JudgeDecision::Escalate => leave_to_a_person(session_id, live, judgment.candidate),
    }
}

/// A call reissued as a new attempt that needs `kind` approved: what was
/// recorded for it, the command that tells the provider, and what the gate
/// did with it.
#[derive(Debug)]
pub struct Reissued {
    pub events: Vec<Event>,
    pub command: Command,
    pub step: GateStep,
}

/// Reissues `request` as a new attempt of the same call that needs `kind`
/// approved for `reason`. A domain grant asked for mid-fetch closes the
/// attempt that stopped at the boundary first. `Err` when recording failed.
pub fn reissue(
    session_id: SessionId,
    live: &LiveState,
    mut request: ToolCallRequest,
    kind: ApprovalKind,
    reason: String,
) -> Result<Reissued, String> {
    let prior_identity = request.identity();
    request.occurrence_id = OccurrenceId::new();
    let mut events = Vec::new();
    if matches!(kind, ApprovalKind::DomainGrant { .. }) {
        // The fetch worker stopped at the next domain boundary. Close that
        // attempt before opening the retry; no provider answer exists yet.
        events.push(Event::ToolCallFinished(
            prior_identity
                .result(serde_json::json!({}))
                .superseded_by_retry(&request.occurrence_id),
        ));
    }
    events.push(Event::ToolCallRequested(request.clone()));
    live.extend_provider_events(events.iter().cloned().map(Into::into))?;
    let command = Command::ToolCallReissued(request.identity());
    let approval = ApprovalRequest {
        call_id: request.call_id.clone(),
        occurrence_id: request.occurrence_id.clone(),
        kind,
        reason,
    };
    let step = offer(session_id, live, ApprovalCandidate { request, approval });
    Ok(Reissued {
        events,
        command,
        step,
    })
}

fn offer(session_id: SessionId, live: &LiveState, candidate: ApprovalCandidate) -> GateStep {
    let gate = match session_runtime(session_id) {
        Some(runtime) => review(
            &runtime.tool_state,
            session_id,
            candidate,
            runtime.async_results.clone(),
        ),
        None => ApprovalGate::Human(Box::new(candidate)),
    };
    match gate {
        ApprovalGate::Pending => GateStep::Pending,
        ApprovalGate::Human(candidate) => leave_to_a_person(session_id, live, *candidate),
    }
}

/// Puts `candidate` to the session's verdict modules that review calls
/// before a person is asked (the judge): pending when one takes it.
pub(crate) fn review(
    tool_state: &super::ToolSessionState,
    session_id: SessionId,
    candidate: ApprovalCandidate,
    results: crossbeam_channel::Sender<super::ToolCompletion>,
) -> ApprovalGate {
    let cx = crate::modules::ReviewContext {
        session_id,
        candidate: &candidate,
        results: &results,
    };
    if tool_state
        .verdict_modules()
        .iter()
        .any(|module| module.review(&cx))
    {
        ApprovalGate::Pending
    } else {
        ApprovalGate::Human(Box::new(candidate))
    }
}

/// A session nobody can approve for refuses the call; any other asks,
/// while the call is still waiting for a verdict.
fn leave_to_a_person(
    session_id: SessionId,
    live: &LiveState,
    candidate: ApprovalCandidate,
) -> GateStep {
    if let Some(outcome) = refuse_unattended(session_id, &candidate) {
        return GateStep::Decided(outcome);
    }
    let frame = live.frame();
    let approval = candidate.approval;
    if !super::should_fold_completion(&frame, &approval.call_id)
        || !frame
            .tool_call_request(&approval.call_id)
            .is_some_and(|request| request.identity() == approval.identity())
    {
        return GateStep::Decided(ApprovalOutcome::AlreadyResolved);
    }
    let events = vec![
        Event::ApprovalRequested(approval),
        Event::StateChanged(SessionState::WaitingForApproval),
    ];
    match live.extend_provider_events(events.iter().cloned().map(Into::into)) {
        Ok(_) => GateStep::Asked(events),
        Err(error) => GateStep::Decided(ApprovalOutcome::PersistenceFailed(error)),
    }
}

#[cfg(test)]
mod tests;
