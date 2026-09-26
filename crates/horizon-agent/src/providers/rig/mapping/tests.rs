use super::*;
use crate::tools::cancelled_tool_call_result;

fn request(tool: &str, occurrence: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: ToolCallId("reused".into()),
        tool_id: tool.into(),
        input: serde_json::json!({}).into(),
        occurrence_id: OccurrenceId(occurrence.into()),
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
    {
        let first = request("fs.read", "first");
        let second = request("bash", "second");
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
    let first = request("bash", "first");
    let retry = request("bash", "retry");
    let final_attempt = request("bash", "last");
    let finished = result(&final_attempt, "done");
    let events = vec![
        Event::ToolCallRequested(first.clone()),
        Event::ToolCallRequested(retry.clone()),
        Event::ToolCallFinished(result(&first, "denied").superseded_by_retry(&retry.occurrence_id)),
        // A physical completion can arrive after the retired attempt closed.
        Event::ToolCallFinished(result(&first, "stale physical completion")),
        Event::ToolCallRequested(final_attempt.clone()),
        Event::ToolCallFinished(
            result(&retry, "denied again").superseded_by_retry(&final_attempt.occurrence_id),
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
    let first = request("bash", "first");
    let retry = request("bash", "retry");
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
    let first = request("bash", "first");
    let second = request("bash", "second");
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
            rig_tool_result_message(&cancelled_tool_call_result(first.identity()), "bash"),
            Message::user("try again"),
            Message::from(rig_tool_call_from_request(&second)),
            rig_tool_result_message(&answered, "bash"),
        ]
    );
    assert_eq!(repair_replayed_message_pairing(messages.clone()), messages);
}

#[test]
fn an_unknown_occurrence_cannot_answer_a_known_provider_id() {
    let announced = request("fs.read", "known");
    let unknown = request("fs.read", "unknown");
    assert_eq!(
        rig_messages_from_horizon_events(&[
            Event::ToolCallRequested(announced.clone()),
            Event::ToolCallFinished(result(&unknown, "unrelated")),
        ]),
        vec![
            Message::from(rig_tool_call_from_request(&announced)),
            rig_tool_result_message(&cancelled_tool_call_result(announced.identity()), "fs.read"),
        ]
    );
}

#[test]
fn provider_results_preserve_outcome_separately_from_arbitrary_tool_data() {
    use crate::contract::ToolOutcome;
    let request = request("bash", "attempt");
    for outcome in [
        ToolOutcome::Succeeded,
        ToolOutcome::Failed,
        ToolOutcome::Denied,
        ToolOutcome::Cancelled,
    ] {
        let mut result = result(&request, "denied by user");
        result.outcome = outcome.clone();
        let message = rig_tool_result_message(&result, "bash");
        assert_eq!(
            message,
            Message::tool_result(
                "reused",
                "bash",
                serde_json::json!({
                    "outcome": outcome, "output": {"text": "denied by user"}
                })
                .to_string()
            )
        );
        let replay = rig_messages_from_horizon_events(&[
            Event::ToolCallRequested(request.clone()),
            Event::ToolCallFinished(result),
        ]);
        assert_eq!(replay.last(), Some(&message));
    }
}

#[test]
fn actual_partial_edit_survives_log_database_provider_and_change_projection() {
    use crate::contract::tool_output::{decode, EditOutcome, FileEdits};
    use crate::contract::{SessionId, ToolOutcome};
    use crate::live::LiveState;
    use crate::persistence::{event_log, projection::duckdb::Store};
    use crate::tools::{execute_agent_tool, Execution, HostTools, ToolSessionBuilder, ToolUpdate};
    use crate::transcript::{aggregate_changes, build_tool_call_views};
    use serde_json::json;

    struct NoHost;
    impl HostTools for NoHost {
        fn execute_auto(&self, _: &str, _: &serde_json::Value) -> Option<serde_json::Value> {
            None
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let path = root.join("changed.txt");
    let untouched = root.join("untouched.txt");
    std::fs::write(&path, "old\nold\n").unwrap();
    std::fs::write(&untouched, "keep\n").unwrap();
    let state = ToolSessionBuilder::new(root.clone())
        .with_isolated_worktree(true)
        .build();
    let session = SessionId::new();
    let log = root.join("events.jsonl");
    let (writer, ready) = event_log::WriterHandle::open(&log);
    assert!(matches!(
        ready.recv().unwrap(),
        event_log::WriterInit::Ready(_)
    ));
    let live =
        LiveState::with_event_log_and_history(session, None, None, writer.clone(), Vec::new());
    let execute = |tool: &str, input: serde_json::Value| {
        let request = ToolCallRequest {
            call_id: ToolCallId(tool.into()),
            occurrence_id: OccurrenceId::new(),
            tool_id: tool.into(),
            input: input.into(),
        };
        live.extend_events([Event::ToolCallRequested(request.clone())]);
        let Execution::Applied(ToolUpdate::Finished { result, .. }) =
            execute_agent_tool(&NoHost, &state, session, &live, &request).unwrap()
        else {
            panic!("expected a synchronous result")
        };
        result
    };
    execute("fs.read", json!({"path": path}));
    let result = execute(
        "fs.edit",
        json!({"edits": [
            {"path": path, "old_string": "old", "new_string": "new", "replace_all": true},
            {"path": path, "old_string": "absent", "new_string": "never"},
            {"path": untouched, "old_string": "keep", "new_string": "never"}
        ]}),
    );
    assert_eq!(result.outcome, ToolOutcome::Failed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\nnew\n");
    assert_eq!(std::fs::read_to_string(&untouched).unwrap(), "keep\n");
    let facts = decode::<FileEdits>(&result.output).unwrap();
    assert_eq!(facts.applied_count, 1);
    assert_eq!(facts.failed_index, Some(1));
    assert!(matches!(
        facts.edits[0].outcome,
        EditOutcome::Applied { occurrences: 2 }
    ));
    assert!(matches!(facts.edits[1].outcome, EditOutcome::Failed { .. }));
    assert_eq!(facts.edits[2].outcome, EditOutcome::NotAttempted);

    writer.flush().unwrap();
    let records = event_log::read(&log).unwrap().records;
    let replay: Vec<_> = records.iter().map(|record| record.event.clone()).collect();
    assert_eq!(replay, live.events());
    let store = Store::open_in_memory().unwrap();
    let imported = store.replace_from_event_log_records(records).unwrap();
    assert_eq!(imported.skipped, 0);
    let saved_frame = store.frame_for_session(session).unwrap();
    assert_eq!(saved_frame, live.frame());
    for frame in [&saved_frame, &live.frame()] {
        let views = build_tool_call_views(&frame.items);
        let changes = aggregate_changes(&views);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, path.to_str().unwrap());
        assert_eq!((changes[0].added, changes[0].removed), (2, 2));
        assert!(views.last().unwrap().is_error());
        assert!(views
            .last()
            .unwrap()
            .result_summary
            .as_ref()
            .unwrap()
            .contains("1 applied"));
    }
    let messages = rig_messages_from_horizon_events(&replay);
    assert_eq!(
        messages.last(),
        Some(&rig_tool_result_message(&result, "fs.edit"))
    );
    let stored = crate::persistence::projection::duckdb::DuckdbStoreHandle::new(store);
    let restored = super::super::history::load_rig_session_history(Some(&stored), session, &[]);
    assert_eq!(restored.messages, messages);
}
