use super::*;
use rig_core::{
    completion::{message::ToolFunction, Usage},
    streaming::StreamFinal,
};

fn config() -> RigAgentConfig {
    RigAgentConfig {
        stream_flush_interval_ms: u64::MAX,
        stream_flush_chars: usize::MAX,
        max_output_tokens: 20,
        ..RigAgentConfig::default()
    }
}

fn call() -> ToolCall {
    ToolCall::new(
        rig_core::message::ToolCallId::new_or_mint("call-1"),
        ToolFunction::new(
            "fs.read".to_string(),
            serde_json::json!("{\"path\":\"README.md\"}"),
        ),
    )
}

#[test]
fn tool_call_flushes_deltas_and_preserves_raw_payload_before_text_commit() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut durable = false;
    let mut response = ResponseCollector::new(&config(), tx, &mut durable);
    response.push(StreamedAssistantContent::ReasoningDelta {
        id: "reasoning-1".into(),
        provider_id: None,
        reasoning: "thinking".into(),
    });
    response.push(StreamedAssistantContent::text("reading"));
    let raw = call();
    let payload = rig_tool_call_provider_payload(&raw);
    response.push(StreamedAssistantContent::ToolCall {
        tool_call: raw.clone(),
        internal_call_id: "internal-1".into(),
    });
    let before_finish: Vec<_> = rx.try_iter().collect();
    assert_eq!(before_finish.len(), 4);
    assert!(matches!(
        before_finish[0].event,
        Event::ProviderRequestFirstToken
    ));
    assert!(matches!(before_finish[1].event, Event::ReasoningDelta(_)));
    assert!(matches!(
        before_finish[2].event,
        Event::AssistantTextDelta(_)
    ));
    let Event::ToolCallRequested(request) = &before_finish[3].event else {
        panic!("tool request")
    };
    assert_eq!(request.input.0, serde_json::json!({"path":"README.md"}));
    assert_eq!(before_finish[3].provider_payload, Some(payload));

    let (message, outcome) = response.finish(
        false,
        Some("message-1".into()),
        vec![AssistantContent::ToolCall(raw)],
    );
    assert!(durable);
    assert_eq!(outcome.final_text.as_deref(), Some("reading"));
    assert_eq!(
        outcome.requested_tool_call_ids,
        vec![ToolCallId("call-1".into())]
    );
    let Message::Assistant { id, content } = message else {
        panic!("assistant history")
    };
    assert_eq!(id.as_deref(), Some("message-1"));
    let AssistantContent::ToolCall(replayed) = &content[0] else {
        panic!("replayed call")
    };
    assert_eq!(replayed.function.arguments, request.input.0);
    let after_finish: Vec<_> = rx.try_iter().collect();
    assert_eq!(after_finish.len(), 1);
    assert!(
        matches!(&after_finish[0].event, Event::MessageCommitted(message) if message.text == "reading")
    );
}

#[test]
fn failed_response_does_not_commit_text_but_keeps_tool_call_durability() {
    for with_call in [false, true] {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut durable = false;
        let mut response = ResponseCollector::new(&config(), tx, &mut durable);
        response.push(StreamedAssistantContent::text("partial"));
        if with_call {
            response.push(StreamedAssistantContent::ToolCall {
                tool_call: call(),
                internal_call_id: "internal-1".into(),
            });
        }
        // A stream error drops the collector without calling finish.
        drop(response);
        assert_eq!(durable, with_call);
        let events: Vec<_> = rx.try_iter().map(|event| event.event).collect();
        assert!(!events
            .iter()
            .any(|event| matches!(event, Event::MessageCommitted(_))));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::AssistantTextDelta(_)))
                .count(),
            usize::from(with_call)
        );
    }
}

#[test]
fn cancelled_response_retains_observed_history_and_suppresses_truncation() {
    let (tx, _) = crossbeam_channel::unbounded();
    let mut durable = false;
    let mut response = ResponseCollector::new(&config(), tx, &mut durable);
    response.push(StreamedAssistantContent::text("partial"));
    response.push(StreamedAssistantContent::ToolCall {
        tool_call: call(),
        internal_call_id: "finished".into(),
    });
    response.push(StreamedAssistantContent::ToolCallDelta {
        internal_call_id: "unfinished".into(),
        content: ToolCallDeltaContent::Delta("{".into()),
    });
    response.push(StreamedAssistantContent::Final(StreamFinal::new(
        "test",
        Usage {
            input_tokens: 10,
            total_tokens: 30,
            ..Usage::new()
        },
    )));
    let (message, outcome) = response.finish(true, Some("partial-id".into()), Vec::new());
    assert!(outcome.cancelled);
    assert!(!outcome.truncated);
    assert!(!outcome.cap_truncated);
    assert!(outcome.final_text.is_none());
    let Message::Assistant { id, content } = message else {
        panic!("partial history")
    };
    assert_eq!(id.as_deref(), Some("partial-id"));
    assert!(content
        .iter()
        .any(|item| matches!(item, AssistantContent::Text(text) if text.text == "partial")));
    assert!(content.iter().any(|item| matches!(item, AssistantContent::ToolCall(call) if call.function.arguments == serde_json::json!({"path":"README.md"}))));
}

#[test]
fn unfinished_call_and_usage_cap_withhold_a_normal_final_answer() {
    let (tx, _) = crossbeam_channel::unbounded();
    let mut durable = false;
    let mut response = ResponseCollector::new(&config(), tx, &mut durable);
    response.push(StreamedAssistantContent::text("partial"));
    response.push(StreamedAssistantContent::ToolCallDelta {
        internal_call_id: "unfinished".into(),
        content: ToolCallDeltaContent::Delta("{".into()),
    });
    response.push(StreamedAssistantContent::Final(StreamFinal::new(
        "test",
        Usage {
            input_tokens: 10,
            total_tokens: 30,
            ..Usage::new()
        },
    )));
    let (_, outcome) = response.finish(false, None, Vec::new());
    assert!(outcome.truncated);
    assert_eq!(outcome.truncated_tool_call_count, 1);
    assert!(outcome.cap_truncated);
    assert_eq!(outcome.input_tokens, Some(10));
    assert_eq!(outcome.output_tokens, Some(20));
    assert!(outcome.final_text.is_none());
}
