use super::*;
use crate::contract::{ConversationInputKind, MessageRole, ToolCallRequest, TurnEndReason};
use rig_core::completion::message::{Reasoning, ReasoningContent, ToolFunction};
use serde_json::json;

fn request(index: usize) -> ToolCallRequest {
    ToolCallRequest {
        call_id: ToolCallId("reused".into()),
        occurrence_id: OccurrenceId(format!("attempt-{index}")),
        tool_id: "fs.read".into(),
        input: json!({"path":format!("file-{index}")}).into(),
    }
}
fn capture(rx: &crossbeam_channel::Receiver<ProviderEvent>, events: &mut Vec<Event>) {
    events.extend(rx.try_iter().filter_map(ProviderEvent::into_event));
}

#[test]
fn long_conversation_replays_two_clearing_passes_and_explicit_input_boundaries() {
    use crate::providers::rig::clearing::{
        cleared_occurrence_ids_from_events, history_for_provider_request, ClearingState,
    };
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut live = ConversationHistory::default();
    let mut events = Vec::new();
    let mut clearing = ClearingState::new(Some(1), 1);
    clearing.record_input_tokens(1);
    for pass in 0..2 {
        live.open_turn(&tx);
        live.append_prompt(
            Prompt::input(ConversationInputKind::User, format!("question-{pass}")),
            &tx,
        )
        .unwrap();
        for index in pass * 40..(pass + 1) * 40 {
            let request = request(index);
            let event = announcement(&request, &format!("response-{index}"));
            live.apply_event(&event).unwrap();
            capture(&rx, &mut events);
            events.push(event);
            events.push(Event::ToolCallRequested(request.clone()));
            let result = request
                .identity()
                .result(json!({"content":"x".repeat(16_384)}));
            live.append_result(&result, "fs.read").unwrap();
            events.push(Event::ToolCallFinished(result));
        }
        let cleared = clearing.run_pass(&live).unwrap();
        assert!(!cleared.cleared_occurrence_ids.is_empty());
        events.push(Event::HistoryCleared(cleared));
    }
    for (kind, text) in [
        (ConversationInputKind::Notification, "task finished"),
        (ConversationInputKind::Continuation, "continue the work"),
    ] {
        live.append_prompt(Prompt::input(kind, text), &tx).unwrap();
    }
    capture(&rx, &mut events);
    let restored = ConversationHistory::from_events(&events).unwrap();
    let mut restored_clearing = ClearingState::disabled();
    restored_clearing.seed_cleared(cleared_occurrence_ids_from_events(&events));
    assert_eq!(live.messages(), restored.messages());
    assert_eq!(live.turn_start(), restored.turn_start());
    for memory in [
        None,
        Some(crate::tools::MemoryDocument {
            goal: "remember earlier work".into(),
            ..Default::default()
        }),
    ] {
        assert_eq!(
            history_for_provider_request(&live, clearing.cleared(), memory.as_ref(), None),
            history_for_provider_request(
                &restored,
                restored_clearing.cleared(),
                memory.as_ref(),
                None
            )
        );
    }
    assert_eq!(
        restored.owner_conversation(true),
        [(true, "question-0".into()), (true, "question-1".into())]
    );
}

#[test]
fn exact_provider_metadata_and_parallel_result_order_survive_early_results() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut history = ConversationHistory::default();
    history.open_turn(&tx);
    history
        .append_prompt(Prompt::input(ConversationInputKind::User, "read"), &tx)
        .unwrap();
    let mut calls = Vec::new();
    let mut identities = Vec::new();
    for index in 0..2 {
        let tool = ToolCall {
            id: rig_core::message::ToolCallId::new_or_mint(format!("local-{index}")),
            provider: rig_core::completion::message::ProviderCallId::new(format!("wire-{index}")),
            function: ToolFunction::new("fs.read".into(), json!({"path":index.to_string()})),
            signature: Some("signed-tool".into()),
            additional_params: Some(json!({"provider_specific":{"value":42}})),
        };
        identities.push(ToolCallIdentity {
            call_id: ToolCallId(format!("wire-{index}")),
            occurrence_id: OccurrenceId(format!("occ-{index}")),
        });
        calls.push(tool);
    }
    let reasoning = Reasoning {
        id: Some("thought".into()),
        content: vec![
            ReasoningContent::Text {
                text: "planning".into(),
                signature: Some("signed-reasoning".into()),
            },
            ReasoningContent::Encrypted("encrypted-content".into()),
        ],
    };
    for (tool, identity) in calls.iter().zip(&identities) {
        tx.send(
            Event::ConversationRecorded(ConversationRecord::ToolAnnounced {
                response_id: "response".into(),
                identity: identity.clone(),
                codec: MESSAGE_CODEC,
                tool_call: serde_json::to_value(tool).unwrap().into(),
                reasoning: serde_json::to_value(vec![reasoning.clone()])
                    .unwrap()
                    .into(),
            })
            .into(),
        )
        .unwrap();
    }
    let message = Message::Assistant {
        id: Some("upstream-message".into()),
        content: std::iter::once(AssistantContent::Reasoning(reasoning))
            .chain(calls.into_iter().map(AssistantContent::ToolCall))
            .collect(),
    };
    let results: Vec<_> = identities
        .iter()
        .map(|identity| identity.result(json!({"content":identity.occurrence_id.0})))
        .collect();
    // Results can be durable while the response stream is still open.
    for result in results.iter().rev() {
        tx.send(Event::ToolCallFinished(result.clone()).into())
            .unwrap();
    }
    history
        .record_response("response".into(), &message, identities, &tx)
        .unwrap();
    for result in &results {
        history.append_result(result, "fs.read").unwrap();
    }
    let events: Vec<_> = rx
        .try_iter()
        .filter_map(ProviderEvent::into_event)
        .collect();
    let encoded = serde_json::to_vec(&events).unwrap();
    let replay =
        ConversationHistory::from_events(&serde_json::from_slice::<Vec<Event>>(&encoded).unwrap())
            .unwrap();
    assert_eq!(replay.messages(), history.messages());
    assert_eq!(replay.messages()[1], message);
    let Message::User { content } = &replay.messages()[2] else {
        panic!("result")
    };
    let rig_core::completion::message::UserContent::ToolResult(result) = &content[0] else {
        panic!("result")
    };
    assert_eq!(result.call.as_str(), "local-0");
    assert_eq!(result.wire_call_id(), "wire-0");
}

#[test]
fn interrupted_announcements_are_settled_and_failed_answers_do_not_reach_proposers() {
    let request = request(1);
    let events = vec![
        Event::ConversationRecorded(ConversationRecord::TurnOpened),
        Event::ConversationRecorded(ConversationRecord::Input {
            kind: ConversationInputKind::User,
            text: "question".into(),
        }),
        announcement(&request, "interrupted"),
    ];
    let mut history = ConversationHistory::from_events(&events).unwrap();
    assert!(history.validate_ready().is_err());
    assert_eq!(
        interrupted_conversation_calls(&events).unwrap(),
        vec![request.identity()]
    );
    history
        .apply_event(&Event::ToolCallFinished(ToolCallResult::cancelled(
            request.identity(),
        )))
        .unwrap();
    history.validate_ready().unwrap();
    let (tx, _) = crossbeam_channel::unbounded();
    history
        .record_response(
            "failed-answer".into(),
            &Message::assistant("partial answer"),
            vec![],
            &tx,
        )
        .unwrap();
    history
        .apply_event(&Event::TurnEnded(TurnEndReason::Failed))
        .unwrap();
    assert_eq!(
        history.owner_conversation(true),
        [(true, "question".into())]
    );
    history
        .record_response(
            "answer".into(),
            &Message::assistant("final answer"),
            vec![],
            &tx,
        )
        .unwrap();
    history
        .apply_event(&Event::TurnEnded(TurnEndReason::Completed))
        .unwrap();
    assert_eq!(
        history.owner_conversation(true),
        [(true, "question".into()), (false, "final answer".into())]
    );
}

#[test]
fn unknown_codec_or_identity_conflicts_fail_without_changing_valid_history() {
    let mut history = ConversationHistory::default();
    let request = request(1);
    let event = announcement(&request, "one");
    history.apply_event(&event).unwrap();
    let original = history.messages();
    let Event::ConversationRecorded(mut invalid) = announcement(&request, "two") else {
        unreachable!()
    };
    if let ConversationRecord::ToolAnnounced { codec, .. } = &mut invalid {
        *codec = 999;
    }
    assert!(history.apply(&invalid).is_err());
    assert_eq!(history.messages(), original);
    assert!(ConversationHistory::from_events(&[Event::MessageCommitted(
        crate::contract::Message {
            role: MessageRole::User,
            text: "missing journal".into()
        }
    )])
    .is_err());
}

#[test]
fn legacy_request_boundaries_keep_early_results_in_one_parallel_response() {
    let first = request(1);
    let mut second = request(2);
    second.call_id = ToolCallId("second".into());
    let events = upgrade::fixture(vec![
        Event::MessageCommitted(crate::contract::Message {
            role: MessageRole::User,
            text: "read both".into(),
        }),
        Event::ProviderRequestSent(crate::contract::ProviderRequestSent {
            model: "legacy".into(),
        }),
        Event::ToolCallRequested(first.clone()),
        Event::ToolCallFinished(first.identity().result(json!({"content":"first"}))),
        Event::ToolCallRequested(second.clone()),
        Event::ToolCallFinished(second.identity().result(json!({"content":"second"}))),
        Event::MessageCommitted(crate::contract::Message {
            role: MessageRole::Assistant,
            text: "reading".into(),
        }),
    ]);
    let history = ConversationHistory::from_events(&events).unwrap();
    assert_eq!(history.messages().len(), 4);
    let Message::Assistant { content, .. } = &history.messages()[1] else {
        panic!("batch")
    };
    assert_eq!(
        content
            .iter()
            .filter(|item| matches!(item, AssistantContent::ToolCall(_)))
            .count(),
        2
    );
    history.validate_ready().unwrap();
}
