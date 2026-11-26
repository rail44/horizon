//! The approval seam on the host side: gating a policy-generated prompt
//! behind the judge, emitting the human prompt when one is owed, and
//! resolving an inbound approve/deny into provider commands.

use std::sync::Arc;

use crossbeam_channel::Sender;

use horizon_agent::contract::{ApprovalDecisionPayload, Command, Event, SessionId, ToolCallId};
use horizon_agent::hosting::AgentWireEvent;
use horizon_agent::live::LiveState;
use horizon_agent::tools::gate::GateStep;
use horizon_agent::tools::{ApprovalDecision, ApprovalOutcome, resolve_approval};

use super::events::{
    apply_and_send_session_events, persist_and_send_session_event, report_persistence_failure,
    send_session_event,
};
use super::state::AgentdState;

/// Carries out what the verdict stage's gate did with a call
/// (`horizon_agent::tools::gate`): a prompt's events, already in the live
/// state, go to the subscribers; a decision goes out as any approval's
/// outcome does.
pub(super) fn deliver_gate_step(
    state: &Arc<AgentdState>,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    call_id: ToolCallId,
    step: GateStep,
) {
    match step {
        GateStep::Pending => {}
        GateStep::Asked(events) => {
            for event in events {
                send_session_event(state, session_id, AgentWireEvent::Event(event));
            }
        }
        GateStep::Decided(outcome) => {
            forward_approval_outcome(state, commands_tx, session_id, call_id, outcome)
        }
    }
}

/// Reissues `request` as a new attempt that needs `kind` approved
/// (`gate::reissue`), publishes what that recorded, tells the provider, and
/// carries out what the gate did with it.
pub(super) fn begin_reissued_approval(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    session_id: SessionId,
    request: horizon_agent::contract::ToolCallRequest,
    kind: horizon_agent::contract::ApprovalKind,
    reason: String,
    commands_tx: &Sender<Command>,
) {
    let call_id = request.call_id.clone();
    match horizon_agent::tools::gate::reissue(session_id, live_state, request, kind, reason) {
        Ok(reissued) => {
            for event in reissued.events {
                send_session_event(state, session_id, AgentWireEvent::Event(event));
            }
            let _ = commands_tx.send(reissued.command);
            deliver_gate_step(state, commands_tx, session_id, call_id, reissued.step);
        }
        Err(message) => report_persistence_failure(state, live_state, session_id, message),
    }
}

/// A `Command` envelope arriving from Horizon for this session.
/// `ApproveToolCall`/`DenyToolCall` are resolved right here (decision 2:
/// "Approval decisions stay in Horizon... resolved in agentd") via
/// `tools::approval::resolve_approval`. Every other command forwards
/// straight to the provider. (An earlier in-process shell
/// shared this helper from its own click handler; that path retired with
/// the runtime split.)
pub(super) fn dispatch_inbound_command(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    command: Command,
) {
    if !super::events::execution_available(state, live_state, session_id) {
        let _ = commands_tx.send(Command::Shutdown);
        return;
    }
    match command {
        command @ (Command::Cancel { .. } | Command::Shutdown) => {
            // Close the accepted executions before forwarding the stop. A
            // worker/judge racing this command can no longer reissue them.
            let frame = live_state.frame();
            for request in frame.unfinished_tool_calls() {
                horizon_agent::tools::cancel_tool_execution(session_id, &request.identity());
                let event = Event::ToolCallFinished(
                    horizon_agent::tools::cancelled_tool_call_result(request.identity()),
                );
                if !apply_and_send_session_events(state, live_state, session_id, vec![event.into()])
                {
                    return;
                }
            }
            let _ = commands_tx.send(command);
        }
        Command::SetSessionModel { provider, model } => {
            match super::model_selection::resolve(state, &provider, &model) {
                Ok(command) => {
                    // Kept on the session's branch, so a session started
                    // again over it switches again (`model_selection::restore`).
                    let selected = Event::ModelSelected { provider, model };
                    if persist_and_send_session_event(state, live_state, session_id, selected) {
                        let _ = commands_tx.send(command);
                    }
                }
                Err(message) => send_session_event(
                    state,
                    session_id,
                    AgentWireEvent::Event(Event::Error(horizon_agent::contract::Error { message })),
                ),
            }
        }
        Command::SetSessionReasoning { level } => {
            let selected = Event::ReasoningSelected { level };
            if persist_and_send_session_event(state, live_state, session_id, selected) {
                let _ = commands_tx.send(Command::SetSessionReasoning { level });
            }
        }
        Command::SendSessionInput {
            session_id: recipient,
            input,
        } => {
            super::input::send_input(state, live_state, session_id, recipient, input);
        }
        command @ Command::UserMessage { .. } => {
            super::input::checkpoint_a_settled_position(state, live_state, session_id);
            let _ = commands_tx.send(command);
        }
        Command::SessionInput(input) => {
            if super::input::accept_input(state, live_state, session_id, &input) {
                let _ = commands_tx.send(Command::SessionInput(input));
            }
        }
        Command::AcknowledgeDelivery { delivery_id } => {
            super::input::acknowledge_delivery(state, live_state, session_id, delivery_id);
        }

        Command::ApproveToolCall { identity } => resolve_and_forward(
            state,
            live_state,
            commands_tx,
            session_id,
            identity,
            ApprovalDecision::Approve,
        ),
        Command::DenyToolCall { identity, reason } => resolve_and_forward(
            state,
            live_state,
            commands_tx,
            session_id,
            identity,
            ApprovalDecision::Deny { reason },
        ),
        other => {
            let _ = commands_tx.send(other);
        }
    }
}

fn resolve_and_forward(
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    identity: horizon_agent::contract::ToolCallIdentity,
    decision: ApprovalDecision,
) {
    let frame = live_state.frame();
    if frame.actionable_approval(&identity).is_none() {
        return;
    }
    let logged_call_id = identity.call_id.clone();
    let resolved = Event::ApprovalResolved(horizon_agent::contract::ApprovalResolved {
        call_id: identity.call_id.clone(),
        occurrence_id: identity.occurrence_id.clone(),
        decision: approval_decision_payload(&decision),
    });
    if !apply_and_send_session_events(state, live_state, session_id, vec![resolved.into()]) {
        return;
    }
    // Resolve against the validated snapshot preceding this decision's acknowledgement.
    let outcome = resolve_approval(&frame, session_id, identity, decision);
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
    state: &Arc<AgentdState>,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    logged_call_id: ToolCallId,
    outcome: ApprovalOutcome,
) {
    match outcome {
        ApprovalOutcome::Applied(update) => {
            super::completion::publish_tool_update(state, commands_tx, session_id, Ok(update));
        }
        ApprovalOutcome::PersistenceFailed(_) => {
            let _ = commands_tx.send(Command::Shutdown);
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
        // visible in the agent process's own stderr.
        ApprovalOutcome::AlreadyResolved => {
            eprintln!(
                "horizon-agent: dropped duplicate approve/deny for session {session_id:?}, \
                 call {logged_call_id:?} (already resolved)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::{drain_events, judge_candidate, test_state};
    use horizon_agent::contract::{
        ApprovalDecisionPayload, ApprovalKind, ApprovalRequest, ApprovalResolved, OccurrenceId,
        ProviderEvent, ToolCallRequest,
    };
    use horizon_agent::live::LiveState;

    #[test]
    fn cancellation_closes_every_retry_attempt_before_a_late_completion_arrives() {
        let state = test_state();
        let live = LiveState::with_disabled_persistence();
        let session = SessionId::new();
        let (commands, received) = crossbeam_channel::unbounded();
        let first = judge_candidate("cancel-retry").request;
        let retry = ToolCallRequest {
            occurrence_id: OccurrenceId::new(),
            ..first.clone()
        };
        live.extend_provider_events([
            Event::ToolCallRequested(first.clone()).into(),
            Event::ToolCallStarted(first.identity()).into(),
            Event::ToolCallRequested(retry.clone()).into(),
            Event::ApprovalRequested(ApprovalRequest {
                call_id: retry.call_id.clone(),
                occurrence_id: retry.occurrence_id.clone(),
                reason: "retry".into(),
                kind: ApprovalKind::Standard,
            })
            .into(),
        ])
        .unwrap();
        dispatch_inbound_command(
            &state,
            &live,
            &commands,
            session,
            Command::Cancel { request_id: None },
        );
        std::assert_matches!(received.try_recv(), Ok(Command::Cancel { .. }));
        assert!(live.frame().unfinished_tool_calls().is_empty());
        assert!(live.frame().pending_approval_call_id().is_none());
        let cancelled = live.events();
        let results = cancelled
            .iter()
            .filter_map(|event| match event {
                Event::ToolCallFinished(result) => Some(result),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].occurrence_id, first.occurrence_id);
        assert_eq!(results[1].occurrence_id, retry.occurrence_id);
        super::super::completion::fold_tool_completion(
            &state,
            &live,
            &commands,
            session,
            horizon_agent::tools::ToolCompletion::DomainGrantRequired {
                call_id: retry.call_id,
                occurrence_id: retry.occurrence_id,
                domains: vec!["example.test".into()],
            },
        );
        assert_eq!(live.events(), cancelled);
        assert!(received.try_recv().is_err());
    }

    /// `SESSION_PROTOCOL_VERSION` v16's `Event::ApprovalResolved`:
    /// approving a pending `ApprovalRequested` records the audit row *before*
    /// `resolve_approval` runs, so it exists regardless of which
    /// `ApprovalOutcome` variant (`Executed`/`Started`/`Forward`/
    /// `AlreadyResolved`) the resolve takes. This case exercises the
    /// `Forward` branch (the `mock_approval_required` path: not a
    /// Horizon-executed tool, so the decision is forwarded to the
    /// provider unchanged). The audit row carries the right
    /// `ApprovalDecisionPayload` and the frame's `occurrence_id`, so the
    /// SQL `requested -> resolved` join survives a reused `call_id`.
    #[test]
    fn resolve_and_forward_records_approval_resolved_with_occurrence_id() {
        let state = test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, commands_rx) = crossbeam_channel::unbounded::<Command>();

        let call_id = ToolCallId("approval-fwd".to_string());
        let occurrence_id = OccurrenceId("occ-fwd".to_string());
        live_state
            .extend_provider_events([
                ProviderEvent::from(Event::ToolCallRequested(ToolCallRequest {
                    call_id: call_id.clone(),
                    tool_id: "mock_approval_required".to_string(),
                    input: serde_json::json!({}).into(),
                    occurrence_id: occurrence_id.clone(),
                })),
                ProviderEvent::from(Event::ApprovalRequested(ApprovalRequest {
                    call_id: call_id.clone(),
                    reason: "test".to_string(),
                    kind: ApprovalKind::Standard,
                    occurrence_id: occurrence_id.clone(),
                })),
            ])
            .unwrap();

        dispatch_inbound_command(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            Command::ApproveToolCall {
                identity: horizon_agent::contract::ToolCallIdentity {
                    call_id: call_id.clone(),
                    occurrence_id: occurrence_id.clone(),
                },
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
        assert_eq!(resolved.1, occurrence_id);
        std::assert_matches!(resolved.2, ApprovalDecisionPayload::Approve);

        // The `Forward` branch sends the original command to the provider,
        // so the resume path beyond the audit row is unchanged.
        let forwarded = commands_rx
            .try_recv()
            .expect("the original command forwarded");
        match forwarded {
            Command::ApproveToolCall { identity } => assert_eq!(identity.call_id, call_id),
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
        let state = test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, _) = crossbeam_channel::unbounded::<Command>();

        let call_id = ToolCallId("approval-deny".to_string());
        let occurrence_id = OccurrenceId(call_id.0.clone());
        live_state
            .extend_provider_events([Event::ToolCallRequested(
                horizon_agent::contract::ToolCallRequest {
                    call_id: call_id.clone(),
                    occurrence_id: horizon_agent::contract::OccurrenceId(call_id.0.clone()),
                    tool_id: "mock_approval_required".into(),
                    input: serde_json::json!({}).into(),
                },
            )
            .into()])
            .unwrap();
        live_state
            .extend_provider_events([ProviderEvent::from(Event::ApprovalRequested(
                ApprovalRequest {
                    call_id: call_id.clone(),
                    reason: "test".to_string(),
                    kind: ApprovalKind::Standard,
                    occurrence_id: horizon_agent::contract::OccurrenceId(call_id.0.clone()),
                },
            ))])
            .unwrap();

        dispatch_inbound_command(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            Command::DenyToolCall {
                identity: horizon_agent::contract::ToolCallIdentity {
                    call_id: call_id.clone(),
                    occurrence_id: occurrence_id.clone(),
                },
                reason: Some("wrong tool, try fs_read".to_string()),
            },
        );

        let resolved = live_state.events().iter().find_map(|event| match event {
            Event::ApprovalResolved(ApprovalResolved { decision, .. }) => Some(decision.clone()),
            _ => None,
        });
        match resolved {
            Some(ApprovalDecisionPayload::Deny { reason }) => {
                assert_eq!(reason.as_deref(), Some("wrong tool, try fs_read"));
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
        let state = test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, _) = crossbeam_channel::unbounded::<Command>();
        let mut subscriber_rx =
            crate::session::Connection::new(state.clone()).subscribe_agent(session_id);

        let call_id = ToolCallId("approval-fanout".to_string());
        let occurrence_id = OccurrenceId(call_id.0.clone());
        live_state
            .extend_provider_events([Event::ToolCallRequested(
                horizon_agent::contract::ToolCallRequest {
                    call_id: call_id.clone(),
                    occurrence_id: horizon_agent::contract::OccurrenceId(call_id.0.clone()),
                    tool_id: "mock_approval_required".into(),
                    input: serde_json::json!({}).into(),
                },
            )
            .into()])
            .unwrap();
        live_state
            .extend_provider_events([ProviderEvent::from(Event::ApprovalRequested(
                ApprovalRequest {
                    call_id: call_id.clone(),
                    reason: "test".to_string(),
                    kind: ApprovalKind::Standard,
                    occurrence_id: horizon_agent::contract::OccurrenceId(call_id.0.clone()),
                },
            ))])
            .unwrap();

        dispatch_inbound_command(
            &state,
            &live_state,
            &commands_tx,
            session_id,
            Command::ApproveToolCall {
                identity: horizon_agent::contract::ToolCallIdentity {
                    call_id,
                    occurrence_id,
                },
            },
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

    #[test]
    fn stale_and_duplicate_human_decisions_never_consume_a_new_occurrence_even_after_replay() {
        let state = test_state();
        let session = SessionId::new();
        let first = judge_candidate("reused");
        let mut retry = first.clone();
        retry.request.occurrence_id = OccurrenceId::new();
        retry.approval.occurrence_id = retry.request.occurrence_id.clone();
        let live = LiveState::with_disabled_persistence();
        live.extend_provider_events([
            Event::ToolCallRequested(first.request.clone()).into(),
            Event::ApprovalRequested(first.approval.clone()).into(),
            Event::ToolCallRequested(retry.request.clone()).into(),
            Event::ApprovalRequested(retry.approval.clone()).into(),
        ])
        .unwrap();
        let before = live.events();
        let (tx, rx) = crossbeam_channel::unbounded();
        for command in [
            Command::ApproveToolCall {
                identity: first.request.identity(),
            },
            Command::DenyToolCall {
                identity: first.request.identity(),
                reason: None,
            },
        ] {
            dispatch_inbound_command(&state, &live, &tx, session, command);
        }
        assert_eq!(live.events(), before);
        assert!(rx.try_recv().is_err());
        assert!(
            live.frame()
                .actionable_approval(&retry.request.identity())
                .is_some()
        );
        let approve = Command::ApproveToolCall {
            identity: retry.request.identity(),
        };
        dispatch_inbound_command(&state, &live, &tx, session, approve.clone());
        assert_eq!(rx.try_recv().unwrap(), approve);
        let accepted = live.events();
        // Reconstruct the frame from the saved log before the provider acknowledges the command.
        let replay = LiveState::with_disabled_persistence();
        replay
            .extend_provider_events(accepted.iter().cloned().map(Into::into))
            .unwrap();
        dispatch_inbound_command(&state, &replay, &tx, session, approve);
        dispatch_inbound_command(
            &state,
            &replay,
            &tx,
            session,
            Command::DenyToolCall {
                identity: retry.request.identity(),
                reason: None,
            },
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(replay.events(), accepted);
        assert!(
            replay
                .frame()
                .actionable_approval(&retry.request.identity())
                .is_none()
        );
    }

    #[test]
    fn a_human_command_without_a_displayed_approval_cannot_authorize_a_tool() {
        let state = test_state();
        let session = SessionId::new();
        let candidate = judge_candidate("unsolicited");
        let live = LiveState::with_disabled_persistence();
        live.extend_provider_events([Event::ToolCallRequested(candidate.request.clone()).into()])
            .unwrap();
        let before = live.events();
        let (tx, rx) = crossbeam_channel::unbounded();
        dispatch_inbound_command(
            &state,
            &live,
            &tx,
            session,
            Command::ApproveToolCall {
                identity: candidate.request.identity(),
            },
        );
        assert_eq!(live.events(), before);
        assert!(rx.try_recv().is_err());
    }
}
