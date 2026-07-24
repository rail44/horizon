use std::{collections::HashMap, future::Future, time::Duration};

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
use rig_memory::{HeuristicTokenCounter, MemoryPolicy};
use tokio_util::sync::CancellationToken;

use crate::{
    config::RigAgentConfig,
    contract::{
        Error, Event, Message as AgentMessage, MessageDelta, MessageRole, ProviderEvent,
        ProviderRequestSent, ProviderRequestUsage, ToolCallId, ToolCallResult,
    },
    prompt::{system_prompt, SessionEnvironment},
    tools::{definitions, Definition},
};

use super::{
    mapping::{
        horizon_provider_events_from_rig_message, rig_multi_snapshot_calls,
        rig_tool_call_provider_payload, rig_tool_call_request,
    },
    memory::{ToolResultPruningMemory, TOOL_RESULT_HISTORY_PRUNING_ENABLED},
    rig_workspace_snapshot_call, StreamDeltaBuffer, StreamDeltaKind, ToolCallProgressBuffer,
};

/// Bounds the HTTP/request setup phase before rig yields a response stream.
///
/// Provider requests are deliberately not retried here: once a request has
/// crossed the network boundary Horizon cannot know whether retrying would
/// duplicate generation, billing, or tool-call intent.
const PROVIDER_STREAM_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum silence between response-stream chunks, including the wait for
/// the first chunk after the HTTP response stream has been established.
const PROVIDER_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ProviderWait<T> {
    Ready(T),
    Cancelled,
}

/// Waits for one provider phase while keeping both cancellation and a
/// wall-clock bound active. Keeping this generic lets establishment and each
/// streamed chunk share exactly the same stop semantics.
pub(super) async fn await_provider_phase<T>(
    future: impl Future<Output = T>,
    token: &CancellationToken,
    timeout: Duration,
    phase: &'static str,
) -> anyhow::Result<ProviderWait<T>> {
    tokio::select! {
        _ = token.cancelled() => Ok(ProviderWait::Cancelled),
        result = tokio::time::timeout(timeout, future) => {
            result
                .map(ProviderWait::Ready)
                .map_err(|_| anyhow::anyhow!(
                    "provider {phase} timed out after {timeout:?}"
                ))
        }
    }
}

/// Guarantees a matching `ProviderRequestFinished` marker for every path
/// after `ProviderRequestSent`, including stream setup errors, idle
/// timeouts, cancellation, and task unwinding. `finish` preserves the normal
/// event ordering by closing the span before transcript events are emitted.
pub(super) struct ProviderRequestSpan {
    events_tx: Option<Sender<ProviderEvent>>,
}

impl ProviderRequestSpan {
    pub(super) fn new(events_tx: Sender<ProviderEvent>) -> Self {
        Self {
            events_tx: Some(events_tx),
        }
    }

    fn finish(&mut self) {
        if let Some(events_tx) = self.events_tx.take() {
            let _ = events_tx.send(Event::ProviderRequestFinished.into());
        }
    }
}

impl Drop for ProviderRequestSpan {
    fn drop(&mut self) {
        self.finish();
    }
}

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
    /// The provider request itself failed (e.g. the OpenAI completion call
    /// returned an error) rather than the turn completing or being
    /// cancelled — a third, distinct stop reason `apply_turn_outcome` (in
    /// `session.rs`) maps to `Event::TurnEnded(TurnEndReason::Failed)`. An
    /// `Error` event has already been sent by the time this is set (see the
    /// `Err` branch below); this field only exists so the caller can tell
    /// "failed" apart from "completed with nothing to do", which otherwise
    /// look identical (empty tool calls, not cancelled).
    pub(super) failed: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn complete_rig_turn(
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    extra_sections: &[String],
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
            extra_sections,
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
                return TurnCompletion {
                    failed: true,
                    ..TurnCompletion::default()
                };
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
        failed: false,
    }
}

async fn rig_openai_turn_streaming(
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    extra_sections: &[String],
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
    let mut request_span = ProviderRequestSpan::new(events_tx.clone());
    let history = history_for_provider_request(config, history);
    let stream_request = model
        .completion_request(prompt)
        .messages(history)
        .tools(rig_tool_definitions(config.allowed_tool_ids.as_deref()))
        .preamble(system_prompt(environment, extra_sections))
        .additional_params(openai_turn_additional_params())
        .stream();
    let mut stream = match await_provider_phase(
        stream_request,
        token,
        PROVIDER_STREAM_ESTABLISH_TIMEOUT,
        "stream establishment",
    )
    .await?
    {
        ProviderWait::Ready(result) => result?,
        ProviderWait::Cancelled => {
            return Ok((
                partial_assistant_message(None, "", Vec::new()),
                TurnCompletion {
                    cancelled: true,
                    ..TurnCompletion::default()
                },
            ));
        }
    };

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
        let chunk = match await_provider_phase(
            stream.next(),
            token,
            PROVIDER_STREAM_IDLE_TIMEOUT,
            "response stream",
        )
        .await?
        {
            ProviderWait::Cancelled => {
                cancelled = true;
                break;
            }
            ProviderWait::Ready(chunk) => chunk,
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
                        args: request.input.0.clone(),
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
            StreamedAssistantContent::Final(response) => {
                let _ = events_tx
                    .send(provider_request_usage_event_from_openai_final(&response).into());
            }
        }
    }

    // The provider's response stream is done, either exhausted normally or
    // cut short by cancellation — either way, the request's wall-clock span
    // ends here, before the resulting message/tool-call events below.
    request_span.finish();

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
            failed: false,
        },
    ))
}

pub(super) fn provider_request_usage_event_from_openai_final(
    response: &openai::completion::streaming::StreamingCompletionResponse,
) -> Event {
    let usage = &response.usage;
    let input_tokens = saturating_u64(usage.prompt_tokens);
    let total_tokens = saturating_u64(usage.total_tokens);
    Event::ProviderRequestUsage(ProviderRequestUsage {
        input_tokens,
        output_tokens: total_tokens.saturating_sub(input_tokens),
        total_tokens,
        cached_input_tokens: usage
            .prompt_tokens_details
            .as_ref()
            .map(|details| saturating_u64(details.cached_tokens))
            .unwrap_or_default(),
    })
}

fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// OpenAI defaults this to true, but Horizon also supports configurable
/// OpenAI-compatible endpoints. Sending the flag explicitly makes the
/// intended contract stable across those backends: one assistant response
/// may request several independent tools, while `session::fold_batched_tool_result`
/// still waits for every result before the next completion.
pub(super) fn openai_turn_additional_params() -> serde_json::Value {
    serde_json::json!({ "parallel_tool_calls": true })
}

/// Builds the retained tool-result-aware memory policy. The policy is not
/// currently applied to production requests; see
/// [`history_for_provider_request`] and
/// [`TOOL_RESULT_HISTORY_PRUNING_ENABLED`].
///
/// [`ToolResultPruningMemory`] (axis B,
/// `docs/research/agent-context-memory-separation-2026-07-20.md`'s
/// "Decision (2026-07-20)"), which prefers to shrink old tool-result
/// *content* to a short placeholder before ever dropping a whole message,
/// so the task instruction (a plain `UserContent::Text`, never touched by
/// that step) survives as a byproduct. Replaces the stock `rig_memory::
/// TokenWindowMemory` this used to return, which applied a pure recency
/// cutoff with no distinction between tool output and everything else.
///
/// Uses `rig-memory`'s OpenAI [`HeuristicTokenCounter`] preset -- a
/// provider-agnostic, byte-length heuristic, not the real tokenizer of
/// whatever model `config.model` names -- which is why
/// `config.history_token_budget` (axis A: model-derived when resolvable,
/// see `model_catalog::apply_model_derived_history_budget`, else
/// `config::DEFAULT_HISTORY_TOKEN_BUDGET`) already reserves a safety margin
/// against that approximation's own documented ~30% error rather than
/// tracking a specific context window exactly.
pub(super) fn history_token_window_policy(config: &RigAgentConfig) -> ToolResultPruningMemory {
    ToolResultPruningMemory::new(
        config.history_token_budget,
        config.protected_recent_tool_result_tokens,
        HeuristicTokenCounter::openai(),
    )
}

/// Returns the history view sent to the provider.
///
/// Owner decision 2026-07-25 disables every form of tool-result pruning
/// while Horizon first addresses why ordinary tasks need so many sequential
/// provider/tool rounds. With the switch off this is an exact pass-through:
/// no duplicate-read elision, proactive soft pruning, over-budget
/// tool-result replacement, or oldest-turn dropping. A real provider context
/// limit is therefore surfaced by the provider instead of Horizon silently
/// discarding information.
///
/// The disabled branch remains here, rather than deleting the policy, so the
/// implementation and its direct tests stay available for an explicit future
/// product decision.
pub(super) fn history_for_provider_request(
    config: &RigAgentConfig,
    history: Vec<Message>,
) -> Vec<Message> {
    if !TOOL_RESULT_HISTORY_PRUNING_ENABLED {
        return history;
    }
    let policy = history_token_window_policy(config);
    windowed_history_for_request(history, &policy)
}

/// Applies `policy` to `history` -- the *view* of the conversation sent to
/// the provider for this turn. This never touches `rig_history` itself
/// (the session loop's source of truth, appended to and persisted via the
/// DuckDB projection unchanged by the caller in `complete_rig_turn`): only
/// the clone handed to this function is ever windowed.
///
/// [`ToolResultPruningMemory::apply`] cannot currently fail (its
/// `MemoryPolicy` impl only ever returns `Ok`), but [`MemoryPolicy::apply`]
/// is fallible by contract, so a future policy change (or a different
/// policy swapped in here) could start returning `Err`. On `Err` the
/// original, unwindowed history is used instead and the failure is logged
/// via `tracing` -- never silently dropping context (an empty history) or
/// failing the turn outright over a policy bug.
pub(super) fn windowed_history_for_request(
    history: Vec<Message>,
    policy: &dyn MemoryPolicy,
) -> Vec<Message> {
    let fallback = history.clone();
    match policy.apply(history) {
        Ok(windowed) => windowed,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "history token-window policy failed; sending unwindowed history"
            );
            fallback
        }
    }
}

/// Builds the OpenAI Completions client for a turn.
///
/// The API key is always read straight from `OPENAI_API_KEY` — secrets
/// never flow through the config file (`agent::config`'s module doc) — so
/// this can't just call `openai::CompletionsClient::from_env()` the way it
/// used to: that helper also reads `OPENAI_BASE_URL` itself, which would
/// silently ignore Horizon's own precedence for the base URL. Instead the
/// base URL comes from `config.base_url`, already resolved by
/// `agent::config::RigAgentConfig::from_env_and_provider` with the right
/// precedence (env `OPENAI_BASE_URL` > `[provider].base_url` in the config
/// file); `None`
/// leaves rig's own default (`https://api.openai.com/v1`) in place by
/// simply not calling `.base_url(..)` on the builder, mirroring exactly
/// what `from_env()` did before.
fn openai_completions_client(config: &RigAgentConfig) -> anyhow::Result<openai::CompletionsClient> {
    let api_key = std::env::var(crate::config::OPENAI_API_KEY_VAR)
        .map_err(|_| anyhow::anyhow!("{} is not set", crate::config::OPENAI_API_KEY_VAR))?;

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
    let lower = text.to_ascii_lowercase();
    if lower.contains("multi tool") {
        // Deterministic hook for exercising a parallel-tool-call batch (see
        // `rig_multi_snapshot_calls`'s doc comment) without a network
        // provider.
        multi_tool_call_message(MULTI_TOOL_TEST_BATCH_SIZE)
    } else if lower.contains("snapshot") {
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

/// How many tool calls `deterministic_rig_response`'s "multi tool" trigger
/// and `deterministic_tool_result_response`'s `loop_again_batch` hook
/// request — arbitrary but fixed, so tests can assert an exact count.
pub(super) const MULTI_TOOL_TEST_BATCH_SIZE: usize = 4;

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
    // Same idea, but for a parallel batch: requests another
    // `loop_again_batch`-many tool calls at once, so tests can drive
    // consecutive tool-*batch* turns (e.g. asserting the iteration-cap guard
    // counts one turn per batch, not one per result).
    if let Some(count) = result
        .output
        .get("loop_again_batch")
        .and_then(serde_json::Value::as_u64)
    {
        return multi_tool_call_message(count as usize);
    }
    Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text::new(format!(
            "Tool result received for {}.",
            result.call_id.0
        )))),
    }
}

fn multi_tool_call_message(count: usize) -> Message {
    Message::Assistant {
        id: None,
        content: OneOrMany::many(
            rig_multi_snapshot_calls(count)
                .into_iter()
                .map(AssistantContent::ToolCall)
                .collect::<Vec<_>>(),
        )
        .expect("multi_tool_call_message is only ever called with count >= 1"),
    }
}

/// Converts the catalog's tool definitions to rig's `ToolDefinition` shape,
/// optionally restricted to `allowed_tool_ids` (`RigAgentConfig::
/// allowed_tool_ids` — see that field's doc comment). `None` is the current,
/// unrestricted behavior: every tool in `tools::definitions()` is advertised
/// to the provider, unchanged from before this parameter existed.
pub(super) fn rig_tool_definitions(allowed_tool_ids: Option<&[String]>) -> Vec<ToolDefinition> {
    definitions()
        .into_iter()
        .filter(|definition| match allowed_tool_ids {
            Some(allowed) => allowed.iter().any(|id| id == &definition.id),
            None => true,
        })
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
                    args: request.input.0.clone(),
                },
            )),
            _ => None,
        })
        .collect()
}
