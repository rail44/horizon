use rig_core::completion::{
    message::{ToolCall, ToolFunction},
    AssistantContent, Message, ToolDefinition,
};

use crate::agent::{
    tools::AgentToolDefinition, AgentEvent, AgentMessage, AgentMessageRole, AgentProviderEvent,
    AgentToolCallId, AgentToolCallRequest, AgentToolCallResult, AgentToolPermission,
};

pub(super) const RIG_PROVIDER_PAYLOAD_SCHEMA: &str = "horizon.rig.provider_payload";
pub(super) const RIG_PROVIDER_PAYLOAD_VERSION: u32 = 1;

pub fn horizon_events_from_rig_message(message: Message) -> Vec<AgentEvent> {
    horizon_provider_events_from_rig_message(message)
        .into_iter()
        .map(|event| event.event)
        .collect()
}

pub fn horizon_provider_events_from_rig_message(message: Message) -> Vec<AgentProviderEvent> {
    match message {
        Message::System { content } => vec![AgentEvent::MessageCommitted(AgentMessage {
            role: AgentMessageRole::Assistant,
            text: format!("system: {content}"),
        })
        .into()],
        Message::User { content } => content
            .into_iter()
            .filter_map(|content| match content {
                rig_core::completion::message::UserContent::Text(text) => Some(
                    AgentEvent::MessageCommitted(AgentMessage {
                        role: AgentMessageRole::User,
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
                        AgentEvent::MessageCommitted(AgentMessage {
                            role: AgentMessageRole::Assistant,
                            text: text.text,
                        })
                        .into(),
                    ),
                    AssistantContent::ToolCall(call) => {
                        output_events.push(AgentProviderEvent::with_provider_payload(
                            AgentEvent::ToolCallRequested(rig_tool_call_request(call.clone())),
                            rig_tool_call_provider_payload(&call),
                        ));
                    }
                    AssistantContent::Reasoning(reasoning) => {
                        let text = reasoning.display_text();
                        if !text.is_empty() {
                            reasoning_events.push(
                                AgentEvent::ReasoningDelta(crate::agent::AgentMessageDelta {
                                    role: AgentMessageRole::Assistant,
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

pub fn horizon_tool_definition_from_rig(
    definition: ToolDefinition,
    permission: AgentToolPermission,
) -> AgentToolDefinition {
    AgentToolDefinition {
        id: definition.name,
        title: definition.description.clone(),
        description: definition.description,
        input_schema: definition.parameters,
        permission,
    }
}

pub fn rig_messages_from_horizon_events(events: &[AgentEvent]) -> Vec<Message> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageCommitted(message) => match message.role {
                AgentMessageRole::User => Some(Message::user(message.text.clone())),
                AgentMessageRole::Assistant => Some(Message::assistant(message.text.clone())),
            },
            AgentEvent::ToolCallRequested(request) => {
                Some(Message::from(rig_tool_call_from_request(request)))
            }
            AgentEvent::ToolCallFinished(result) => Some(rig_tool_result_message(result)),
            AgentEvent::Error(error) => {
                Some(Message::assistant(format!("error: {}", error.message)))
            }
            AgentEvent::StateChanged(_)
            | AgentEvent::ReasoningDelta(_)
            | AgentEvent::AssistantTextDelta(_)
            | AgentEvent::ToolCallStarted(_)
            | AgentEvent::ApprovalRequested(_)
            | AgentEvent::Exited(_) => None,
        })
        .collect()
}

pub(super) fn rig_tool_call_request(call: ToolCall) -> AgentToolCallRequest {
    AgentToolCallRequest {
        call_id: AgentToolCallId(call.call_id.unwrap_or(call.id)),
        tool_id: call.function.name,
        input: call.function.arguments,
    }
}

pub fn rig_tool_call_provider_payload(call: &ToolCall) -> serde_json::Value {
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

fn rig_tool_call_from_request(request: &AgentToolCallRequest) -> ToolCall {
    ToolCall::new(
        request.call_id.0.clone(),
        ToolFunction::new(request.tool_id.clone(), request.input.clone()),
    )
}

pub(super) fn rig_tool_result_message(result: &AgentToolCallResult) -> Message {
    Message::tool_result(result.call_id.0.clone(), result.output.to_string())
}

pub(super) fn rig_workspace_snapshot_call() -> ToolCall {
    ToolCall::new(
        "rig-workspace-snapshot-1".to_string(),
        ToolFunction::new("workspace.snapshot".to_string(), serde_json::json!({})),
    )
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
