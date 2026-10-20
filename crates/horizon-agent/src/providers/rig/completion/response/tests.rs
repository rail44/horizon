use super::*;

/// These fixtures drive events, not the folded response, so they pass no
/// terminal record; `finish` is called through this shim to keep the call
/// sites that 0.42's signature produced.
trait FinishShim {
    fn finish_test(
        self,
        end: ResponseEnd,
        message_id: Option<String>,
        content: Vec<AssistantContent>,
    ) -> (Message, TurnCompletion);
}

/// A response as rig 0.44 folds it: `CompletionResponse` has private fields and
/// no constructor, so a fixture decodes the shape rig serializes.
fn completion_response(
    reason: Option<serde_json::Value>,
    usage_tokens: Option<(u64, u64)>,
    choice: Vec<AssistantContent>,
) -> rig_core::completion::CompletionResponse {
    serde_json::from_value(serde_json::json!({
        "choice": serde_json::to_value(choice).expect("choice json"),
        "usage": match usage_tokens {
            Some((input, output)) => {
                serde_json::json!({ "input_tokens": input, "output_tokens": output })
            }
            None => serde_json::json!({}),
        },
        "origin": { "api": "openai", "provider": "openai", "model": "test" },
        "finish_reason": reason,
        "raw": { "choices": [ { "finish_reason": reason } ] },
    }))
    .expect("fixture completion response")
}

/// The raw provider spelling of a normalized finish reason.
fn reason_text(reason: Option<&rig_core::completion::FinishReason>) -> Option<serde_json::Value> {
    match reason {
        Some(rig_core::completion::FinishReason::Stop) => Some(serde_json::json!("stop")),
        Some(rig_core::completion::FinishReason::Length) => Some(serde_json::json!("length")),
        Some(rig_core::completion::FinishReason::ContentFilter) => {
            Some(serde_json::json!("content_filter"))
        }
        Some(rig_core::completion::FinishReason::ToolCalls) => {
            Some(serde_json::json!("tool_calls"))
        }
        Some(rig_core::completion::FinishReason::Other(reason)) => {
            Some(serde_json::json!({ "other": reason }))
        }
        // `FinishReason` is non-exhaustive.
        Some(_) => None,
        None => None,
    }
}

impl FinishShim for ResponseCollector {
    fn finish_test(
        self,
        end: ResponseEnd,
        message_id: Option<String>,
        content: Vec<AssistantContent>,
    ) -> (Message, TurnCompletion) {
        self.finish(
            end,
            message_id,
            Some(completion_response(
                Some(serde_json::json!("stop")),
                None,
                content,
            )),
        )
    }
}
use rig_core::completion::message::ToolFunction;
use rig_core::streaming::{Item, StreamEvent};

/// rig 0.44 builds a `Part` only inside the crate, so a fixture event is
/// written as the JSON rig itself serializes and then decoded.
fn stream_item(value: serde_json::Value) -> Item<StreamEvent> {
    serde_json::from_value(value).expect("fixture stream item")
}

fn text_item(text: &str) -> Item<StreamEvent> {
    stream_item(serde_json::json!({
        "item": "event",
        "value": { "event": "text", "part": 0, "text": text }
    }))
}

fn reasoning_item(text: &str) -> Item<StreamEvent> {
    stream_item(serde_json::json!({
        "item": "event",
        "value": { "event": "reasoning", "part": 0, "text": text }
    }))
}

fn tool_start_item(name: &str) -> Item<StreamEvent> {
    stream_item(serde_json::json!({
        "item": "event",
        "value": { "event": "start", "part": 1, "kind": "tool_call", "name": name }
    }))
}

fn tool_arguments_item(fragment: &str) -> Item<StreamEvent> {
    stream_item(serde_json::json!({
        "item": "event",
        "value": { "event": "arguments", "part": 1, "json": fragment }
    }))
}

fn tool_end_item(call: ToolCall) -> Item<StreamEvent> {
    let content = serde_json::to_value(AssistantContent::ToolCall(call)).expect("tool call json");
    stream_item(serde_json::json!({
        "item": "event",
        "value": { "event": "end", "part": 1, "content": content }
    }))
}

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
        rig_core::message::CallId::from_wire("call-1"),
        ToolFunction::new(
            rig_core::message::ToolName::new("fs.read").expect("tool id"),
            serde_json::json!("{\"path\":\"README.md\"}"),
        ),
    )
}

#[test]
fn tool_call_flushes_deltas_and_preserves_raw_payload_before_text_commit() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut response = ResponseCollector::new(&config(), tx);
    response.push(reasoning_item("thinking"));
    response.push(text_item("reading"));
    let raw = call();
    let payload = rig_tool_call_provider_payload(&raw);
    response.push(tool_end_item(raw.clone()));
    let before_finish: Vec<_> = rx.try_iter().collect();
    assert_eq!(before_finish.len(), 4);
    assert!(matches!(
        before_finish[0]
            .clone()
            .into_event()
            .expect("conversation event"),
        Event::ProviderRequestFirstToken
    ));
    // Reasoning deltas are not shown (decided), so the first event after
    // first-token is the assistant text.
    assert!(matches!(
        before_finish[1]
            .clone()
            .into_event()
            .expect("conversation event"),
        Event::AssistantTextDelta(_)
    ));
    assert!(matches!(
        before_finish[1]
            .clone()
            .into_event()
            .expect("conversation event"),
        Event::AssistantTextDelta(_)
    ));
    let Event::ToolCallRequested(request) = &before_finish[3]
        .clone()
        .into_event()
        .expect("conversation event")
    else {
        panic!("tool request")
    };
    assert_eq!(request.input.0, serde_json::json!({"path":"README.md"}));
    assert!(
        matches!(&before_finish[3], ProviderEvent::Event { provider_payload: Some(value), .. } if value == &payload)
    );
    let _ = rx.try_iter().collect::<Vec<_>>();
    let (message, outcome) = response.finish_test(
        ResponseEnd::Finished,
        Some("message-1".into()),
        vec![AssistantContent::ToolCall(raw)],
    );

    assert_eq!(outcome.final_text(), Some("reading"));
    assert_eq!(
        outcome.requested_tool_call_ids,
        vec![ToolCallId("call-1".into())]
    );
    let Message::Assistant(rig_core::completion::message::AssistantMessage { content, .. }) =
        message
    else {
        panic!("assistant history")
    };
    let AssistantContent::ToolCall(replayed) = &content[0] else {
        panic!("replayed call")
    };
    assert_eq!(
        serde_json::Value::Object(replayed.function.arguments.clone()),
        request.input.0
    );
    let after_finish: Vec<_> = rx.try_iter().collect();
    assert!(
        after_finish.is_empty(),
        "commit is published only after retry selection"
    );
}

#[test]
fn failed_response_does_not_commit_text_but_keeps_tool_call_durability() {
    for with_call in [false, true] {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut response = ResponseCollector::new(&config(), tx);
        response.push(text_item("partial"));
        // An opened tool part that never ends is what marks a truncated batch.
        response.push(tool_start_item("workspace.snapshot"));
        if with_call {
            response.push(tool_end_item(call()));
        }
        // A stream error drops the collector without calling finish.
        drop(response);

        let events: Vec<_> = rx
            .try_iter()
            .filter_map(ProviderEvent::into_event)
            .collect();
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
    let mut response = ResponseCollector::new(&config(), tx);
    response.push(text_item("partial"));
    response.push(tool_end_item(call()));
    response.push(tool_arguments_item("{"));
    let (message, outcome) = response.finish(
        ResponseEnd::Cancelled,
        Some("partial-id".into()),
        Some(completion_response(
            Some(serde_json::json!("stop")),
            Some((10, 20)),
            Vec::new(),
        )),
    );
    assert_eq!(outcome.stop, CompletionStop::Cancelled);
    assert_eq!(outcome.requested_tool_call_ids.len(), 1);
    assert_eq!(outcome.requested_tool_calls.len(), 1);
    assert_eq!(outcome.input_tokens, Some(10));
    assert_eq!(outcome.output_tokens, Some(20));
    assert!(outcome.final_text().is_none());
    let Message::Assistant(rig_core::completion::message::AssistantMessage { content, .. }) =
        message
    else {
        panic!("partial history")
    };
    assert!(content
        .iter()
        .any(|item| matches!(item, AssistantContent::Text(text) if text.text == "partial")));
    assert!(content.iter().any(|item| matches!(item, AssistantContent::ToolCall(call) if serde_json::Value::Object(call.function.arguments.clone()) == serde_json::json!({"path":"README.md"}))));
}

#[test]
fn unfinished_call_and_usage_cap_withhold_a_normal_final_answer() {
    let (tx, _) = crossbeam_channel::unbounded();
    let mut response = ResponseCollector::new(&config(), tx);
    response.push(text_item("partial"));
    response.push(tool_start_item("workspace.snapshot"));
    response.push(tool_arguments_item("{"));
    let (_, outcome) = response.finish(
        ResponseEnd::Finished,
        None,
        Some(completion_response(
            Some(serde_json::json!("length")),
            Some((10, config().max_output_tokens)),
            Vec::new(),
        )),
    );
    assert_eq!(
        outcome.stop,
        CompletionStop::Truncated(super::super::Truncation::Tools {
            unfinished: std::num::NonZeroUsize::new(1).unwrap(),
            output_cap: true,
        })
    );
    assert_eq!(outcome.input_tokens, Some(10));
    assert_eq!(outcome.output_tokens, Some(20));
    assert!(outcome.final_text().is_none());
}

#[test]
fn explicit_finish_reasons_override_usage_and_missing_metadata_is_bounded() {
    use rig_core::completion::FinishReason;
    for (reason, tokens, expected) in [
        (
            Some(FinishReason::Stop),
            20,
            CompletionStop::Finished {
                text: "answer".into(),
            },
        ),
        (
            Some(FinishReason::Length),
            0,
            CompletionStop::Truncated(super::super::Truncation::OutputCap),
        ),
        (
            Some(FinishReason::ContentFilter),
            20,
            CompletionStop::Refused,
        ),
        (
            Some(FinishReason::Other("new_reason".into())),
            20,
            CompletionStop::Unknown {
                reason: "new_reason".into(),
            },
        ),
        (
            None,
            20,
            CompletionStop::Truncated(super::super::Truncation::OutputCap),
        ),
    ] {
        let (tx, _) = crossbeam_channel::unbounded();
        let mut response = ResponseCollector::new(&config(), tx);
        response.push(text_item("answer"));
        let (_, completion) = response.finish(
            ResponseEnd::Finished,
            None,
            Some(completion_response(
                reason_text(reason.as_ref()),
                Some((tokens, tokens)),
                vec![AssistantContent::Text(rig_core::message::Text::new(
                    "answer",
                ))],
            )),
        );
        assert_eq!(completion.stop, expected);
    }
    let (tx, _) = crossbeam_channel::unbounded();
    let response = ResponseCollector::new(&config(), tx);
    assert!(matches!(
        response.finish(ResponseEnd::Finished, None, None).1.stop,
        CompletionStop::Unknown { .. }
    ));
}
