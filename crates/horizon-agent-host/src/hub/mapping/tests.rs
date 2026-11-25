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
        modes: Vec::new(),
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
            Outgoing::Update(update) => Some(*update),
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
    std::assert_matches!(
        updates.iter().find(|update| matches!(update, v2::SessionUpdate::AgentMessage(_))),
        Some(v2::SessionUpdate::AgentMessage(message))
        if message.content == MaybeUndefined::Value(vec!["Mock response".to_string().into()])
    );
}

fn input_accepted(id: &str) -> Event {
    Event::InputAccepted(
        serde_json::from_value(serde_json::json!({"id": id, "origin": "user", "text": id}))
            .unwrap(),
    )
}

fn input_finished(id: &str) -> Event {
    Event::InputOutcome(horizon_agent::contract::SessionInputOutcome {
        delivery_id: format!("result:{id}"),
        input_ids: vec![id.into()],
        reply_to: None,
        outcome: horizon_agent::contract::InputResult::Interrupted,
    })
}

/// One whole turn for input `id`, from its acceptance to its outcome.
fn turn(id: &str) -> Vec<Event> {
    vec![
        input_accepted(id),
        Event::InputStarted(vec![id.into()]),
        committed(MessageRole::User, id),
        committed(MessageRole::Assistant, "done"),
        Event::TurnEnded(TurnEndReason::Completed),
        input_finished(id),
    ]
}

/// The cursors announced, each with the user message it is before.
fn cursors(updates: &[v2::SessionUpdate]) -> Vec<(String, String)> {
    updates
        .iter()
        .filter_map(|update| match update {
            v2::SessionUpdate::CursorUpdate(cursor) => match &cursor.anchor {
                Some(v2::CursorAnchor::UserMessage(anchor)) => {
                    assert_eq!(anchor.position, v2::CursorAnchorPosition::Before);
                    Some((
                        cursor.cursor_id.0.to_string(),
                        anchor.message_id.0.to_string(),
                    ))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn a_cursor_comes_before_each_user_message_that_follows_a_settled_position() {
    let events: Vec<_> = [turn("a"), turn("b")].concat();
    let updates = updates(&mut mapper(), &events);
    assert_eq!(
        cursors(&updates),
        [
            ("before-msg-0".to_string(), "msg-0".to_string()),
            ("before-msg-2".to_string(), "msg-2".to_string()),
        ]
    );
    let first_cursor = updates
        .iter()
        .position(|update| matches!(update, v2::SessionUpdate::CursorUpdate(_)))
        .unwrap();
    std::assert_matches!(
        &updates[first_cursor + 1],
        v2::SessionUpdate::UserMessage(message) if message.message_id.0.as_ref() == "msg-0"
    );
}

#[test]
fn a_message_queued_during_a_turn_gets_no_cursor() {
    let mut events = vec![
        input_accepted("a"),
        Event::InputStarted(vec!["a".into()]),
        committed(MessageRole::User, "a"),
        input_accepted("b"),
        Event::TurnEnded(TurnEndReason::Completed),
        input_finished("a"),
    ];
    events.extend(turn("b").into_iter().skip(1));
    assert_eq!(
        cursors(&updates(&mut mapper(), &events)),
        [("before-msg-0".to_string(), "msg-0".to_string())]
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
            Event::TurnEnded(TurnEndReason::HaltedByTokenBudget),
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
                v2::IdleStateUpdate::new().stop_reason(v2::StopReason::MaxTurnRequests)
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
    assert_eq!(pending[0].request.occurrence_id.0, "occ-2");

    let live = mapper.map(&AgentWireEvent::Event(approval("occ-3")));
    std::assert_matches!(&live[..], [Outgoing::AskPermission(ask)]
        if ask.request.occurrence_id.0 == "occ-3" && ask.task.is_none());
    let settled = mapper.map(&AgentWireEvent::Event(Event::ToolCallFinished(
        ToolCallResult::cancelled(identity("call-1", "occ-3")),
    )));
    assert!(settled.iter().any(|outgoing| matches!(outgoing,
        Outgoing::ApprovalSettled(occurrence) if occurrence == "occ-3")));
}

/// A `task` child's approval, shown through the requester
/// (`docs/agent-subagent-design.md` decision 4), is asked for with its
/// child named -- after the bootstrap when it arrived during replay -- and
/// withdrawn once settled.
#[test]
fn a_task_childs_approval_is_asked_naming_the_child_and_withdrawn_when_settled() {
    let child = SessionId::new();
    let Event::ApprovalRequested(request) = approval("occ-child") else {
        unreachable!()
    };
    let mut replaying = Mapper::new(SessionId::new(), facts());
    assert!(
        replaying
            .map(&AgentWireEvent::TaskApproval {
                task_session_id: child,
                request: request.clone(),
            })
            .is_empty(),
        "nothing is asked while replaying"
    );
    let pending = replaying.finish_replay();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].task, Some(child));

    let mut mapper = mapper();
    let asked = mapper.map(&AgentWireEvent::TaskApproval {
        task_session_id: child,
        request,
    });
    std::assert_matches!(&asked[..], [Outgoing::AskPermission(ask)]
        if ask.task == Some(child) && ask.request.occurrence_id.0 == "occ-child");
    let settled = mapper.map(&AgentWireEvent::TaskApprovalSettled {
        task_session_id: child,
        occurrence_id: OccurrenceId("occ-child".into()),
    });
    std::assert_matches!(&settled[..], [Outgoing::ApprovalSettled(occurrence)]
        if occurrence == "occ-child");
}

#[test]
fn model_announcements_become_one_select_option() {
    let mut mapper = mapper();
    assert!(
        mapper
            .map(&AgentWireEvent::SessionModel("gpt-4o".into()))
            .len()
            == 1
    );
    let outgoing = mapper.map(&AgentWireEvent::SessionSelection(ModelSelection {
        provider: "openai".into(),
        model: "gpt-4o".into(),
    }));
    let [Outgoing::Update(update)] = &outgoing[..] else {
        panic!("expected one update, got {outgoing:?}");
    };
    let v2::SessionUpdate::ConfigOptionUpdate(update) = &**update else {
        panic!("expected a config option update, got {outgoing:?}");
    };
    // The mode comes first: no role, the default.
    assert_eq!(
        update.config_options,
        vec![
            mode_option(&[], None),
            model_option(&[], "openai", "gpt-4o")
        ]
    );
    let v2::SessionConfigKind::Select(mode) = &update.config_options[0].kind else {
        panic!("expected a select option");
    };
    assert_eq!(&*mode.current_value.0, acp::DEFAULT_MODE);
    let option = &update.config_options[1];
    assert_eq!(&*option.config_id.0, acp::MODEL_CONFIG_ID);
    assert_eq!(
        option.category,
        Some(v2::SessionConfigOptionCategory::Model)
    );
    let v2::SessionConfigKind::Select(select) = &option.kind else {
        panic!("expected a select option");
    };
    assert_eq!(&*select.current_value.0, "openai/gpt-4o");
}

fn message_ids(updates: &[v2::SessionUpdate]) -> Vec<(String, String)> {
    updates
        .iter()
        .filter_map(|update| {
            let kind = match update {
                v2::SessionUpdate::UserMessage(_) => "user",
                v2::SessionUpdate::AgentMessage(_) => "message",
                v2::SessionUpdate::AgentMessageChunk(_) => "chunk",
                _ => return None,
            };
            Some((kind.to_string(), message_id(update)?))
        })
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(kind, id)| (kind.to_string(), id.to_string()))
        .collect()
}

#[test]
fn a_commit_after_a_tool_call_keeps_its_chunks_id() {
    let mut mapper = mapper();
    let updates = updates(
        &mut mapper,
        &[
            committed(MessageRole::User, "hello"),
            delta("Let me "),
            delta("look."),
            Event::ToolCallRequested(tool_request("call-1", "occ-1", "bash")),
            Event::ToolCallRequested(tool_request("call-2", "occ-2", "bash")),
            committed(MessageRole::Assistant, "Let me look."),
        ],
    );
    assert_eq!(
        message_ids(&updates),
        pairs(&[
            ("user", "msg-0"),
            ("chunk", "msg-1"),
            ("chunk", "msg-1"),
            ("message", "msg-1"),
        ])
    );
}

#[test]
fn a_commit_without_chunks_takes_a_fresh_id() {
    let mut mapper = mapper();
    let updates = updates(
        &mut mapper,
        &[
            committed(MessageRole::Assistant, "init"),
            committed(MessageRole::User, "hello"),
            Event::ToolCallRequested(tool_request("call-1", "occ-1", "bash")),
            committed(MessageRole::Assistant, "Mock response"),
        ],
    );
    assert_eq!(
        message_ids(&updates),
        pairs(&[
            ("message", "msg-0"),
            ("user", "msg-1"),
            ("message", "msg-3")
        ])
    );
}

#[test]
fn texts_separated_by_a_tool_call_match_their_own_chunks() {
    let mut mapper = mapper();
    let updates = updates(
        &mut mapper,
        &[
            committed(MessageRole::User, "hello"),
            delta("first"),
            Event::ToolCallRequested(tool_request("call-1", "occ-1", "bash")),
            committed(MessageRole::Assistant, "first"),
            Event::ToolCallStarted(identity("call-1", "occ-1")),
            delta("second"),
            Event::ToolCallRequested(tool_request("call-2", "occ-2", "bash")),
            committed(MessageRole::Assistant, "second"),
        ],
    );
    assert_eq!(
        message_ids(&updates),
        pairs(&[
            ("user", "msg-0"),
            ("chunk", "msg-1"),
            ("message", "msg-1"),
            ("chunk", "msg-3"),
            ("message", "msg-3"),
        ])
    );
}

#[test]
fn an_uncommitted_stream_closes_at_the_next_provider_request() {
    let mut mapper = mapper();
    let updates = updates(
        &mut mapper,
        &[
            committed(MessageRole::User, "hello"),
            delta("failed prefix"),
            Event::ProviderRequestSent(horizon_agent::contract::ProviderRequestSent {
                model: "model".into(),
            }),
            delta("retry"),
            committed(MessageRole::Assistant, "retry"),
            delta("orphan"),
            committed(MessageRole::User, "again"),
        ],
    );
    assert_eq!(
        message_ids(&updates),
        pairs(&[
            ("user", "msg-0"),
            ("chunk", "msg-1"),
            ("chunk", "msg-2"),
            ("message", "msg-2"),
            ("chunk", "msg-3"),
            ("user", "msg-4"),
        ])
    );
}

#[test]
fn a_task_childs_progress_is_its_subagent_state() {
    let mut mapper = mapper();
    let child = SessionId::new();
    let mut state_of = |state| {
        let outgoing = mapper.map(&AgentWireEvent::TaskProgress(
            horizon_agent::contract::TaskProgress {
                task_session_id: child,
                description: "map the module".into(),
                state,
            },
        ));
        let [Outgoing::Update(update)] = outgoing.as_slice() else {
            panic!("one update, got {outgoing:?}");
        };
        let v2::SessionUpdate::SubagentUpdate(subagent) = update.as_ref() else {
            panic!("a subagent update, got {update:?}");
        };
        assert_eq!(subagent.session_id, acp_session_id(child));
        assert_eq!(
            subagent.title,
            MaybeUndefined::Value("map the module".to_string())
        );
        subagent.state.clone()
    };
    use horizon_agent::contract::TaskProgressState;
    std::assert_matches!(
        state_of(TaskProgressState::Running),
        MaybeUndefined::Value(v2::StateUpdate::Running(_))
    );
    std::assert_matches!(
        state_of(TaskProgressState::WaitingForApproval),
        MaybeUndefined::Value(v2::StateUpdate::RequiresAction(_))
    );
    std::assert_matches!(
        state_of(TaskProgressState::Finished),
        MaybeUndefined::Value(v2::StateUpdate::Idle(_))
    );
}

fn failure(message: &str) -> Event {
    Event::Error(horizon_agent::contract::Error {
        message: message.into(),
    })
}

fn notice(update: &v2::SessionUpdate) -> Option<(v2::NoticeSeverity, String)> {
    match update {
        v2::SessionUpdate::Notice(notice) => Some((notice.severity.clone(), notice.title.clone())),
        _ => None,
    }
}

/// An error in a turn the turn's failure carries; one the turn survives,
/// or one outside a turn, is an error notice.
#[test]
fn errors_are_a_failed_turns_stop_reason_or_notices() {
    let mut mapper = mapper();
    let failed = updates(
        &mut mapper,
        &[
            Event::StateChanged(SessionState::Running),
            failure("provider unreachable"),
            Event::TurnEnded(TurnEndReason::Failed),
            Event::StateChanged(SessionState::WaitingForUser),
        ],
    );
    let stop = v2::ErrorStopReason::new().error(v2::Error::new(-32603, "provider unreachable"));
    assert_eq!(
        failed.last(),
        Some(&v2::SessionUpdate::StateUpdate(v2::StateUpdate::Idle(
            v2::IdleStateUpdate::new().stop_reason(v2::StopReason::Error(stop))
        )))
    );
    assert!(failed.iter().all(|update| notice(update).is_none()));

    let survived = updates(
        &mut mapper,
        &[
            Event::StateChanged(SessionState::Running),
            failure("a child failed"),
            Event::TurnEnded(TurnEndReason::Completed),
            Event::StateChanged(SessionState::WaitingForUser),
            failure("outside a turn"),
        ],
    );
    let notices: Vec<_> = survived.iter().filter_map(notice).collect();
    assert_eq!(
        notices,
        [
            (v2::NoticeSeverity::Error, "a child failed".to_string()),
            (v2::NoticeSeverity::Error, "outside a turn".to_string()),
        ]
    );
}

/// A rate limit is a notice for now, left out of a replay; history
/// clearing is a compaction, replayed where it happened.
#[test]
fn rate_limits_are_live_notices_and_clearing_is_a_compaction() {
    let events = [
        Event::ProviderRateLimited(horizon_agent::contract::ProviderRateLimited {
            status: Some(429),
            attempt: 2,
            backoff_ms: 1500,
        }),
        Event::HistoryCleared(horizon_agent::contract::HistoryCleared {
            cleared_occurrence_ids: vec![OccurrenceId("a".into()), OccurrenceId("b".into())],
            recovered_chars: 4096,
        }),
    ];
    let live = updates(&mut mapper(), &events);
    assert_eq!(
        notice(&live[0]),
        Some((
            v2::NoticeSeverity::Warning,
            "Provider rate limited (HTTP 429)".to_string()
        ))
    );
    let v2::SessionUpdate::CompactionUpdate(compaction) = &live[1] else {
        panic!("a compaction, got {:?}", live[1]);
    };
    assert_eq!(compaction.status, v2::CompactionStatus::Completed);
    assert_eq!(
        compaction.summary,
        MaybeUndefined::Value(vec!["2 tool outputs, 4096 characters reclaimed".into()])
    );

    let mut replaying = Mapper::new(SessionId::new(), facts());
    let replayed: Vec<_> = events
        .iter()
        .flat_map(|event| replaying.map(&AgentWireEvent::Event(event.clone())))
        .filter_map(|outgoing| match outgoing {
            Outgoing::Update(update) => Some(*update),
            _ => None,
        })
        .collect();
    assert_eq!(replayed.len(), 1);
    assert!(matches!(
        replayed[0],
        v2::SessionUpdate::CompactionUpdate(_)
    ));
}

/// The model option groups the catalog's models by provider, with the
/// current model among them even when its provider did not list it.
#[test]
fn the_model_option_groups_the_catalog_with_the_current_model() {
    let catalog = [
        ModelGroup {
            provider: "openai".into(),
            models: vec!["gpt-4o".into(), "o3".into()],
        },
        ModelGroup {
            provider: "local".into(),
            models: vec!["llama".into()],
        },
    ];
    let groups = |option: &v2::SessionConfigOption| -> Vec<(String, Vec<String>)> {
        let v2::SessionConfigKind::Select(select) = &option.kind else {
            panic!("a select");
        };
        let v2::SessionConfigSelectOptions::Grouped(groups) = &select.options else {
            panic!("grouped, got {:?}", select.options);
        };
        (groups.iter())
            .map(|group| {
                let values = (group.options.iter())
                    .map(|o| o.value.0.to_string())
                    .collect();
                (group.name.clone(), values)
            })
            .collect()
    };
    let id = acp::encode_model_option_id;
    assert_eq!(
        groups(&model_option(&catalog, "openai", "gpt-5")),
        [
            (
                "openai".to_string(),
                vec![
                    id("openai", "gpt-4o"),
                    id("openai", "o3"),
                    id("openai", "gpt-5")
                ]
            ),
            ("local".to_string(), vec![id("local", "llama")]),
        ]
    );
    assert_eq!(
        groups(&model_option(&catalog, "other", "m")).last(),
        Some(&("other".to_string(), vec![id("other", "m")]))
    );
}
