use crate::contract::{
    Event, Message as AgentMessage, MessageDelta, MessageRole, OccurrenceId, ProviderEvent,
    ToolCallId, ToolCallRequest, ToolCallResult,
};
use rig_core::completion::{
    message::{ToolCall, ToolFunction},
    AssistantContent, Message,
};

#[cfg(test)]
use rig_core::completion::ToolDefinition;

#[cfg(test)]
use crate::{contract::ToolPermission, tools::Definition};

#[cfg(test)]
mod tests;

pub(super) const RIG_PROVIDER_PAYLOAD_SCHEMA: &str = "horizon.rig.provider_payload";
pub(super) const RIG_PROVIDER_PAYLOAD_VERSION: u32 = 2;

#[cfg(test)]
pub(super) fn horizon_events_from_rig_message(message: Message) -> Vec<Event> {
    horizon_provider_events_from_rig_message(message)
        .into_iter()
        .filter_map(ProviderEvent::into_event)
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
        Message::Assistant(assistant) => {
            let mut reasoning_events = Vec::new();
            let mut output_events = Vec::new();

            for content in assistant.content {
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
                        let text = reasoning.text;
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
                    AssistantContent::Image(_) | AssistantContent::Opaque(_) => {}
                    _ => {}
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
        id: definition.name.as_str().to_string(),
        title: definition.description.clone(),
        description: definition.description,
        input_schema: definition.parameters,
        permission,
    }
}

#[cfg(test)]
pub(super) fn rig_messages_from_horizon_events(events: &[Event]) -> Vec<Message> {
    let events = if events
        .iter()
        .any(|event| matches!(event, Event::ConversationRecorded(_)))
    {
        events.to_vec()
    } else {
        super::conversation::upgrade::fixture(events.to_vec())
    };
    super::conversation::ConversationHistory::from_events(&events)
        .unwrap()
        .messages()
}

pub(super) fn rig_tool_call_request(call: ToolCall) -> ToolCallRequest {
    ToolCallRequest {
        // The provider-issued id when the call carries one, rig's own
        // correlation handle otherwise (`CallId` keeps both).
        call_id: ToolCallId(
            call.id
                .provider()
                .map(|p| p.as_str().to_string())
                .unwrap_or_else(|| call.id.wire().into_owned()),
        ),
        tool_id: call.function.name.as_str().to_string(),
        input: serde_json::Value::Object(call.function.arguments).into(),
        // Mint a fresh `OccurrenceId` here -- the upstream provider only
        // knows its own `call_id`, which it can reuse across genuinely
        // distinct calls (the `functions.fs.edit:66` incident in session
        // 05254b6a) and which Horizon also reuses on every sandbox-denial
        // retry. `OccurrenceId` is the per-occurrence second key the
        // transcript, approval, and analytics each follow; a UUID v4 is
        // globally unique without coordination across resumed sessions
        // and replayed logs, which a per-process counter would not be.
        occurrence_id: OccurrenceId::new(),
    }
}

pub(super) fn rig_tool_call_provider_payload(call: &ToolCall) -> serde_json::Value {
    serde_json::json!({
        "schema": RIG_PROVIDER_PAYLOAD_SCHEMA,
        "version": RIG_PROVIDER_PAYLOAD_VERSION,
        "rig": {
            "tool_call": {
                "id": call.id.wire(),
                "call_id": call.id.provider().map(|p| p.as_str()),
                "native": call.native.as_ref().map(|native| native.item.clone()),
                "function": {
                    "name": call.function.name.as_str(),
                    "arguments": serde_json::Value::Object(call.function.arguments.clone()),
                }
            }
        }
    })
}

/// Rebuilds a rig tool call from a persisted `ToolCallRequested` event —
/// the history-reload path, so the arguments get the same normalization a
/// live turn applies (see `completion::replay_safe_tool_arguments`).
/// Without it, a session recorded before that repair existed would
/// re-poison itself the first time its history was reloaded from the
/// projection.
#[cfg(test)]
pub(super) fn rig_tool_call_from_request(request: &ToolCallRequest) -> ToolCall {
    let mut arguments = request.input.0.clone();
    super::completion::replay_safe_tool_arguments(&mut arguments);
    ToolCall::new(
        rig_core::message::CallId::from_wire(request.call_id.0.clone()),
        ToolFunction::new(
            rig_core::message::ToolName::new(request.tool_id.clone())
                .expect("horizon tool ids are never empty"),
            arguments,
        ),
    )
}

/// `tool_id` is the executed tool's id -- rig 0.42 requires it on every
/// tool result (`Message::tool_result`'s `name`; several wires key the
/// replay on it). Callers source it from the `ToolCallRequested` that
/// announced the call: the replayed call, the pending-tool-call
/// descriptors, or the result-producing context.
pub(super) fn rig_tool_result_message(result: &ToolCallResult, tool_id: &str) -> Message {
    let content =
        serde_json::json!({"outcome": result.outcome, "output": result.output}).to_string();
    Message::User {
        content: vec![rig_core::message::UserContent::ToolResult(
            rig_core::message::ToolResult {
                call: rig_core::message::CallId::from_wire(result.call_id.0.clone()),
                name: rig_core::message::ToolName::new(tool_id.to_string())
                    .expect("horizon tool ids are never empty"),
                content: vec![rig_core::message::ToolResultContent::text(content)],
                is_error: false,
            },
        )],
    }
}

/// One `fs.read` of `path` — the deterministic fallback's hook for driving
/// a real file read (an out-of-workspace one included) through the whole
/// tool pipeline without a network provider. See
/// `completion::deterministic_rig_response`.
pub(super) fn rig_fs_read_call(path: &str) -> ToolCall {
    ToolCall::new(
        rig_core::message::CallId::from_wire("rig-fs-read-1"),
        ToolFunction::new(
            rig_core::message::ToolName::new("fs.read").expect("fs.read is a tool id"),
            serde_json::json!({ "path": path }),
        ),
    )
}

pub(super) fn rig_workspace_snapshot_call() -> ToolCall {
    ToolCall::new(
        rig_core::message::CallId::from_wire("rig-workspace-snapshot-1"),
        ToolFunction::new(
            rig_core::message::ToolName::new("workspace.snapshot")
                .expect("workspace.snapshot is a tool id"),
            serde_json::json!({}),
        ),
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
                rig_core::message::CallId::from_wire(format!("rig-multi-snapshot-{index}")),
                ToolFunction::new(
                    rig_core::message::ToolName::new("workspace.snapshot")
                        .expect("workspace.snapshot is a tool id"),
                    serde_json::json!({ "n": index }),
                ),
            )
        })
        .collect()
}

#[cfg(test)]
pub(super) fn rig_workspace_snapshot_call_with_provider_metadata() -> ToolCall {
    ToolCall {
        native: Some(rig_core::message::Native {
            fingerprint: rig_core::message::Fingerprint::of(&serde_json::json!({
                "call_id": "provider-call-1",
                "signature": "signature-1",
            })),
            item: serde_json::json!({
                "call_id": "provider-call-1",
                "signature": "signature-1",
                "additional_params": {"provider": "rig", "reasoning_ref": "reasoning-1"},
            }),
        }),
        ..rig_workspace_snapshot_call()
    }
}
