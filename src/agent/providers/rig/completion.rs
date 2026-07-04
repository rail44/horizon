use std::collections::HashMap;

use crossbeam_channel::Sender;
use futures_util::StreamExt;
use rig_core::client::CompletionClient;
use rig_core::{
    completion::{
        message::{Text, ToolCall},
        AssistantContent, CompletionModel, Message, ToolDefinition,
    },
    providers::openai,
    streaming::{StreamedAssistantContent, ToolCallDeltaContent},
    OneOrMany,
};
use tokio_util::sync::CancellationToken;

use crate::{
    agent::config::RigAgentConfig,
    agent::{
        contract::{
            Error, Event, Message as AgentMessage, MessageDelta, MessageRole, ProviderEvent,
            ProviderRequestSent, ToolCallId, ToolCallResult,
        },
        prompt::{system_prompt, SessionEnvironment},
        tools::{definitions, Definition},
    },
};

use super::{
    mapping::{
        horizon_provider_events_from_rig_message, rig_tool_call_provider_payload,
        rig_tool_call_request,
    },
    rig_workspace_snapshot_call, StreamDeltaBuffer, StreamDeltaKind, ToolCallProgressBuffer,
};

/// What the session loop must remember about a requested tool call while
/// its result is outstanding: the tool id and the call's arguments.
/// Together with the eventual output they form the (tool, args, result)
/// doom-loop fingerprint in `session.rs` — args included per the design
/// doc, so distinct calls that happen to produce identical output (e.g.
/// greps for different patterns, each with zero matches) are not mistaken
/// for a loop.
#[derive(Clone, Debug, Default)]
pub(super) struct ToolCallDescriptor {
    pub(super) tool_id: String,
    pub(super) args: serde_json::Value,
}

/// Outcome of a single turn: which tool calls (if any) it requested (with
/// a descriptor per call id, for the doom-loop fingerprint in
/// `session.rs`), and whether it ended via cancellation rather than running
/// to completion. Cancellation is a stop reason, not an error — the caller
/// still gets a well-formed outcome, just with `cancelled: true`.
#[derive(Debug, Default)]
pub(super) struct TurnCompletion {
    pub(super) requested_tool_call_ids: Vec<ToolCallId>,
    pub(super) requested_tool_calls: HashMap<ToolCallId, ToolCallDescriptor>,
    pub(super) cancelled: bool,
}

pub(super) async fn complete_rig_turn(
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    rig_history: &mut Vec<Message>,
    prompt: Message,
    events_tx: &Sender<ProviderEvent>,
    fallback: impl FnOnce() -> Message,
    token: &CancellationToken,
) -> TurnCompletion {
    if config.openai_enabled {
        match rig_openai_turn_streaming(
            config,
            environment,
            prompt.clone(),
            rig_history.clone(),
            events_tx.clone(),
            token,
        )
        .await
        {
            Ok((assistant_message, completion)) => {
                rig_history.push(prompt);
                rig_history.push(assistant_message);
                return completion;
            }
            Err(error) => {
                let _ = events_tx.send(
                    Event::Error(Error {
                        message: format!("Rig OpenAI completion failed: {error}"),
                    })
                    .into(),
                );
                return TurnCompletion::default();
            }
        }
    }

    let assistant_message = fallback();
    rig_history.push(prompt);
    rig_history.push(assistant_message.clone());
    let events = horizon_provider_events_from_rig_message(assistant_message);
    let requested = tool_call_requests_from_events(&events);
    let requested_tool_call_ids = requested.iter().map(|(id, _)| id.clone()).collect();
    let requested_tool_calls = requested.into_iter().collect();
    for event in events {
        let _ = events_tx.send(event);
    }
    TurnCompletion {
        requested_tool_call_ids,
        requested_tool_calls,
        cancelled: false,
    }
}

async fn rig_openai_turn_streaming(
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    prompt: Message,
    history: Vec<Message>,
    events_tx: Sender<ProviderEvent>,
    token: &CancellationToken,
) -> anyhow::Result<(Message, TurnCompletion)> {
    let client = openai_completions_client(config)?;
    let model = client.completion_model(&config.model);
    // Marks the request leaving Horizon for the provider, before the
    // (possibly slow) network call below — see `Event::ProviderRequestSent`'s
    // doc comment for why this is persisted rather than only observed live.
    let _ = events_tx.send(
        Event::ProviderRequestSent(ProviderRequestSent {
            model: config.model.clone(),
        })
        .into(),
    );
    let mut stream = model
        .completion_request(prompt)
        .messages(history)
        .tools(rig_tool_definitions())
        .preamble(system_prompt(environment))
        .temperature_opt(config.temperature)
        .max_tokens_opt(config.max_tokens)
        .stream()
        .await?;

    let mut first_token_seen = false;
    let mut text = String::new();
    let mut requested_tool_call_ids = Vec::new();
    let mut requested_tool_calls = HashMap::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut cancelled = false;
    let mut text_buffer = StreamDeltaBuffer::new(
        events_tx.clone(),
        StreamDeltaKind::AssistantText,
        MessageRole::Assistant,
        config,
    );
    let mut reasoning_buffer = StreamDeltaBuffer::new(
        events_tx.clone(),
        StreamDeltaKind::Reasoning,
        MessageRole::Assistant,
        config,
    );
    let mut tool_call_progress = ToolCallProgressBuffer::new(events_tx.clone(), config);

    loop {
        let chunk = tokio::select! {
            _ = token.cancelled() => {
                cancelled = true;
                break;
            }
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };
        if !first_token_seen {
            first_token_seen = true;
            // The gap between `ProviderRequestSent` above and this event is
            // provider time-to-first-byte, regardless of what kind of chunk
            // arrived first (text, reasoning, or a tool-call delta).
            let _ = events_tx.send(Event::ProviderRequestFirstToken.into());
        }

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
                        Event::ReasoningDelta(MessageDelta {
                            role: MessageRole::Assistant,
                            text,
                        })
                        .into(),
                    );
                }
            }
            StreamedAssistantContent::ToolCall { tool_call, .. } => {
                reasoning_buffer.flush();
                text_buffer.flush();
                let request = rig_tool_call_request(tool_call.clone());
                requested_tool_call_ids.push(request.call_id.clone());
                requested_tool_calls.insert(
                    request.call_id.clone(),
                    ToolCallDescriptor {
                        tool_id: request.tool_id.clone(),
                        args: request.input.clone(),
                    },
                );
                let _ = events_tx.send(ProviderEvent::with_provider_payload(
                    Event::ToolCallRequested(request),
                    rig_tool_call_provider_payload(&tool_call),
                ));
                tool_calls.push(tool_call);
            }
            // Tool-call arguments can arrive as many small chunks (a 4.7KB
            // `fs.write` argument produced 13s of otherwise-silent
            // streaming — see the design note on `ToolCallProgressBuffer`).
            // These were previously dropped entirely; now they surface as
            // coalesced, ephemeral `ToolCallProgress` ticks so the pane can
            // show "preparing a tool call… (N bytes)" instead of going
            // quiet mid-turn.
            StreamedAssistantContent::ToolCallDelta {
                internal_call_id,
                content,
                ..
            } => match content {
                ToolCallDeltaContent::Name(name) => {
                    tool_call_progress.note_name(&internal_call_id, name);
                }
                ToolCallDeltaContent::Delta(delta) => {
                    tool_call_progress.note_delta(&internal_call_id, &delta);
                }
            },
            StreamedAssistantContent::Final(_) => {}
        }
    }

    // The provider's response stream is done, either exhausted normally or
    // cut short by cancellation — either way, the request's wall-clock span
    // ends here, before the resulting message/tool-call events below.
    let _ = events_tx.send(Event::ProviderRequestFinished.into());

    reasoning_buffer.flush();
    text_buffer.flush();

    if !text.is_empty() {
        let _ = events_tx.send(
            Event::MessageCommitted(AgentMessage {
                role: MessageRole::Assistant,
                text: text.clone(),
            })
            .into(),
        );
    }

    // `stream.choice` is only aggregated when the stream runs to its end;
    // on cancellation it is still the empty placeholder, so the history
    // message must be assembled from the chunks observed before the cancel —
    // otherwise the streamed partial (text and especially tool calls) would
    // be lost from history and cancelled tool results would dangle.
    let assistant_message = if cancelled {
        partial_assistant_message(stream.message_id.clone(), &text, tool_calls)
    } else {
        Message::Assistant {
            id: stream.message_id.clone(),
            content: stream.choice.clone(),
        }
    };

    Ok((
        assistant_message,
        TurnCompletion {
            requested_tool_call_ids,
            requested_tool_calls,
            cancelled,
        },
    ))
}

/// Builds the OpenAI Completions client for a turn.
///
/// The API key is always read straight from `OPENAI_API_KEY` — secrets
/// never flow through the config file (`agent::config`'s module doc) — so
/// this can't just call `openai::CompletionsClient::from_env()` the way it
/// used to: that helper also reads `OPENAI_BASE_URL` itself, which would
/// silently ignore Horizon's own precedence for the base URL. Instead the
/// base URL comes from `config.base_url`, already resolved by
/// `agent::config::RigAgentConfig::from_env` with the right precedence (env
/// `OPENAI_BASE_URL` > `[provider].base_url` in the config file); `None`
/// leaves rig's own default (`https://api.openai.com/v1`) in place by
/// simply not calling `.base_url(..)` on the builder, mirroring exactly
/// what `from_env()` did before.
fn openai_completions_client(config: &RigAgentConfig) -> anyhow::Result<openai::CompletionsClient> {
    let api_key = std::env::var(crate::agent::config::OPENAI_API_KEY_VAR)
        .map_err(|_| anyhow::anyhow!("{} is not set", crate::agent::config::OPENAI_API_KEY_VAR))?;

    let mut builder = openai::CompletionsClient::builder().api_key(&api_key);
    if let Some(base_url) = &config.base_url {
        builder = builder.base_url(base_url);
    }
    builder.build().map_err(Into::into)
}

/// Builds the assistant history message for a cancelled turn from whatever
/// streamed before cancellation: the accumulated text (if any) followed by
/// the tool calls that were already emitted as `ToolCallRequested` events.
pub(super) fn partial_assistant_message(
    message_id: Option<String>,
    text: &str,
    tool_calls: Vec<ToolCall>,
) -> Message {
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(AssistantContent::Text(Text::new(text.to_string())));
    }
    content.extend(tool_calls.into_iter().map(AssistantContent::ToolCall));

    Message::Assistant {
        id: message_id,
        content: OneOrMany::many(content)
            .unwrap_or_else(|_| OneOrMany::one(AssistantContent::Text(Text::new(String::new())))),
    }
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

pub(super) fn deterministic_tool_result_response(result: &ToolCallResult) -> Message {
    // Deterministic hook for exercising the tool-call loop without a
    // network provider: a result whose output sets `"loop_again": true`
    // makes the fallback responder request the snapshot tool again, so
    // tests can drive consecutive tool-driven turns (e.g. the
    // iteration-cap guard). Real tool outputs never carry this key.
    if result.output.get("loop_again") == Some(&serde_json::Value::Bool(true)) {
        return Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(rig_workspace_snapshot_call())),
        };
    }
    Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text::new(format!(
            "Tool result received for {}.",
            result.call_id.0
        )))),
    }
}

fn rig_tool_definitions() -> Vec<ToolDefinition> {
    definitions()
        .into_iter()
        .map(rig_tool_definition_from_horizon)
        .collect()
}

fn rig_tool_definition_from_horizon(definition: Definition) -> ToolDefinition {
    ToolDefinition {
        name: definition.id,
        description: definition.description,
        parameters: definition.input_schema,
    }
}

fn tool_call_requests_from_events(
    events: &[ProviderEvent],
) -> Vec<(ToolCallId, ToolCallDescriptor)> {
    events
        .iter()
        .filter_map(|event| match &event.event {
            Event::ToolCallRequested(request) => Some((
                request.call_id.clone(),
                ToolCallDescriptor {
                    tool_id: request.tool_id.clone(),
                    args: request.input.clone(),
                },
            )),
            _ => None,
        })
        .collect()
}
