use std::{path::PathBuf, thread};

use crossbeam_channel::{unbounded, Sender};
use futures_util::StreamExt;
use rig_core::client::{CompletionClient, ProviderClient};
use rig_core::{
    completion::{message::Text, AssistantContent, CompletionModel, Message, ToolDefinition},
    providers::openai,
    streaming::StreamedAssistantContent,
    OneOrMany,
};

mod mapping;
mod stream;

use mapping::{
    horizon_provider_events_from_rig_message, rig_tool_call_request, rig_tool_result_message,
    rig_workspace_snapshot_call,
};
use stream::{StreamDeltaBuffer, StreamDeltaKind};

pub use mapping::{
    horizon_events_from_rig_message, horizon_tool_definition_from_rig,
    rig_messages_from_horizon_events, rig_tool_call_provider_payload,
};

use crate::{
    agent::{
        tools::{agent_tool_definitions, AgentToolDefinition},
        AgentCommand, AgentEvent, AgentMessage, AgentMessageRole, AgentProvider,
        AgentProviderEvent, AgentProviderId, AgentSessionHandle, AgentSessionState,
        AgentToolCallResult, StartAgentSession,
    },
    agent_config::RigAgentConfig,
    workspace::SessionId,
};

pub struct RigAgentProvider {
    config: RigAgentConfig,
    memory_duckdb_path: Option<PathBuf>,
}

impl RigAgentProvider {
    pub fn new(config: RigAgentConfig, memory_duckdb_path: Option<PathBuf>) -> Self {
        Self {
            config,
            memory_duckdb_path,
        }
    }
}

impl AgentProvider for RigAgentProvider {
    fn provider_id(&self) -> AgentProviderId {
        AgentProviderId("builtin.agent.rig".to_string())
    }

    fn start_session(&self, request: StartAgentSession) -> AgentSessionHandle {
        let (commands_tx, commands_rx) = unbounded();
        let (events_tx, events_rx) = unbounded::<AgentProviderEvent>();
        let provider_id = request.provider_id;
        let config = self.config.clone();
        let memory_duckdb_path = self.memory_duckdb_path.clone();
        let session_id = request.session_id;

        thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().ok();
            let mut rig_history = load_rig_history(memory_duckdb_path.as_deref(), session_id);

            let _ = events_tx.send(AgentEvent::StateChanged(AgentSessionState::Created).into());
            let _ = events_tx.send(
                AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::Assistant,
                    text: rig_initialization_message(&provider_id, &config, rig_history.len()),
                })
                .into(),
            );
            let _ =
                events_tx.send(AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into());

            while let Ok(command) = commands_rx.recv() {
                match command {
                    AgentCommand::Initialize(_) => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::Running).into());
                        let _ = events_tx.send(
                            AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                        );
                    }
                    AgentCommand::UserMessage { text } => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::Running).into());
                        let _ = events_tx.send(
                            AgentEvent::MessageCommitted(AgentMessage {
                                role: AgentMessageRole::User,
                                text: text.clone(),
                            })
                            .into(),
                        );

                        let contains_tool_call = complete_rig_turn(
                            runtime.as_ref(),
                            &config,
                            &mut rig_history,
                            Message::user(text.clone()),
                            &events_tx,
                            || deterministic_rig_response(&text),
                        );
                        if !contains_tool_call {
                            let _ = events_tx.send(
                                AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                            );
                        }
                    }
                    AgentCommand::ToolCallResult(result) => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::Running).into());
                        let contains_tool_call = complete_rig_turn(
                            runtime.as_ref(),
                            &config,
                            &mut rig_history,
                            rig_tool_result_message(&result),
                            &events_tx,
                            || deterministic_tool_result_response(&result),
                        );
                        if !contains_tool_call {
                            let _ = events_tx.send(
                                AgentEvent::StateChanged(AgentSessionState::WaitingForUser).into(),
                            );
                        }
                    }
                    AgentCommand::Shutdown => {
                        let _ = events_tx
                            .send(AgentEvent::StateChanged(AgentSessionState::Terminated).into());
                        break;
                    }
                    AgentCommand::Cancel { .. }
                    | AgentCommand::ApproveToolCall { .. }
                    | AgentCommand::DenyToolCall { .. } => {}
                }
            }
        });

        AgentSessionHandle::new(commands_tx, events_rx)
    }
}

fn rig_initialization_message(
    provider_id: &AgentProviderId,
    config: &RigAgentConfig,
    loaded_history_messages: usize,
) -> String {
    let memory = if loaded_history_messages == 0 {
        String::new()
    } else {
        format!(" Loaded {loaded_history_messages} persisted Rig history message(s).")
    };
    if config.openai_enabled {
        format!(
            "Rig provider `{}` initialized with OpenAI model `{}`.{}",
            provider_id.0, config.model, memory
        )
    } else {
        format!(
            "Rig provider `{}` initialized in deterministic fallback mode.{}",
            provider_id.0, memory
        )
    }
}

fn complete_rig_turn(
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

fn rig_system_preamble() -> String {
    "You are the Horizon agent. Use available tools when workspace state is needed. Return concise, directly useful answers.".to_string()
}

fn load_rig_history(path: Option<&std::path::Path>, session_id: SessionId) -> Vec<Message> {
    let Some(path) = path else {
        return Vec::new();
    };

    crate::agent::duckdb_state::DuckDbAgentStateStore::open(path)
        .and_then(|store| store.rig_messages_for_session(session_id))
        .unwrap_or_default()
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

fn deterministic_rig_response(text: &str) -> Message {
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

fn deterministic_tool_result_response(result: &AgentToolCallResult) -> Message {
    Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text::new(format!(
            "Tool result received for {}.",
            result.call_id.0
        )))),
    }
}

fn events_contain_tool_call(events: &[AgentProviderEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event.event, AgentEvent::ToolCallRequested(_)))
}

#[cfg(test)]
mod tests {
    use super::mapping::{
        rig_workspace_snapshot_call_with_provider_metadata, RIG_PROVIDER_PAYLOAD_SCHEMA,
        RIG_PROVIDER_PAYLOAD_VERSION,
    };
    use super::*;
    use crate::agent::{AgentToolCallId, AgentToolCallRequest, AgentToolPermission};
    use rig_core::completion::message::{ToolResultContent, UserContent};

    #[test]
    fn converts_rig_assistant_text_to_horizon_message() {
        let events = horizon_events_from_rig_message(Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text::new("hello"))),
        });

        assert!(matches!(
            events.as_slice(),
            [AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text,
            })] if text == "hello"
        ));
    }

    #[test]
    fn emits_rig_reasoning_before_assistant_text() {
        let events = horizon_events_from_rig_message(Message::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::Text(Text::new("final answer")),
                AssistantContent::Reasoning(rig_core::completion::message::Reasoning::new(
                    "thinking first",
                )),
            ])
            .expect("assistant content"),
        });

        assert!(matches!(
            events.as_slice(),
            [
                AgentEvent::ReasoningDelta(delta),
                AgentEvent::MessageCommitted(AgentMessage {
                    role: AgentMessageRole::Assistant,
                    text,
                }),
            ] if delta.text == "thinking first" && text == "final answer"
        ));
    }

    #[test]
    fn converts_rig_tool_call_to_horizon_tool_request() {
        let events = horizon_events_from_rig_message(Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(rig_workspace_snapshot_call())),
        });

        assert!(matches!(
            events.as_slice(),
            [AgentEvent::ToolCallRequested(request)]
                if request.tool_id == "workspace.snapshot"
                    && request.call_id.0 == "rig-workspace-snapshot-1"
        ));
    }

    #[test]
    fn builds_versioned_rig_tool_call_provider_payload() {
        let call = rig_workspace_snapshot_call_with_provider_metadata();
        let payload = rig_tool_call_provider_payload(&call);

        assert_eq!(payload["schema"], RIG_PROVIDER_PAYLOAD_SCHEMA);
        assert_eq!(payload["version"], RIG_PROVIDER_PAYLOAD_VERSION);
        assert_eq!(
            payload["rig"]["tool_call"]["id"],
            "rig-workspace-snapshot-1"
        );
        assert_eq!(payload["rig"]["tool_call"]["call_id"], "provider-call-1");
        assert_eq!(payload["rig"]["tool_call"]["signature"], "signature-1");
        assert_eq!(
            payload["rig"]["tool_call"]["additional_params"]["reasoning_ref"],
            "reasoning-1"
        );
        assert_eq!(
            payload["rig"]["tool_call"]["function"]["name"],
            "workspace.snapshot"
        );
    }

    #[test]
    fn converts_rig_tool_call_to_provider_event_with_payload() {
        let events = horizon_provider_events_from_rig_message(Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(
                rig_workspace_snapshot_call_with_provider_metadata(),
            )),
        });

        assert!(matches!(
            events.as_slice(),
            [AgentProviderEvent {
                event: AgentEvent::ToolCallRequested(request),
                provider_payload: Some(payload),
            }] if request.call_id.0 == "provider-call-1"
                && payload["schema"] == RIG_PROVIDER_PAYLOAD_SCHEMA
                && payload["rig"]["tool_call"]["id"] == "rig-workspace-snapshot-1"
        ));
    }

    #[test]
    fn duckdb_store_preserves_rig_provider_payload_for_tool_call() {
        let store =
            crate::agent::duckdb_state::DuckDbAgentStateStore::open_in_memory().expect("store");
        let session_id = crate::workspace::SessionId::new();
        let call = rig_workspace_snapshot_call_with_provider_metadata();
        let provider_payload = rig_tool_call_provider_payload(&call);
        let event = AgentEvent::ToolCallRequested(rig_tool_call_request(call));

        store
            .append_event(crate::agent::duckdb_state::AppendAgentEvent {
                session_id,
                turn_id: Some("turn-1".to_string()),
                provider_id: Some(AgentProviderId("builtin.agent.rig".to_string())),
                event,
                provider_payload: Some(provider_payload.clone()),
            })
            .expect("append rig payload event");

        let events = store.events_for_session(session_id).expect("events");
        assert_eq!(
            events[0].provider_id,
            Some(AgentProviderId("builtin.agent.rig".to_string()))
        );
        assert_eq!(events[0].provider_payload, Some(provider_payload));
        assert_eq!(
            store
                .tool_calls_for_session(session_id)
                .expect("tool calls")[0]
                .call_id
                .0,
            "provider-call-1"
        );
    }

    #[test]
    fn converts_rig_tool_definition_without_leaking_rig_type() {
        let definition = horizon_tool_definition_from_rig(
            ToolDefinition {
                name: "workspace.snapshot".to_string(),
                description: "Read workspace state".to_string(),
                parameters: serde_json::json!({ "type": "object" }),
            },
            AgentToolPermission::AutoAllowRead,
        );

        assert_eq!(definition.id, "workspace.snapshot");
        assert_eq!(definition.permission, AgentToolPermission::AutoAllowRead);
    }

    #[test]
    fn rebuilds_rig_memory_messages_from_horizon_transcript_events() {
        let events = vec![
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::User,
                text: "snapshot please".to_string(),
            }),
            AgentEvent::ToolCallRequested(AgentToolCallRequest {
                call_id: AgentToolCallId("call-1".to_string()),
                tool_id: "workspace.snapshot".to_string(),
                input: serde_json::json!({}),
            }),
            AgentEvent::ToolCallFinished(AgentToolCallResult {
                call_id: AgentToolCallId("call-1".to_string()),
                output: serde_json::json!({ "tab_count": 1 }),
            }),
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: "There is one tab.".to_string(),
            }),
        ];

        let messages = rig_messages_from_horizon_events(&events);

        assert!(matches!(&messages[0], Message::User { .. }));
        assert!(matches!(
            &messages[1],
            Message::Assistant { content, .. }
                if matches!(content.first_ref(), AssistantContent::ToolCall(call)
                    if call.id == "call-1" && call.function.name == "workspace.snapshot")
        ));
        assert!(matches!(&messages[2], Message::User { content }
            if matches!(content.first_ref(), UserContent::ToolResult(result)
                if result.id == "call-1"
                    && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                        if text.text.contains("tab_count")))));
        assert!(matches!(&messages[3], Message::Assistant { .. }));
    }

    #[test]
    fn loads_initial_rig_history_from_duckdb_projection() {
        let path = std::env::temp_dir().join(format!(
            "horizon-rig-memory-{}.duckdb",
            uuid::Uuid::new_v4()
        ));
        let session_id = crate::workspace::SessionId::new();
        let events = vec![
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::User,
                text: "hello".to_string(),
            }),
            AgentEvent::AssistantTextDelta(crate::agent::AgentMessageDelta {
                role: AgentMessageRole::Assistant,
                text: "streaming ignored".to_string(),
            }),
            AgentEvent::MessageCommitted(AgentMessage {
                role: AgentMessageRole::Assistant,
                text: "hi".to_string(),
            }),
        ];

        {
            let store =
                crate::agent::duckdb_state::DuckDbAgentStateStore::open(&path).expect("open store");
            store
                .append_events(
                    session_id,
                    Some(AgentProviderId("builtin.agent.rig".to_string())),
                    events.clone(),
                )
                .expect("append events");
        }

        let history = load_rig_history(Some(&path), session_id);
        assert_eq!(
            history,
            rig_message_json_roundtrip(rig_messages_from_horizon_events(&events))
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn horizon_mediated_tool_result_can_continue_as_rig_history() {
        let tool_call = rig_workspace_snapshot_call();
        let mut events = horizon_events_from_rig_message(Message::from(tool_call));
        let request = match events.first().expect("tool request") {
            AgentEvent::ToolCallRequested(request) => request.clone(),
            other => panic!("expected tool request, got {other:?}"),
        };

        events.push(AgentEvent::ToolCallStarted(request.call_id.clone()));
        events.push(AgentEvent::ToolCallFinished(AgentToolCallResult {
            call_id: request.call_id.clone(),
            output: serde_json::json!({
                "tab_count": 1,
                "active_title": "Agent #1",
            }),
        }));

        let messages = rig_messages_from_horizon_events(&events);

        assert_eq!(messages.len(), 2);
        assert!(matches!(
            &messages[0],
            Message::Assistant { content, .. }
                if matches!(content.first_ref(), AssistantContent::ToolCall(call)
                    if call.id == request.call_id.0)
        ));
        assert!(matches!(&messages[1], Message::User { content }
            if matches!(content.first_ref(), UserContent::ToolResult(result)
                if result.id == request.call_id.0)));
    }

    fn rig_message_json_roundtrip(messages: Vec<Message>) -> Vec<Message> {
        messages
            .into_iter()
            .map(|message| {
                let json = serde_json::to_string(&message).expect("serialize Rig message");
                serde_json::from_str(&json).expect("deserialize Rig message")
            })
            .collect()
    }
}
