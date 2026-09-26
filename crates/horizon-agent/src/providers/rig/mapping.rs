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
pub(super) const RIG_PROVIDER_PAYLOAD_VERSION: u32 = 1;

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
        // The old `call_id.unwrap_or(id)` resolution, on rig 0.42's shapes:
        // the provider-issued id when the call carries one, rig's
        // correlation handle otherwise.
        call_id: ToolCallId(
            call.provider
                .as_ref()
                .map(|p| p.call_id.clone())
                .unwrap_or_else(|| call.id.as_str().to_string()),
        ),
        tool_id: call.function.name,
        input: call.function.arguments.into(),
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
                "id": call.id.as_str(),
                "call_id": call.provider.as_ref().map(|p| p.call_id.clone()),
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
        rig_core::message::ToolCallId::new_or_mint(request.call_id.0.clone()),
        ToolFunction::new(request.tool_id.clone(), arguments),
    )
}

/// `tool_id` is the executed tool's id -- rig 0.42 requires it on every
/// tool result (`Message::tool_result`'s `name`; several wires key the
/// replay on it). Callers source it from the `ToolCallRequested` that
/// announced the call: the replayed call, the pending-tool-call
/// descriptors, or the result-producing context.
pub(super) fn rig_tool_result_message(result: &ToolCallResult, tool_id: &str) -> Message {
    Message::tool_result(
        result.call_id.0.clone(),
        tool_id,
        serde_json::json!({"outcome": result.outcome, "output": result.output}).to_string(),
    )
}

/// One `fs.read` of `path` — the deterministic fallback's hook for driving
/// a real file read (an out-of-workspace one included) through the whole
/// tool pipeline without a network provider. See
/// `completion::deterministic_rig_response`.
pub(super) fn rig_fs_read_call(path: &str) -> ToolCall {
    ToolCall::new(
        rig_core::message::ToolCallId::new_or_mint("rig-fs-read-1"),
        ToolFunction::new("fs.read".to_string(), serde_json::json!({ "path": path })),
    )
}

pub(super) fn rig_workspace_snapshot_call() -> ToolCall {
    ToolCall::new(
        rig_core::message::ToolCallId::new_or_mint("rig-workspace-snapshot-1"),
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
                rig_core::message::ToolCallId::new_or_mint(format!("rig-multi-snapshot-{index}")),
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
        provider: rig_core::message::ProviderCallId::new("provider-call-1"),
        signature: Some("signature-1".to_string()),
        additional_params: Some(serde_json::json!({
            "provider": "rig",
            "reasoning_ref": "reasoning-1"
        })),
        ..rig_workspace_snapshot_call()
    }
}
