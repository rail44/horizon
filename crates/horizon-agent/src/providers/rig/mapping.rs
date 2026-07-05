use rig_core::completion::{
    message::{ToolCall, ToolFunction},
    AssistantContent, Message,
};

use crate::contract::{
    Event, Message as AgentMessage, MessageDelta, MessageRole, ProviderEvent, ToolCallId,
    ToolCallRequest, ToolCallResult,
};

#[cfg(test)]
use rig_core::completion::ToolDefinition;

#[cfg(test)]
use crate::{contract::ToolPermission, tools::Definition};

pub(super) const RIG_PROVIDER_PAYLOAD_SCHEMA: &str = "horizon.rig.provider_payload";
pub(super) const RIG_PROVIDER_PAYLOAD_VERSION: u32 = 1;

#[cfg(test)]
pub(super) fn horizon_events_from_rig_message(message: Message) -> Vec<Event> {
    horizon_provider_events_from_rig_message(message)
        .into_iter()
        .map(|event| event.event)
        .collect()
}

pub(super) fn horizon_provider_events_from_rig_message(message: Message) -> Vec<ProviderEvent> {
    match message {
        Message::System { content } => vec![Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: format!("system: {content}"),
        })
        .into()],
        Message::User { content } => content
            .into_iter()
            .filter_map(|content| match content {
                rig_core::completion::message::UserContent::Text(text) => Some(
                    Event::MessageCommitted(AgentMessage {
                        role: MessageRole::User,
                        text: text.text,
                    })
                    .into(),
                ),
                _ => None,
            })
            .collect(),
        Message::Assistant { content, .. } => {
            let mut reasoning_events = Vec::new();
            let mut output_events = Vec::new();

            for content in content {
                match content {
                    AssistantContent::Text(text) => output_events.push(
                        Event::MessageCommitted(AgentMessage {
                            role: MessageRole::Assistant,
                            text: text.text,
                        })
                        .into(),
                    ),
                    AssistantContent::ToolCall(call) => {
                        output_events.push(ProviderEvent::with_provider_payload(
                            Event::ToolCallRequested(rig_tool_call_request(call.clone())),
                            rig_tool_call_provider_payload(&call),
                        ));
                    }
                    AssistantContent::Reasoning(reasoning) => {
                        let text = reasoning.display_text();
                        if !text.is_empty() {
                            reasoning_events.push(
                                Event::ReasoningDelta(MessageDelta {
                                    role: MessageRole::Assistant,
                                    text,
                                })
                                .into(),
                            );
                        }
                    }
                    AssistantContent::Image(_) => {}
                }
            }

            reasoning_events.extend(output_events);
            reasoning_events
        }
    }
}

#[cfg(test)]
pub(super) fn horizon_tool_definition_from_rig(
    definition: ToolDefinition,
    permission: ToolPermission,
) -> Definition {
    Definition {
        id: definition.name,
        title: definition.description.clone(),
        description: definition.description,
        input_schema: definition.parameters,
        permission,
    }
}

pub(super) fn rig_messages_from_horizon_events(events: &[Event]) -> Vec<Message> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::MessageCommitted(message) => match message.role {
                MessageRole::User => Some(Message::user(message.text.clone())),
                MessageRole::Assistant => Some(Message::assistant(message.text.clone())),
            },
            Event::ToolCallRequested(request) => {
                Some(Message::from(rig_tool_call_from_request(request)))
            }
            Event::ToolCallFinished(result) => Some(rig_tool_result_message(result)),
            Event::Error(error) => Some(Message::assistant(format!("error: {}", error.message))),
            Event::StateChanged(_)
            | Event::ReasoningDelta(_)
            | Event::AssistantTextDelta(_)
            | Event::ToolCallStarted(_)
            | Event::ApprovalRequested(_)
            | Event::ProviderRequestSent(_)
            | Event::ProviderRequestFirstToken
            | Event::ProviderRequestFinished
            | Event::Exited(_)
            | Event::TurnEnded(_) => None,
        })
        .collect()
}

pub(super) fn rig_tool_call_request(call: ToolCall) -> ToolCallRequest {
    ToolCallRequest {
        call_id: ToolCallId(call.call_id.unwrap_or(call.id)),
        tool_id: call.function.name,
        input: call.function.arguments,
    }
}

pub(super) fn rig_tool_call_provider_payload(call: &ToolCall) -> serde_json::Value {
    serde_json::json!({
        "schema": RIG_PROVIDER_PAYLOAD_SCHEMA,
        "version": RIG_PROVIDER_PAYLOAD_VERSION,
        "rig": {
            "tool_call": {
                "id": call.id.clone(),
                "call_id": call.call_id.clone(),
                "signature": call.signature.clone(),
                "additional_params": call.additional_params.clone(),
                "function": {
                    "name": call.function.name.clone(),
                    "arguments": call.function.arguments.clone(),
                }
            }
        }
    })
}

fn rig_tool_call_from_request(request: &ToolCallRequest) -> ToolCall {
    ToolCall::new(
        request.call_id.0.clone(),
        ToolFunction::new(request.tool_id.clone(), request.input.clone()),
    )
}

pub(super) fn rig_tool_result_message(result: &ToolCallResult) -> Message {
    Message::tool_result(result.call_id.0.clone(), result.output.to_string())
}

pub(super) fn rig_workspace_snapshot_call() -> ToolCall {
    ToolCall::new(
        "rig-workspace-snapshot-1".to_string(),
        ToolFunction::new("workspace.snapshot".to_string(), serde_json::json!({})),
    )
}

/// `count` distinct `workspace.snapshot` calls (a fresh call id and an
/// index in the arguments per call, so each has its own doom-loop
/// fingerprint) — the deterministic fallback's hook for exercising a
/// parallel-tool-call batch (e.g. `deterministic_rig_response`'s "multi
/// tool" trigger) without a network provider. Mirrors the shape of a real
/// completion that requests several tool calls at once (the production
/// incident this covers: a MiniMax completion routinely requesting 4
/// parallel `fs.read`s).
pub(super) fn rig_multi_snapshot_calls(count: usize) -> Vec<ToolCall> {
    (1..=count)
        .map(|index| {
            ToolCall::new(
                format!("rig-multi-snapshot-{index}"),
                ToolFunction::new(
                    "workspace.snapshot".to_string(),
                    serde_json::json!({ "n": index }),
                ),
            )
        })
        .collect()
}

#[cfg(test)]
pub(super) fn rig_workspace_snapshot_call_with_provider_metadata() -> ToolCall {
    ToolCall {
        call_id: Some("provider-call-1".to_string()),
        signature: Some("signature-1".to_string()),
        additional_params: Some(serde_json::json!({
            "provider": "rig",
            "reasoning_ref": "reasoning-1"
        })),
        ..rig_workspace_snapshot_call()
    }
}
