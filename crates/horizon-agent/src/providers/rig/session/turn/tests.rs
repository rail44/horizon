use super::*;
use crate::config::RigAgentConfig;
use crate::contract::ProviderEvent;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve_summary_request(listener: TcpListener) -> Value {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut request = Vec::new();
    let (body_start, body_len) = loop {
        let mut chunk = [0; 4096];
        let read = socket.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request ended before its headers");
        request.extend_from_slice(&chunk[..read]);
        assert!(request.len() < 1024 * 1024, "unexpectedly large request");
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]);
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .expect("JSON request content length");
            assert!(length < 1024 * 1024);
            break (end + 4, length);
        }
    };
    let received = request.len();
    request.resize(body_start + body_len, 0);
    // The first read may already contain part or all of the body.
    // Re-read only the bytes not received with the headers.
    if received < request.len() {
        socket.read_exact(&mut request[received..]).await.unwrap();
    }
    let body: Value = serde_json::from_slice(&request[body_start..]).unwrap();
    let chunk = json!({
        "id": "summary-response", "object": "chat.completion.chunk", "created": 0,
        "model": "summary-model",
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Partial summary."},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
    });
    let response = format!("data: {chunk}\n\ndata: [DONE]\n\n");
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
    );
    socket.write_all(headers.as_bytes()).await.unwrap();
    socket.write_all(response.as_bytes()).await.unwrap();
    body
}

#[tokio::test]
async fn cap_summary_request_disables_tools_without_changing_session_config() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let request = tokio::spawn(serve_summary_request(listener));
    let key_var = "HORIZON_TEST_CAP_SUMMARY_KEY";
    std::env::set_var(key_var, "local-test-key");
    let (events_tx, events) = crossbeam_channel::unbounded();
    let mut state = SessionLoopState {
        config: RigAgentConfig {
            api_key_present: true,
            api_key_env: key_var.into(),
            base_url: Some(base_url),
            model: "summary-model".into(),
            allowed_tool_ids: Some(vec!["fs.read".into()]),
            ..Default::default()
        },
        events_tx,
        ..Default::default()
    };
    let original_config = state.config.clone();
    let result = ToolCallResult::new(
        ToolCallId("last-call".into()),
        crate::contract::OccurrenceId("last-call".into()),
        json!({"content": "read"}),
    );
    let summarized = tokio::time::timeout(
        Duration::from_secs(5),
        state.run_cap_summary_turn(&result, "fs.read"),
    )
    .await
    .expect("summary request timed out");
    assert!(summarized);
    let request = request.await.unwrap();
    assert!(
        request
            .get("tools")
            .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)),
        "the final summary must not advertise tools: {request}"
    );
    assert_eq!(state.config, original_config);
    assert!(events.try_iter().any(|event| matches!(
        event.clone().into_event().expect("conversation event"),
        Event::MessageCommitted(AgentMessage { role: MessageRole::Assistant, text })
            if text == "Partial summary."
    )));
}

/// A completing standing-role interaction gets one reminder, even if the
/// fallback responder never calls tools. The next interaction gets its own.
#[tokio::test]
async fn memory_reminder_is_bounded_and_resets_for_the_next_interaction() {
    let (events_tx, events) = crossbeam_channel::unbounded();
    let mut state = SessionLoopState {
        memory: Some(Default::default()),
        config: RigAgentConfig {
            api_key_present: false,
            ..Default::default()
        },
        events_tx,
        ..Default::default()
    };
    for text in ["first interaction", "next interaction"] {
        state.handle_user_message(text.into()).await;
        let emitted: Vec<_> = events
            .try_iter()
            .filter_map(ProviderEvent::into_event)
            .collect();
        assert_eq!(
            emitted
                .iter()
                .filter(|event| matches!(event,
                    Event::MessageCommitted(AgentMessage { role: MessageRole::AutoContinue, text })
                        if text == MEMORY_CHECKPOINT_REMINDER
                ))
                .count(),
            1
        );
        assert_eq!(
            emitted
                .iter()
                .filter(|event| matches!(event, Event::MemoryCheckpointMissed))
                .count(),
            1
        );
        assert!(emitted
            .iter()
            .any(|event| matches!(event, Event::TurnEnded(TurnEndReason::Completed))));
    }
    state.memory = None;
    state.handle_user_message("ordinary role".into()).await;
    assert!(!events.try_iter().any(|event| matches!(
        event.clone().into_event().expect("conversation event"),
        Event::MemoryCheckpointMissed
            | Event::MessageCommitted(AgentMessage {
                role: MessageRole::AutoContinue,
                ..
            })
    )));
}

#[tokio::test]
async fn accepted_memory_updates_and_no_update_declarations_satisfy_the_checkpoint() {
    for args in [
        json!({"goal": {"op": "set", "content": "ship"}}),
        json!({"no_update": {"reason": "nothing changed"}}),
    ] {
        let call_id = ToolCallId("memory".into());
        let sibling = ToolCallId("pending-sibling".into());
        let descriptor = ToolCallDescriptor {
            identity: crate::test_support::tool_identity(&call_id),
            tool_id: crate::tools::MEMORY_UPDATE_TOOL_ID.into(),
            args,
        };
        let result = ToolCallResult::new(
            call_id.clone(),
            descriptor.identity.occurrence_id.clone(),
            json!({"ok": true}),
        );
        let (events_tx, events) = crossbeam_channel::unbounded();
        let mut state = SessionLoopState {
            memory: Some(super::super::memory::StandingMemory {
                checkpoint: MemoryCheckpoint::Reminded,
                ..Default::default()
            }),
            pending_tool_calls: HashMap::from([
                (call_id, descriptor),
                (
                    sibling.clone(),
                    ToolCallDescriptor {
                        identity: crate::test_support::tool_identity(&sibling),
                        tool_id: "fs.read".into(),
                        args: json!({"path": "/pending"}),
                    },
                ),
            ]),
            guard: super::super::TurnLoopGuard::new(20, 10),
            events_tx,
            ..Default::default()
        };
        state.handle_tool_result(result).await;
        assert_eq!(
            state.memory.as_ref().unwrap().checkpoint,
            MemoryCheckpoint::Satisfied
        );
        assert!(events.try_iter().any(|event| matches!(
            event.clone().into_event().expect("conversation event"),
            Event::MemoryDigest(_)
        )));
        assert!(state
            .handle_memory_checkpoint(TurnCompletion::default())
            .await
            .unwrap()
            .is_completing());
        assert!(
            events.try_recv().is_err(),
            "a satisfied checkpoint must not remind or report a miss"
        );
    }
}
