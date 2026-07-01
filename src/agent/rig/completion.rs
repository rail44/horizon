use crossbeam_channel::Sender;
use futures_util::StreamExt;
use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::{
    completion::{message::Text, AssistantContent, CompletionModel, Message, ToolDefinition},
    providers::openai,
    streaming::StreamedAssistantContent,
    OneOrMany,
};

use crate::{
    agent::{
        tools::{agent_tool_definitions, AgentToolDefinition},
        AgentEvent, AgentMessage, AgentMessageRole, AgentProviderEvent, AgentToolCallResult,
    },
    agent_config::RigAgentConfig,
    workspace::SessionId,
};

use super::{
    horizon_provider_events_from_rig_message, rig_tool_call_provider_payload,
    rig_tool_call_request, rig_workspace_snapshot_call, StreamDeltaBuffer, StreamDeltaKind,
};

pub(super) fn complete_rig_turn(
    runtime: Option<&tokio::runtime::Runtime>,
    config: &RigAgentConfig,
    rig_history: &mut Vec<Message>,
    prompt: Message,
    events_tx: &Sender<AgentProviderEvent>,
    fallback: impl FnOnce() -> Message,
) -> bool {
    if config.openai_enabled {
        if let Some(runtime) = runtime {
            match runtime.block_on(rig_openai_turn_streaming(
                config,
                prompt.clone(),
                rig_history.clone(),
                events_tx.clone(),
            )) {
                Ok((assistant_message, contains_tool_call)) => {
                    rig_history.push(prompt);
                    rig_history.push(assistant_message);
                    return contains_tool_call;
                }
                Err(error) => {
                    let _ = events_tx.send(
                        AgentEvent::Error(crate::agent::AgentError {
                            message: format!("Rig OpenAI completion failed: {error}"),
                        })
                        .into(),
                    );
                    return false;
                }
            }
        }

        let _ = events_tx.send(
            AgentEvent::Error(crate::agent::AgentError {
                message: "Rig OpenAI completion unavailable: failed to create Tokio runtime."
                    .to_string(),
            })
            .into(),
        );
        return false;
    }

    let assistant_message = fallback();
    rig_history.push(prompt);
    rig_history.push(assistant_message.clone());
    let events = horizon_provider_events_from_rig_message(assistant_message);
    let contains_tool_call = events_contain_tool_call(&events);
    for event in events {
        let _ = events_tx.send(event);
    }
    contains_tool_call
}

async fn rig_openai_turn_streaming(
    config: &RigAgentConfig,
    prompt: Message,
    history: Vec<Message>,
    events_tx: Sender<AgentProviderEvent>,
) -> anyhow::Result<(Message, bool)> {
    let client = openai::CompletionsClient::from_env()?;
    let model = client.completion_model(&config.model);
    let mut stream = model
        .completion_request(prompt)
        .messages(history)
        .tools(rig_tool_definitions())
        .preamble(rig_system_preamble())
        .stream()
        .await?;

    let mut text = String::new();
    let mut contains_tool_call = false;
    let mut text_buffer = StreamDeltaBuffer::new(
        events_tx.clone(),
        StreamDeltaKind::AssistantText,
        AgentMessageRole::Assistant,
    );
    let mut reasoning_buffer = StreamDeltaBuffer::new(
        events_tx.clone(),
        StreamDeltaKind::Reasoning,
        AgentMessageRole::Assistant,
    );

    while let Some(chunk) = stream.next().await {
        match chunk? {
            StreamedAssistantContent::Text(delta) => {
                text.push_str(&delta.text);
                text_buffer.push(delta.text);
            }
            StreamedAssistantContent::ReasoningDelta { reasoning, .. } => {
                reasoning_buffer.push(reasoning);
            }
            StreamedAssistantContent::Reasoning(reasoning) => {
                reasoning_buffer.flush();
                let text = reasoning.display_text();
                if !text.is_empty() {
                    let _ = events_tx.send(
                        AgentEvent::ReasoningDelta(crate::agent::AgentMessageDelta {
                            role: AgentMessageRole::Assistant,
                            text,
                        })
                        .into(),
                    );
                }
            }
            StreamedAssistantContent::ToolCall { tool_call, .. } => {
                reasoning_buffer.flush();
                text_buffer.flush();
                contains_tool_call = true;
                let _ = events_tx.send(AgentProviderEvent::with_provider_payload(
                    AgentEvent::ToolCallRequested(rig_tool_call_request(tool_call.clone())),
                    rig_tool_call_provider_payload(&tool_call),
                ));
            }
            StreamedAssistantContent::ToolCallDelta { .. } | StreamedAssistantContent::Final(_) => {
            }
        }
    }

    reasoning_buffer.flush();
    text_buffer.flush();

    if !text.is_empty() {
        let _ = events_tx.send(
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text,
            })
            .into(),
        );
    }

    Ok((
        Message::Assistant {
            id: stream.message_id.clone(),
            content: stream.choice.clone(),
        },
        contains_tool_call,
    ))
}

pub(super) fn load_rig_history(
    path: Option<&std::path::Path>,
    session_id: SessionId,
) -> Vec<Message> {
    let Some(path) = path else {
        return Vec::new();
    };

    crate::agent::duckdb_state::DuckDbAgentStateStore::open(path)
        .and_then(|store| store.rig_messages_for_session(session_id))
        .unwrap_or_default()
}

pub(super) fn deterministic_rig_response(text: &str) -> Message {
    if text.to_ascii_lowercase().contains("snapshot") {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(rig_workspace_snapshot_call())),
        }
    } else {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text::new(format!(
                "rig-core fallback response: {text}"
            )))),
        }
    }
}

pub(super) fn deterministic_tool_result_response(result: &AgentToolCallResult) -> Message {
    Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text::new(format!(
            "Tool result received for {}.",
            result.call_id.0
        )))),
    }
}

fn rig_system_preamble() -> String {
    "You are the Horizon agent. Use available tools when workspace state is needed. Return concise, directly useful answers.".to_string()
}

fn rig_tool_definitions() -> Vec<ToolDefinition> {
    agent_tool_definitions()
        .into_iter()
        .map(rig_tool_definition_from_horizon)
        .collect()
}

fn rig_tool_definition_from_horizon(definition: AgentToolDefinition) -> ToolDefinition {
    ToolDefinition {
        name: definition.id,
        description: definition.description,
        parameters: definition.input_schema,
    }
}

fn events_contain_tool_call(events: &[AgentProviderEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event.event, AgentEvent::ToolCallRequested(_)))
}
