//! The approval seam on the daemon side: gating a policy-generated prompt
//! behind the judge, emitting the human prompt when one is owed, and
//! resolving an inbound approve/deny into provider commands.

use std::sync::Arc;

use crossbeam_channel::Sender;

use horizon_agent::contract::{
    ApprovalRequest, Command, Event, OccurrenceId, ProviderEvent, SessionId, SessionState,
    ToolCallId,
};
use horizon_agent::live::LiveState;
use horizon_agent::tools::{
    resolve_approval, should_fold_completion, start_approval_gate, ApprovalCandidate,
    ApprovalDecision, ApprovalGate, ApprovalOutcome,
};
use horizon_agent::wire::AgentWireEvent;

use super::events::send_session_event;
use super::state::SessiondState;

/// Intercepts the policy-generated human prompt after its kind/reason are
/// fully derived. The original tool request remains foldable immediately;
/// only the prompt and waiting state are held while the asynchronous judge
/// runs.
pub(super) fn gate_processing_approval(session_id: SessionId, events: &mut Vec<ProviderEvent>) {
    gate_processing_approval_with(events, |candidate| {
        start_approval_gate(session_id, candidate)
    });
}

fn gate_processing_approval_with(
    events: &mut Vec<ProviderEvent>,
    start_gate: impl FnOnce(ApprovalCandidate) -> ApprovalGate,
) {
    let request = events.iter().find_map(|event| match &event.event {
        Event::ToolCallRequested(request) => Some(request.clone()),
        _ => None,
    });
    let approval = events.iter().find_map(|event| match &event.event {
        Event::ApprovalRequested(approval) => Some(approval.clone()),
        _ => None,
    });
    let (Some(request), Some(approval)) = (request, approval) else {
        return;
    };
    let call_id = approval.call_id.clone();
    let candidate = ApprovalCandidate { request, approval };
    if matches!(start_gate(candidate), ApprovalGate::Pending) {
        events.retain(|event| {
            !matches!(
                &event.event,
                Event::ApprovalRequested(approval) if approval.call_id == call_id
            ) && !matches!(
                &event.event,
                Event::StateChanged(SessionState::WaitingForApproval)
            )
        });
    }
}

pub(super) fn emit_human_approval(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    session_id: SessionId,
    approval: ApprovalRequest,
) {
    let frame = live_state.frame();
    if !should_fold_completion(&frame, &approval.call_id) {
        return;
    }
    let events = vec![
        Event::ApprovalRequested(approval),
        Event::StateChanged(SessionState::WaitingForApproval),
    ];
    let _ = live_state.extend_provider_events(events.clone().into_iter().map(Into::into));
    for event in events {
        send_session_event(state, session_id, AgentWireEvent::Event(event));
    }
}

pub(super) fn begin_reissued_approval(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    session_id: SessionId,
    request: horizon_agent::contract::ToolCallRequest,
    approval: ApprovalRequest,
) {
    // Mint a fresh `OccurrenceId` for the reissue. The provider hands the
    // same `call_id` back to us on retry -- it has no concept of a retry
    // -- so by `call_id` alone the two attempts of one conceptual
    // sandbox-denial-retry collapse onto each other (the cosmetic
    // "started-but-never-finished" defect the user sees daily, and the
    // approval-attribution ambiguity that follows). `OccurrenceId` is the
    // second key the transcript, approval modal, and analytics each
    // follow; the first attempt (whose `ToolCallRequested` is already in
    // the frame with its own `OccurrenceId`) keeps its identity, the new
    // attempt takes this fresh one, and both stay attributable. UUID v4
    // -- not a per-session counter -- so a resumed session and a replayed
    // log line up without any shared counter to coordinate (the prior
    // generation-counter pattern in `crates/horizon-agent/src/tools/web/
    // mod.rs`'s task registry -- `next_generation` and the
    // `RegisteredTask`/`finish_registration` pair -- is a process-local
    // `AtomicU64` and is never persisted, so it would not survive
    // either of those).
    let occurrence_id = OccurrenceId::new();
    let request = horizon_agent::contract::ToolCallRequest {
        occurrence_id: Some(occurrence_id.clone()),
        ..request
    };
    let approval = ApprovalRequest {
        occurrence_id: Some(occurrence_id),
        ..approval
    };
    let request_event = Event::ToolCallRequested(request.clone());
    let _ = live_state.extend_provider_events(std::iter::once(request_event.clone().into()));
    send_session_event(state, session_id, AgentWireEvent::Event(request_event));
    let candidate = ApprovalCandidate { request, approval };
    if let ApprovalGate::Human(candidate) = start_approval_gate(session_id, candidate) {
        emit_human_approval(state, live_state, session_id, candidate.approval);
    }
}

/// A `Command` envelope arriving from Horizon for this session.
/// `ApproveToolCall`/`DenyToolCall` are resolved right here (decision 2:
/// "Approval decisions stay in Horizon... resolved in sessiond") via
/// `tools::approval::resolve_approval`; everything else forwards straight
/// to the provider, unchanged. (An earlier in-process shell shared this
/// helper from its own click handler; that path retired with the
/// runtime split.)
pub(super) fn dispatch_inbound_command(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    command: Command,
) {
    match command {
        Command::ApproveToolCall { call_id } => resolve_and_forward(
            state,
            live_state,
            commands_tx,
            session_id,
            call_id,
            ApprovalDecision::Approve,
        ),
        Command::DenyToolCall { call_id, reason } => resolve_and_forward(
            state,
            live_state,
            commands_tx,
            session_id,
            call_id,
            ApprovalDecision::Deny { reason },
        ),
        other => {
            let _ = commands_tx.send(other);
        }
    }
}

fn resolve_and_forward(
    state: &Arc<SessiondState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    call_id: ToolCallId,
    decision: ApprovalDecision,
) {
    // `resolve_approval` moves `call_id`; keep a copy so the
    // `AlreadyResolved` arm below can still name it in its log line.
    let logged_call_id = call_id.clone();
    let frame = live_state.frame();
    let outcome = resolve_approval(&frame, session_id, call_id, decision);
    forward_approval_outcome(state, commands_tx, session_id, logged_call_id, outcome);
}

pub(super) fn forward_approval_outcome(
    state: &Arc<SessiondState>,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    logged_call_id: ToolCallId,
    outcome: ApprovalOutcome,
) {
    match outcome {
        ApprovalOutcome::Executed {
            events, command, ..
        } => {
            for event in events {
                send_session_event(state, session_id, AgentWireEvent::Event(event));
            }
            let _ = commands_tx.send(command);
        }
        ApprovalOutcome::Started { events, .. } => {
            for event in events {
                send_session_event(state, session_id, AgentWireEvent::Event(event));
            }
        }
        ApprovalOutcome::Forward(command) => {
            let _ = commands_tx.send(command);
        }
        // The pending -> resolved transition already happened for this
        // call_id (started or finished) -- see `ApprovalOutcome::
        // AlreadyResolved`'s doc comment. This is the guard that stops a
        // burst of duplicate `Approve`/`Deny` commands (the 2026-07
        // repeated-approval OOM incident) from re-executing anything: every
        // one after the first lands here and is dropped, logged rather than
        // silently swallowed so a runaway burst like that incident's is
        // visible in sessiond's own stderr.
        ApprovalOutcome::AlreadyResolved => {
            eprintln!(
                "horizon-sessiond: dropped duplicate approve/deny for session {session_id:?}, \
                 call {logged_call_id:?} (already resolved)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::judge_candidate;

    #[test]
    fn gate_suppresses_prompt_while_pending_and_preserves_human_fallback() {
        let candidate = judge_candidate("gate-shape");
        let original = vec![
            ProviderEvent::from(Event::ToolCallRequested(candidate.request.clone())),
            ProviderEvent::from(Event::ApprovalRequested(candidate.approval.clone())),
            ProviderEvent::from(Event::StateChanged(SessionState::WaitingForApproval)),
        ];

        let mut pending = original.clone();
        gate_processing_approval_with(&mut pending, |observed| {
            assert_eq!(observed, candidate);
            ApprovalGate::Pending
        });
        assert_eq!(pending.len(), 1);
        assert!(matches!(pending[0].event, Event::ToolCallRequested(_)));

        let mut human = original.clone();
        gate_processing_approval_with(&mut human, |candidate| {
            ApprovalGate::Human(Box::new(candidate))
        });
        assert_eq!(human, original);
    }
}
