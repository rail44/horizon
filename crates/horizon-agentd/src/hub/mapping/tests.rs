use super::*;
use agent_client_protocol::schema::MaybeUndefined;
use horizon_agent::contract::{
    ApprovalDecisionPayload, ApprovalKind, ApprovalResolved, Message, MessageDelta, OccurrenceId,
    ToolCallId, ToolCallIdentity, ToolCallRequest, ToolCallResult, ToolOutcome,
};

fn facts() -> SessionFacts {
    SessionFacts {
        provider_id: "builtin.agent.mock".into(),
        role_id: None,
        parent_session_id: None,
        workspace_root: None,
    }
}

fn mapper() -> Mapper {
    let mut mapper = Mapper::new(SessionId::new(), facts());
    mapper.finish_replay();
    mapper
}

fn updates(mapper: &mut Mapper, events: &[Event]) -> Vec<v2::SessionUpdate> {
    events
        .iter()
        .flat_map(|event| mapper.map(&AgentWireEvent::Event(event.clone())))
        .filter_map(|outgoing| match outgoing {
            Outgoing::Update(update) => Some(update),
            _ => None,
        })
        .collect()
}

fn committed(role: MessageRole, text: &str) -> Event {
    Event::MessageCommitted(Message {
        role,
        text: text.into(),
    })
}

fn delta(text: &str) -> Event {
    Event::AssistantTextDelta(MessageDelta {
        role: MessageRole::Assistant,
        text: text.into(),
    })
}

fn tool_request(call: &str, occurrence: &str, tool_id: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: ToolCallId(call.into()),
        tool_id: tool_id.into(),
        input: serde_json::json!({"command": "true"}).into(),
        occurrence_id: OccurrenceId(occurrence.into()),
    }
}

fn identity(call: &str, occurrence: &str) -> ToolCallIdentity {
    ToolCallIdentity {
        call_id: ToolCallId(call.into()),
        occurrence_id: OccurrenceId(occurrence.into()),
    }
}

fn message_id(update: &v2::SessionUpdate) -> Option<String> {
    Some(
        match update {
            v2::SessionUpdate::UserMessage(message) => &message.message_id,
            v2::SessionUpdate::AgentMessage(message) => &message.message_id,
            v2::SessionUpdate::AgentMessageChunk(chunk) => &chunk.message_id,
            v2::SessionUpdate::AgentThoughtChunk(chunk) => &chunk.message_id,
            _ => return None,
        }
        .0
        .to_string(),
    )
}

#[test]
fn message_ids_follow_commits_and_tool_calls() {
    let mut mapper = mapper();
    let updates = updates(
        &mut mapper,
        &[
            committed(MessageRole::User, "hello"),
            delta("Mock "),
            delta("response"),
            committed(MessageRole::Assistant, "Mock response"),
            Event::ToolCallRequested(tool_request("call-1", "occ-1", "bash")),
            delta("after the tool"),
        ],
    );
    let ids: Vec<_> = updates.iter().filter_map(message_id).collect();
    assert_eq!(ids, ["msg-0", "msg-1", "msg-1", "msg-1", "msg-3"]);
    assert!(
        matches!(&updates[3], v2::SessionUpdate::AgentMessage(message)
        if message.content == MaybeUndefined::Value(vec!["Mock response".to_string().into()]))
    );
}

#[test]
fn the_same_sequence_maps_to_the_same_updates() {
    let events = [
        Event::StateChanged(SessionState::Created),
        committed(MessageRole::User, "hi"),
        Event::StateChanged(SessionState::Running),
        delta("a"),
        committed(MessageRole::Assistant, "a"),
        Event::TurnEnded(TurnEndReason::Completed),
        Event::StateChanged(SessionState::WaitingForUser),
    ];
    let first = updates(&mut mapper(), &events);
    let second = updates(&mut mapper(), &events);
    assert_eq!(first, second);
}

#[test]
fn non_human_user_messages_carry_their_role() {
    let updates = updates(
        &mut mapper(),
        &[committed(MessageRole::TaskNotification, "task done")],
    );
    let v2::SessionUpdate::UserMessage(message) = &updates[0] else {
        panic!("expected a user message, got {updates:?}");
    };
    let meta: acp::MessageMeta = acp::read_horizon_meta(message.meta.as_opt_ref().flatten())
        .unwrap()
        .unwrap();
    assert_eq!(meta.role, acp::MessageRole::TaskNotification);
}

#[test]
fn a_turn_end_is_one_idle_with_its_stop_reason() {
    let updates = updates(
        &mut mapper(),
        &[
            Event::StateChanged(SessionState::Created),
            Event::StateChanged(SessionState::Running),
            Event::StateChanged(SessionState::ToolRunning),
            Event::StateChanged(SessionState::WaitingForApproval),
            Event::StateChanged(SessionState::Running),
            Event::TurnEnded(TurnEndReason::HaltedByDoomLoop),
            Event::StateChanged(SessionState::WaitingForUser),
            Event::StateChanged(SessionState::Terminated),
        ],
    );
    assert_eq!(
        updates,
        vec![
            v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(v2::IdleStateUpdate::new())),
            v2::SessionUpdate::StateUpdate(v2::StateUpdate::Running(v2::RunningStateUpdate::new())),
            v2::SessionUpdate::StateUpdate(v2::StateUpdate::RequiresAction(
                v2::RequiresActionStateUpdate::new()
            )),
            v2::SessionUpdate::StateUpdate(v2::StateUpdate::Running(v2::RunningStateUpdate::new())),
            v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(
                v2::IdleStateUpdate::new().stop_reason(v2::StopReason::Other(
                    acp::STOP_REASON_DOOM_LOOP.to_string()
                ))
            )),
        ]
    );
}

#[test]
fn a_tool_call_is_one_update_series_keyed_by_its_occurrence() {
    let result = ToolCallResult {
        call_id: ToolCallId("call-1".into()),
        occurrence_id: OccurrenceId("occ-1".into()),
        output: serde_json::json!({
            "exit_code": 0,
            "auto_approved": true,
            "policy_tier": "contained"
        })
        .into(),
        outcome: ToolOutcome::Succeeded,
    };
    let updates = updates(
        &mut mapper(),
        &[
            Event::ToolCallRequested(tool_request("call-1", "occ-1", "bash")),
            Event::ToolCallStarted(identity("call-1", "occ-1")),
            Event::ToolCallFinished(result),
        ],
    );
    let calls: Vec<_> = updates
        .iter()
        .map(|update| match update {
            v2::SessionUpdate::ToolCallUpdate(call) => call.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert!(calls.iter().all(|call| &*call.tool_call_id.0 == "occ-1"));
    assert_eq!(calls[0].kind, MaybeUndefined::Value(v2::ToolKind::Execute));
    assert_eq!(
        calls[0].status,
        MaybeUndefined::Value(v2::ToolCallStatus::Pending)
    );
    assert_eq!(
        calls[1].status,
        MaybeUndefined::Value(v2::ToolCallStatus::InProgress)
    );
    assert_eq!(
        calls[2].status,
        MaybeUndefined::Value(v2::ToolCallStatus::Completed)
    );
    let meta: acp::ToolCallMeta = acp::read_horizon_meta(calls[2].meta.as_opt_ref().flatten())
        .unwrap()
        .unwrap();
    assert_eq!(
        meta,
        acp::ToolCallMeta {
            call_id: "call-1".into(),
            tool_id: "bash".into(),
            outcome: Some(acp::ToolOutcome::Succeeded),
            auto_approved: Some(true),
            policy_tier: Some("contained".into()),
        }
    );
}

fn approval(occurrence: &str) -> Event {
    Event::ApprovalRequested(ApprovalRequest {
        call_id: ToolCallId("call-1".into()),
        occurrence_id: OccurrenceId(occurrence.into()),
        reason: "bash needs approval".into(),
        kind: ApprovalKind::Standard,
    })
}

#[test]
fn replayed_approvals_are_asked_after_the_bootstrap_only_while_pending() {
    let mut mapper = Mapper::new(SessionId::new(), facts());
    let mut asked = Vec::new();
    for event in [
        approval("occ-1"),
        approval("occ-2"),
        Event::ApprovalResolved(ApprovalResolved {
            call_id: ToolCallId("call-1".into()),
            occurrence_id: OccurrenceId("occ-1".into()),
            decision: ApprovalDecisionPayload::Approve,
        }),
    ] {
        for outgoing in mapper.map(&AgentWireEvent::Event(event)) {
            if let Outgoing::AskPermission(request) = outgoing {
                asked.push(request);
            }
        }
    }
    assert!(asked.is_empty(), "nothing is asked while replaying");
    let pending = mapper.finish_replay();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].occurrence_id.0, "occ-2");

    let live = mapper.map(&AgentWireEvent::Event(approval("occ-3")));
    assert!(matches!(&live[..], [Outgoing::AskPermission(request)]
        if request.occurrence_id.0 == "occ-3"));
    let settled = mapper.map(&AgentWireEvent::Event(Event::ToolCallFinished(
        ToolCallResult::cancelled(identity("call-1", "occ-3")),
    )));
    assert!(settled.iter().any(|outgoing| matches!(outgoing,
        Outgoing::ApprovalSettled(occurrence) if occurrence == "occ-3")));
}

#[test]
fn model_announcements_become_one_select_option() {
    let mut mapper = mapper();
    assert!(
        mapper
            .map(&AgentWireEvent::SessionModel("m-aggregate".into()))
            .len()
            == 1
    );
    let outgoing = mapper.map(&AgentWireEvent::SessionSelection(ModelSelection {
        provider: "moa".into(),
        model: "mix".into(),
    }));
    let [Outgoing::Update(v2::SessionUpdate::ConfigOptionUpdate(update))] = &outgoing[..] else {
        panic!("expected one config option update, got {outgoing:?}");
    };
    assert_eq!(update.config_options, vec![model_option("moa", "mix")]);
    let option = &update.config_options[0];
    assert_eq!(&*option.config_id.0, acp::MODEL_CONFIG_ID);
    assert_eq!(
        option.category,
        Some(v2::SessionConfigOptionCategory::Model)
    );
    let v2::SessionConfigKind::Select(select) = &option.kind else {
        panic!("expected a select option");
    };
    assert_eq!(&*select.current_value.0, "moa/mix");
}
