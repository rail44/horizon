//! The approval seam on the daemon side: gating a policy-generated prompt
//! behind the judge, emitting the human prompt when one is owed, and
//! resolving an inbound approve/deny into provider commands.

use std::sync::Arc;

use crossbeam_channel::Sender;

use horizon_agent::contract::{
    ApprovalDecisionPayload, ApprovalRequest, Command, ContinueTurnRequested, Event, OccurrenceId,
    ProviderEvent, SessionId, SessionState, ToolCallId,
};
use horizon_agent::live::LiveState;
use horizon_agent::tools::{
    resolve_approval, should_fold_completion, start_approval_gate, unattended_refusal_result,
    ApprovalCandidate, ApprovalDecision, ApprovalGate, ApprovalOutcome, ToolSessionState,
};
use horizon_agent::wire::AgentWireEvent;

use super::events::send_session_event;
use super::state::AgentdState;

/// Intercepts the policy-generated human prompt after its kind/reason are
/// fully derived. The original tool request remains foldable immediately;
/// only the prompt and waiting state are held while the asynchronous judge
/// runs.
///
/// A session nobody can approve for (`ToolSessionState::is_unattended`)
/// never reaches the prompt at all: once the judge has declined to decide
/// on its own, the call resolves as a refused tool result the model can act
/// on, and the turn continues.
pub(super) fn gate_processing_approval(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    events: &mut Vec<ProviderEvent>,
    provider_commands: &mut Vec<Command>,
) {
    gate_processing_approval_with(tool_state, events, provider_commands, |candidate| {
        start_approval_gate(session_id, candidate)
    });
}

fn gate_processing_approval_with(
    tool_state: &ToolSessionState,
    events: &mut Vec<ProviderEvent>,
    provider_commands: &mut Vec<Command>,
    start_gate: impl FnOnce(ApprovalCandidate) -> ApprovalGate,
) {
    let request = events
        .iter()
        .filter_map(ProviderEvent::as_event)
        .find_map(|event| match event {
            Event::ToolCallRequested(request) => Some(request.clone()),
            _ => None,
        });
    let approval =
        events
            .iter()
            .filter_map(ProviderEvent::as_event)
            .find_map(|event| match event {
                Event::ApprovalRequested(approval) => Some(approval.clone()),
                _ => None,
            });
    let (Some(request), Some(approval)) = (request, approval) else {
        return;
    };
    let call_id = approval.call_id.clone();
    let candidate = ApprovalCandidate { request, approval };
    match start_gate(candidate) {
        ApprovalGate::Pending => withhold_prompt(events, &call_id),
        ApprovalGate::Human(candidate) => {
            let Some(result) = unattended_refusal_result(tool_state, &candidate.request) else {
                return;
            };
            withhold_prompt(events, &call_id);
            events.push(ProviderEvent::from(Event::ToolCallFinished(result.clone())));
            provider_commands.push(Command::ToolCallResult(result));
        }
    }
}

/// Drops the prompt and the waiting state from this batch, leaving the tool
/// request itself foldable.
fn withhold_prompt(events: &mut Vec<ProviderEvent>, call_id: &ToolCallId) {
    events.retain(|event| {
        !matches!(
            event.as_event(),
            Some(Event::ApprovalRequested(approval)) if &approval.call_id == call_id
        ) && !matches!(
            event.as_event(),
            Some(Event::StateChanged(SessionState::WaitingForApproval))
        )
    });
}

pub(super) fn emit_human_approval(
    state: &Arc<AgentdState>,
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
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    session_id: SessionId,
    mut request: horizon_agent::contract::ToolCallRequest,
    kind: horizon_agent::contract::ApprovalKind,
    reason: String,
    commands_tx: &Sender<Command>,
) {
    let prior_identity = request.identity();
    request.occurrence_id = OccurrenceId::new();
    if matches!(
        kind,
        horizon_agent::contract::ApprovalKind::DomainGrant { .. }
    ) {
        // The fetch worker stopped at the next domain boundary. Close that
        // attempt before opening the retry; no provider answer exists yet.
        let event = Event::ToolCallFinished(
            prior_identity
                .result(serde_json::json!({}))
                .superseded_by_retry(&request.occurrence_id),
        );
        live_state.extend_provider_events([event.clone().into()]);
        send_session_event(state, session_id, AgentWireEvent::Event(event));
    }
    let approval = ApprovalRequest {
        call_id: request.call_id.clone(),
        occurrence_id: request.occurrence_id.clone(),
        kind,
        reason,
    };
    let _ = commands_tx.send(Command::ToolCallReissued(request.identity()));
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
    state: &Arc<AgentdState>,
    live_state: &LiveState,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    command: Command,
) {
    match command {
        command @ (Command::Cancel { .. } | Command::Shutdown) => {
            // Close the accepted executions before forwarding the stop. A
            // worker/judge racing this command can no longer reissue them.
            let frame = live_state.frame();
            for request in frame.unfinished_tool_calls() {
                horizon_agent::tools::cancel_tool_execution(session_id, &request.call_id);
                let event = Event::ToolCallFinished(
                    horizon_agent::tools::cancelled_tool_call_result(request.identity()),
                );
                let _ = live_state.extend_provider_events(std::iter::once(event.clone().into()));
                send_session_event(state, session_id, AgentWireEvent::Event(event));
            }
            let _ = commands_tx.send(command);
        }
        Command::SetSessionModel { provider, model } => {
            match super::model_selection::resolve(state, &provider, &model) {
                Ok(command) => {
                    let _ = commands_tx.send(command);
                }
                Err(message) => send_session_event(
                    state,
                    session_id,
                    AgentWireEvent::Event(Event::Error(horizon_agent::contract::Error { message })),
                ),
            }
        }
        Command::SendSessionInput {
            session_id: recipient,
            input,
        } => {
            super::input::send_input(state, live_state, session_id, recipient, input);
        }
        Command::SessionInput(input) => {
            if super::input::accept_input(state, live_state, session_id, &input) {
                let _ = commands_tx.send(Command::SessionInput(input));
            }
        }
        Command::AcknowledgeDelivery { delivery_id } => {
            super::input::acknowledge_delivery(state, live_state, session_id, delivery_id);
        }

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
            // real halt case); the no-op replay / idle-session case leaves
            // it `None` per `Event::ContinueTurnRequested`'s doc comment,
            // so a non-zero no-reason count from analytics still surfaces
            // UI races.
            let resumed_from = live_state.frame().last_turn_end_reason();
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
    state: &Arc<AgentdState>,
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
    let Some(request) = frame.tool_call_request(&logged_call_id) else {
        return;
    };
    let occurrence_id = request.occurrence_id.clone();
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
    state: &Arc<AgentdState>,
    commands_tx: &Sender<Command>,
    session_id: SessionId,
    logged_call_id: ToolCallId,
    outcome: ApprovalOutcome,
) {
    match outcome {
        ApprovalOutcome::Applied(update) => {
            super::completion::publish_tool_update(state, commands_tx, session_id, update);
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
    use crate::session::test_support::{drain_events, judge_candidate, test_state};
    use horizon_agent::contract::{
        ApprovalDecisionPayload, ApprovalKind, ApprovalRequest, ApprovalResolved,
        ContinueTurnRequested, OccurrenceId, ProviderEvent, ToolCallRequest, TurnEndReason,
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
        ]);
        dispatch_inbound_command(
            &state,
            &live,
            &commands,
            session,
            Command::Cancel { request_id: None },
        );
        assert!(matches!(received.try_recv(), Ok(Command::Cancel { .. })));
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

    /// A session state with a real, canonical workspace root — what the
    /// out-of-root refusal needs in order to name anything.
    fn rooted_tool_state(root: &std::path::Path, unattended: bool) -> ToolSessionState {
        horizon_agent::tools::ToolSessionBuilder::for_root(
            root.to_path_buf(),
            horizon_agent::config::AgentToolsConfig::default(),
            horizon_agent::tools::RecallContext::default(),
        )
        .with_unattended(unattended)
        .build()
    }

    fn out_of_root_read_events(
        call_id: &str,
        path: &std::path::Path,
    ) -> (ToolCallRequest, Vec<ProviderEvent>) {
        let request = ToolCallRequest {
            call_id: ToolCallId(call_id.to_string()),
            tool_id: "fs.read".to_string(),
            input: serde_json::json!({ "path": path.display().to_string() }).into(),
            occurrence_id: horizon_agent::contract::OccurrenceId(
                (ToolCallId(call_id.to_string())).0.clone(),
            ),
        };
        let events = vec![
            ProviderEvent::from(Event::ToolCallRequested(request.clone())),
            ProviderEvent::from(Event::ApprovalRequested(ApprovalRequest {
                call_id: request.call_id.clone(),
                occurrence_id: horizon_agent::contract::OccurrenceId(request.call_id.0.clone()),
                reason: "outside the workspace root".to_string(),
                kind: ApprovalKind::Standard,
            })),
            ProviderEvent::from(Event::StateChanged(SessionState::WaitingForApproval)),
        ];
        (request, events)
    }

    #[test]
    fn gate_suppresses_prompt_while_pending_and_preserves_human_fallback() {
        let tool_state = rooted_tool_state(&std::env::temp_dir(), false);
        let candidate = judge_candidate("gate-shape");
        let original = vec![
            ProviderEvent::from(Event::ToolCallRequested(candidate.request.clone())),
            ProviderEvent::from(Event::ApprovalRequested(candidate.approval.clone())),
            ProviderEvent::from(Event::StateChanged(SessionState::WaitingForApproval)),
        ];

        let mut pending = original.clone();
        let mut commands = Vec::new();
        gate_processing_approval_with(&tool_state, &mut pending, &mut commands, |observed| {
            assert_eq!(observed, candidate);
            ApprovalGate::Pending
        });
        assert_eq!(pending.len(), 1);
        assert!(matches!(
            pending[0].clone().into_event().expect("conversation event"),
            Event::ToolCallRequested(_)
        ));
        assert!(commands.is_empty());

        let mut human = original.clone();
        gate_processing_approval_with(&tool_state, &mut human, &mut commands, |candidate| {
            ApprovalGate::Human(Box::new(candidate))
        });
        assert_eq!(human, original);
        assert!(commands.is_empty());
    }

    /// An unattended session (a `task` child or a Mixture-of-Agents
    /// proposer) never parks: the prompt is withheld and the call resolves
    /// as an error result the model can act on, so the turn continues. The
    /// message names the root the session may read.
    #[test]
    fn an_unattended_session_is_refused_instead_of_prompted() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let tool_state = rooted_tool_state(&root, true);
        let (request, mut events) =
            out_of_root_read_events("unattended-read", std::path::Path::new("/etc/hostname"));
        let mut commands = Vec::new();

        gate_processing_approval_with(&tool_state, &mut events, &mut commands, |candidate| {
            ApprovalGate::Human(Box::new(candidate))
        });

        assert!(
            !events.iter().any(|event| matches!(
                event.clone().into_event().expect("conversation event"),
                Event::ApprovalRequested(_) | Event::StateChanged(SessionState::WaitingForApproval)
            )),
            "no prompt may survive for a session nobody watches: {events:?}"
        );
        let result = events
            .iter()
            .find_map(
                |event| match &event.clone().into_event().expect("conversation event") {
                    Event::ToolCallFinished(result) => Some(result.clone()),
                    _ => None,
                },
            )
            .expect("the call resolves with a result of its own");
        assert_eq!(result.call_id, request.call_id);
        assert!(result.is_error());
        let message = result.output["message"].as_str().unwrap().to_string();
        assert!(message.contains(&root.display().to_string()), "{message}");
        assert!(
            matches!(commands.as_slice(), [Command::ToolCallResult(forwarded)] if forwarded.call_id == request.call_id),
            "the model must receive the refusal as this call's result: {commands:?}"
        );
    }

    /// The judge stays in the path for an unattended session: a candidate
    /// it accepts is still pending its verdict, not refused here.
    #[test]
    fn a_judge_that_takes_the_call_refuses_nothing_in_an_unattended_session() {
        let tool_state = rooted_tool_state(&std::env::temp_dir(), true);
        let (_, mut events) =
            out_of_root_read_events("unattended-judged", std::path::Path::new("/etc/hostname"));
        let mut commands = Vec::new();

        gate_processing_approval_with(&tool_state, &mut events, &mut commands, |_| {
            ApprovalGate::Pending
        });

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].clone().into_event().expect("conversation event"),
            Event::ToolCallRequested(_)
        ));
        assert!(
            commands.is_empty(),
            "a call the judge took must wait for its verdict, not resolve here"
        );
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
        let state = test_state();
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
        assert_eq!(resolved, Some(TurnEndReason::HaltedByIterationCap));

        // The original command is forwarded to the provider unchanged so
        // the resume itself still happens.
        assert!(matches!(
            commands_rx.try_recv().expect("the command is forwarded"),
            Command::ContinueTurn
        ));
    }

    /// The no-op replay case documented on
    /// `Event::ContinueTurnRequested::resumed_from`: a `ContinueTurn` sent
    /// to a session whose frame has no `TurnEnded` records `resumed_from:
    /// None`, not a panic and not a silent skip, so analytics can count
    /// the attempt.
    #[test]
    fn continue_turn_records_none_when_no_halt_exists() {
        let state = test_state();
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
        assert_eq!(resumed_from, Some(None));
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
        let state = test_state();
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
        assert_eq!(resumed_from, Some(Some(TurnEndReason::Completed)));
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
        let state = test_state();
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
                occurrence_id: occurrence_id.clone(),
            })),
            ProviderEvent::from(Event::ApprovalRequested(ApprovalRequest {
                call_id: call_id.clone(),
                reason: "test".to_string(),
                kind: ApprovalKind::Standard,
                occurrence_id: occurrence_id.clone(),
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
        assert_eq!(resolved.1, occurrence_id);
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
        let state = test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, _) = crossbeam_channel::unbounded::<Command>();

        let call_id = ToolCallId("approval-deny".to_string());
        live_state.extend_provider_events([Event::ToolCallRequested(
            horizon_agent::contract::ToolCallRequest {
                call_id: call_id.clone(),
                occurrence_id: horizon_agent::contract::OccurrenceId(call_id.0.clone()),
                tool_id: "mock.approval_required".into(),
                input: serde_json::json!({}).into(),
            },
        )
        .into()]);
        live_state.extend_provider_events([ProviderEvent::from(Event::ApprovalRequested(
            ApprovalRequest {
                call_id: call_id.clone(),
                reason: "test".to_string(),
                kind: ApprovalKind::Standard,
                occurrence_id: horizon_agent::contract::OccurrenceId(call_id.0.clone()),
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
        let state = test_state();
        let session_id = SessionId::new();
        let live_state = LiveState::with_disabled_persistence();
        let (commands_tx, _) = crossbeam_channel::unbounded::<Command>();
        let mut subscriber_rx =
            crate::session::Connection::new(state.clone()).subscribe_agent(session_id);

        let call_id = ToolCallId("approval-fanout".to_string());
        live_state.extend_provider_events([Event::ToolCallRequested(
            horizon_agent::contract::ToolCallRequest {
                call_id: call_id.clone(),
                occurrence_id: horizon_agent::contract::OccurrenceId(call_id.0.clone()),
                tool_id: "mock.approval_required".into(),
                input: serde_json::json!({}).into(),
            },
        )
        .into()]);
        live_state.extend_provider_events([ProviderEvent::from(Event::ApprovalRequested(
            ApprovalRequest {
                call_id: call_id.clone(),
                reason: "test".to_string(),
                kind: ApprovalKind::Standard,
                occurrence_id: horizon_agent::contract::OccurrenceId(call_id.0.clone()),
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
