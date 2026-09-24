use super::*;
use crate::tools::cancelled_tool_call_result;

fn request(tool: &str, occurrence: Option<&str>) -> ToolCallRequest {
    ToolCallRequest {
        call_id: ToolCallId("reused".into()),
        tool_id: tool.into(),
        input: serde_json::json!({}).into(),
        occurrence_id: occurrence.map(|id| OccurrenceId(id.into())),
    }
}

fn result(request: &ToolCallRequest, text: &str) -> ToolCallResult {
    ToolCallResult::new(
        request.call_id.clone(),
        request.occurrence_id.clone(),
        serde_json::json!({"text": text}),
    )
}

#[test]
fn reused_provider_ids_keep_each_executed_tool_name() {
    for tagged in [false, true] {
        let first = request("fs.read", tagged.then_some("first"));
        let second = request("bash", tagged.then_some("second"));
        let first_result = result(&first, "read");
        let second_result = result(&second, "ran");
        assert_eq!(
            rig_messages_from_horizon_events(&[
                Event::ToolCallRequested(first.clone()),
                Event::ToolCallFinished(first_result.clone()),
                Event::ToolCallRequested(second.clone()),
                Event::ToolCallFinished(second_result.clone()),
            ]),
            vec![
                Message::from(rig_tool_call_from_request(&first)),
                rig_tool_result_message(&first_result, "fs.read"),
                Message::from(rig_tool_call_from_request(&second)),
                rig_tool_result_message(&second_result, "bash"),
            ]
        );
    }
}

#[test]
fn approved_retries_replay_one_provider_call_and_its_final_answer() {
    let first = request("bash", Some("first"));
    let retry = request("bash", Some("retry"));
    let final_attempt = request("bash", Some("last"));
    let finished = result(&final_attempt, "done");
    let events = vec![
        Event::ToolCallRequested(first.clone()),
        Event::ToolCallRequested(retry.clone()),
        Event::ToolCallFinished(
            result(&first, "denied").superseded_by_retry(retry.occurrence_id.as_ref()),
        ),
        Event::ToolCallRequested(final_attempt.clone()),
        Event::ToolCallFinished(
            result(&retry, "denied again")
                .superseded_by_retry(final_attempt.occurrence_id.as_ref()),
        ),
        Event::ToolCallFinished(finished.clone()),
    ];
    let messages = rig_messages_from_horizon_events(&events);
    assert_eq!(
        messages,
        vec![
            Message::from(rig_tool_call_from_request(&first)),
            rig_tool_result_message(&finished, "bash"),
        ]
    );
    assert_eq!(repair_replayed_message_pairing(messages.clone()), messages);
    let session_id = crate::contract::SessionId::new();
    let store = crate::persistence::projection::duckdb::Store::open_in_memory().unwrap();
    store
        .append_events(session_id, None, events.clone())
        .unwrap();
    let store = crate::persistence::projection::duckdb::DuckdbStoreHandle::new(store);
    for history in [
        super::super::history::load_rig_session_history(Some(&store), session_id, &[]),
        super::super::history::load_rig_session_history(None, session_id, &events),
    ] {
        assert_eq!(
            history.messages, messages,
            "both resume sources preserve the provider view"
        );
    }
}

#[test]
fn refusing_a_retry_replays_the_original_attempts_answer_once() {
    let first = request("bash", Some("first"));
    let retry = request("bash", Some("retry"));
    let denied = result(&first, "network denied");
    assert_eq!(
        rig_messages_from_horizon_events(&[
            Event::ToolCallRequested(first.clone()),
            Event::ToolCallRequested(retry),
            Event::ToolCallFinished(denied.clone()),
            Event::ToolCallFinished(denied.clone()),
        ]),
        vec![
            Message::from(rig_tool_call_from_request(&first)),
            rig_tool_result_message(&denied, "bash"),
        ]
    );
}

#[test]
fn a_later_answer_does_not_hide_an_unanswered_earlier_use_of_the_id() {
    let first = request("bash", Some("first"));
    let second = request("bash", Some("second"));
    let answered = result(&second, "done");
    let messages = rig_messages_from_horizon_events(&[
        Event::ToolCallRequested(first.clone()),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "try again".into(),
        }),
        Event::ToolCallRequested(second.clone()),
        Event::ToolCallFinished(answered.clone()),
    ]);
    assert_eq!(
        messages,
        vec![
            Message::from(rig_tool_call_from_request(&first)),
            rig_tool_result_message(&cancelled_tool_call_result(first.call_id), "bash"),
            Message::user("try again"),
            Message::from(rig_tool_call_from_request(&second)),
            rig_tool_result_message(&answered, "bash"),
        ]
    );
    assert_eq!(repair_replayed_message_pairing(messages.clone()), messages);
}

#[test]
fn an_unknown_occurrence_cannot_answer_a_known_provider_id() {
    let announced = request("fs.read", Some("known"));
    let unknown = request("fs.read", Some("unknown"));
    assert_eq!(
        rig_messages_from_horizon_events(&[
            Event::ToolCallRequested(announced.clone()),
            Event::ToolCallFinished(result(&unknown, "unrelated")),
        ]),
        vec![
            Message::from(rig_tool_call_from_request(&announced)),
            rig_tool_result_message(&cancelled_tool_call_result(announced.call_id), "fs.read"),
        ]
    );
}
