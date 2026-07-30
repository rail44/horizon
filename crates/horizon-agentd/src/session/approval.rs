//! The approval seam on the daemon side: gating a policy-generated prompt
//! behind the judge, emitting the human prompt when one is owed, and
//! resolving an inbound approve/deny into provider commands.

use std::sync::Arc;

use crossbeam_channel::Sender;

use horizon_agent::contract::{
    ApprovalDecisionPayload, ApprovalRequest, Command, ContinueTurnRequested, Event, OccurrenceId,
    ProviderEvent, SessionId, SessionState, ToolCallId, TurnEndReason,
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
/// "Approval decisions stay in Horizon... resolved in agentd") via
/// `tools::approval::resolve_approval`; `ContinueTurn` is special-cased to
/// emit an audit event (`Event::ContinueTurnRequested`) recording the
/// `TurnEndReason` of the most recent `TurnEnded` item in the live frame
/// before forwarding the command to the provider unchanged. Every other
/// command forwards straight to the provider. (An earlier in-process shell
/// shared this helper from its own click handler; that path retired with
/// the runtime split.)
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
        Command::ContinueTurn => {
            // The audit event is the v16 (`SESSION_PROTOCOL_VERSION`'s v16
            // note) fix for the operator-intervention gap surfaced by the
            // 2026-07-28 session aa95e066 dogfooding report: a `ContinueTurn`
            // previously left no event at all, so an analyst couldn't tell
            // a 3-Continue-turns run from a 0-Continue-turns one without
            // reading the rig session loop's code. `resumed_from` carries
            // the most recent `TurnEnded`'s reason when there is one (the
            // real halt case); the no-op replay / idle-session case
            // promotes `None` to `Unknown` per
            // `Event::ContinueTurnRequested`'s doc comment, so a non-zero
            // `Unknown` count from analytics still surfaces UI races.
            let resumed_from = live_state
                .frame()
                .last_turn_end_reason()
                .unwrap_or(TurnEndReason::Unknown);
            let event = Event::ContinueTurnRequested(ContinueTurnRequested { resumed_from });
            let _ = live_state.extend_provider_events(std::iter::once(event.clone().into()));
            send_session_event(state, session_id, AgentWireEvent::Event(event));
            let _ = commands_tx.send(Command::ContinueTurn);
        }
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

    // Emit `Event::ApprovalResolved` *before* `resolve_approval` so the
    // audit row exists regardless of which `ApprovalOutcome` variant
    // resolves -- `Executed`/`Started`/`Forward` are real resolutions, and
    // `AlreadyResolved` (a duplicate Approve/Deny click) is itself an
    // operator action the analyst wants to count. `occurrence_id` is
    // recovered from the frame's most-recent `ToolCallRequest` for this
    // `call_id` (the same `.rev()` walk `tools::approval::try_execute`
    // uses to find the matching approval kind) so the audit row pairs
    // with the right `ApprovalRequested` occurrence under a reused
    // `call_id` or a sandbox-denial retry.
    let occurrence_id = frame
        .tool_call_request(&logged_call_id)
        .and_then(|request| request.occurrence_id.clone());
    let resolved_event = Event::ApprovalResolved(horizon_agent::contract::ApprovalResolved {
        call_id: logged_call_id.clone(),
        occurrence_id,
        decision: approval_decision_payload(&decision),
    });
    let _ = live_state.extend_provider_events(std::iter::once(resolved_event.clone().into()));
    send_session_event(state, session_id, AgentWireEvent::Event(resolved_event));

    let outcome = resolve_approval(&frame, session_id, call_id, decision);
    forward_approval_outcome(state, commands_tx, session_id, logged_call_id, outcome);
}

/// Wire-stable conversion from the internal [`ApprovalDecision`] (used only
/// inside `horizon-agent::tools::approval`) to the on-disk event payload
/// [`ApprovalDecisionPayload`] carried by `Event::ApprovalResolved`.
/// `Deny { reason }`'s `reason` is `Option<String>` in both shapes, so a
/// `None` on the inbound side round-trips as `None` (omitted via
/// `skip_serializing_if = "Option::is_none"`) on the audit row.
fn approval_decision_payload(decision: &ApprovalDecision) -> ApprovalDecisionPayload {
    match decision {
        ApprovalDecision::Approve => ApprovalDecisionPayload::Approve,
        ApprovalDecision::Deny { reason } => ApprovalDecisionPayload::Deny {
            reason: reason.clone(),
        },
    }
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
        // visible in agentd's own stderr.
        ApprovalOutcome::AlreadyResolved => {
            eprintln!(
                "horizon-agentd: dropped duplicate approve/deny for session {session_id:?}, \
                 call {logged_call_id:?} (already resolved)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::{drain_events, judge_candidate, judge_test_state};
    use horizon_agent::contract::{
        ApprovalDecisionPayload, ApprovalKind, ApprovalRequest, ApprovalResolved,
        ContinueTurnRequested, OccurrenceId, ProviderEvent, ToolCallRequest,
    };
    use horizon_agent::live::LiveState;

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

    /// `SESSION_PROTOCOL_VERSION` v16's `Event::ContinueTurnRequested`: a
    /// `ContinueTurn` that lands while the live frame's last item is a
    /// guard-halted `TurnEnded` records that halt's reason as
    /// `resumed_from`, the live state folds the event, and the command is
    /// still forwarded to the provider. The audit row is the only signal
    /// an analyst has that the operator resumed a halted turn
    /// (`docs/issues/002-agent-iteration-cap-halts-real-work.md` decision 3);
    /// see the v16 doc comment on `SESSION_PROTOCOL_VERSION` for the wider
    /// motivation.
    #[test]
    fn continue_turn_records_resumed_from_when_halted() {
        let state = judge_test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, commands_rx) = crossbeam_channel::unbounded::<Command>();

        // Seed the frame with a `TurnEnded` that uses one of the v16-meaningful
        // halt reasons, so the emit site must actually walk the frame to
        // recover it (the `.rev()` pattern `last_turn_end_reason` shares with
        // `tool_call_request`/`approval_kind`) rather than hard-coding anything.
        live_state.extend_provider_events([ProviderEvent::from(Event::TurnEnded(
            TurnEndReason::HaltedByIterationCap,
        ))]);

        dispatch_inbound_command(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            Command::ContinueTurn,
        );

        // The live state carries the audit event with the recovered reason.
        let events = live_state.events();
        let resolved = events
            .iter()
            .find_map(|event| match event {
                Event::ContinueTurnRequested(ContinueTurnRequested { resumed_from }) => {
                    Some(*resumed_from)
                }
                _ => None,
            })
            .expect("a ContinueTurnRequested event recorded in the live state");
        assert_eq!(resolved, TurnEndReason::HaltedByIterationCap);

        // The original command is forwarded to the provider unchanged so
        // the resume itself still happens.
        assert!(matches!(
            commands_rx.try_recv().expect("the command is forwarded"),
            Command::ContinueTurn
        ));
    }

    /// The no-op replay case documented on
    /// `Event::ContinueTurnRequested::resumed_from`: a `ContinueTurn` sent
    /// to a session whose frame has no `TurnEnded` records `Unknown`, not
    /// a panic and not a silent skip, so analytics can count the attempt.
    #[test]
    fn continue_turn_records_unknown_when_no_halt_exists() {
        let state = judge_test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, commands_rx) = crossbeam_channel::unbounded::<Command>();

        dispatch_inbound_command(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            Command::ContinueTurn,
        );

        let resumed_from = live_state.events().iter().find_map(|event| match event {
            Event::ContinueTurnRequested(ContinueTurnRequested { resumed_from }) => {
                Some(*resumed_from)
            }
            _ => None,
        });
        assert_eq!(resumed_from, Some(TurnEndReason::Unknown));
        assert!(commands_rx.try_recv().is_ok(), "command still forwarded");
    }

    /// A `ContinueTurn` that lands after the frame has moved past the
    /// halt (a later item, e.g. a new message) records the latest
    /// `TurnEnded` regardless -- the `.rev()` walk in
    /// `last_turn_end_reason` returns the *most recent* one, even if it is
    /// no longer the frame's tail. That is the honest read of "what turn
    /// ended last" and matches the audit semantic in
    /// `Event::ContinueTurnRequested`'s doc comment.
    #[test]
    fn continue_turn_records_the_most_recent_turn_ended_reason() {
        let state = judge_test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, _) = crossbeam_channel::unbounded::<Command>();

        live_state.extend_provider_events([
            ProviderEvent::from(Event::TurnEnded(TurnEndReason::HaltedByDoomLoop)),
            ProviderEvent::from(Event::TurnEnded(TurnEndReason::Completed)),
        ]);

        dispatch_inbound_command(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            Command::ContinueTurn,
        );

        let resumed_from = live_state.events().iter().find_map(|event| match event {
            Event::ContinueTurnRequested(ContinueTurnRequested { resumed_from }) => {
                Some(*resumed_from)
            }
            _ => None,
        });
        assert_eq!(resumed_from, Some(TurnEndReason::Completed));
    }

    /// `SESSION_PROTOCOL_VERSION` v16's `Event::ApprovalResolved`:
    /// approving a pending `ApprovalRequested` records the audit row *before*
    /// `resolve_approval` runs, so it exists regardless of which
    /// `ApprovalOutcome` variant (`Executed`/`Started`/`Forward`/
    /// `AlreadyResolved`) the resolve takes. This case exercises the
    /// `Forward` branch (the `mock.approval_required` path: not a
    /// Horizon-executed tool, so the decision is forwarded to the
    /// provider unchanged). The audit row carries the right
    /// `ApprovalDecisionPayload` and the frame's `occurrence_id`, so the
    /// SQL `requested -> resolved` join survives a reused `call_id`.
    #[test]
    fn resolve_and_forward_records_approval_resolved_with_occurrence_id() {
        let state = judge_test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, commands_rx) = crossbeam_channel::unbounded::<Command>();

        let call_id = ToolCallId("approval-fwd".to_string());
        let occurrence_id = OccurrenceId("occ-fwd".to_string());
        live_state.extend_provider_events([
            ProviderEvent::from(Event::ToolCallRequested(ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "mock.approval_required".to_string(),
                input: serde_json::json!({}).into(),
                occurrence_id: Some(occurrence_id.clone()),
            })),
            ProviderEvent::from(Event::ApprovalRequested(ApprovalRequest {
                call_id: call_id.clone(),
                reason: "test".to_string(),
                kind: ApprovalKind::Standard,
                occurrence_id: Some(occurrence_id.clone()),
            })),
        ]);

        dispatch_inbound_command(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            Command::ApproveToolCall {
                call_id: call_id.clone(),
            },
        );

        let resolved = live_state
            .events()
            .iter()
            .find_map(|event| match event {
                Event::ApprovalResolved(ApprovalResolved {
                    call_id: cid,
                    occurrence_id,
                    decision,
                }) => Some((cid.clone(), occurrence_id.clone(), decision.clone())),
                _ => None,
            })
            .expect("an ApprovalResolved event in the live state");
        assert_eq!(resolved.0, call_id);
        assert_eq!(resolved.1, Some(occurrence_id));
        assert!(matches!(resolved.2, ApprovalDecisionPayload::Approve));

        // The `Forward` branch sends the original command to the provider,
        // so the resume path beyond the audit row is unchanged.
        let forwarded = commands_rx
            .try_recv()
            .expect("the original command forwarded");
        match forwarded {
            Command::ApproveToolCall { call_id: cid } => assert_eq!(cid, call_id),
            other => panic!("unexpected forward command: {other:?}"),
        }
    }

    /// The Deny branch carries the user's optional reason string through
    /// to the audit row, so the analyst can see *why* an approval was
    /// rejected (the original incident's report needed this distinction:
    /// the 8 outstanding approvals were a mix of "skip this" and "wrong
    /// tool, don't retry", and the deny reason is what told them apart).
    #[test]
    fn resolve_and_forward_records_deny_with_reason() {
        let state = judge_test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, _) = crossbeam_channel::unbounded::<Command>();

        let call_id = ToolCallId("approval-deny".to_string());
        live_state.extend_provider_events([ProviderEvent::from(Event::ApprovalRequested(
            ApprovalRequest {
                call_id: call_id.clone(),
                reason: "test".to_string(),
                kind: ApprovalKind::Standard,
                occurrence_id: None,
            },
        ))]);

        dispatch_inbound_command(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            Command::DenyToolCall {
                call_id: call_id.clone(),
                reason: Some("wrong tool, try fs.read".to_string()),
            },
        );

        let resolved = live_state.events().iter().find_map(|event| match event {
            Event::ApprovalResolved(ApprovalResolved { decision, .. }) => Some(decision.clone()),
            _ => None,
        });
        match resolved {
            Some(ApprovalDecisionPayload::Deny { reason }) => {
                assert_eq!(reason.as_deref(), Some("wrong tool, try fs.read"));
            }
            other => panic!("expected a Deny decision, got {other:?}"),
        }
    }

    /// The audit row is published to wire subscribers as well as folded
    /// into the live state. `send_session_event` fans to any agent
    /// subscriber on this session id; this is the path the live UI takes
    /// when it isn't replaying from the JSONL log (the resume path uses
    /// the log; the live path uses the channel). Without the channel
    /// emit, the operator would see no immediate feedback in the pane
    /// even though the audit row exists post-hoc.
    #[test]
    fn resolve_and_forward_fans_approval_resolved_to_subscribers() {
        let state = judge_test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, _) = crossbeam_channel::unbounded::<Command>();
        let mut subscriber_rx =
            crate::session::Connection::new(state.clone()).subscribe_agent(session_id);

        let call_id = ToolCallId("approval-fanout".to_string());
        live_state.extend_provider_events([ProviderEvent::from(Event::ApprovalRequested(
            ApprovalRequest {
                call_id: call_id.clone(),
                reason: "test".to_string(),
                kind: ApprovalKind::Standard,
                occurrence_id: None,
            },
        ))]);

        dispatch_inbound_command(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            Command::ApproveToolCall { call_id },
        );

        let drained = drain_events(&mut subscriber_rx);
        assert!(
            drained.iter().any(|event| matches!(
                event,
                Event::ApprovalResolved(ApprovalResolved {
                    decision: ApprovalDecisionPayload::Approve,
                    ..
                })
            )),
            "subscriber must see the ApprovalResolved audit event; got {drained:?}"
        );
    }
}
