mod response;

use std::{collections::HashMap, future::Future, time::Duration};

use crossbeam_channel::Sender;
use futures_util::StreamExt;
use rig_core::client::CompletionClient;
use rig_core::{
    completion::{
        message::{Text, ToolCall},
        AssistantContent, CompletionModel, Message, ToolDefinition,
    },
    providers::{anthropic, openai},
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::{ProviderKind, RigAgentConfig},
    contract::{
        Error, Event, MessageRole, ProviderEvent, ProviderRateLimited, ProviderRequestSent,
        ProviderRequestUsage, ToolCallId, ToolCallResult,
    },
    prompt::{system_prompt, SessionEnvironment},
    tools::{definitions, Definition},
};

use super::{
    clearing::{history_for_provider_request, ClearingState},
    mapping::{
        horizon_provider_events_from_rig_message, rig_fs_read_call, rig_multi_snapshot_calls,
    },
    retry::{with_pre_generation_retry, Attempt, Retried},
    rig_workspace_snapshot_call,
};

/// Bounds the HTTP/request setup phase before rig yields a response stream.
///
/// A request that has crossed the network boundary is deliberately not
/// retried on timeout: Horizon cannot know whether retrying would duplicate
/// generation, billing, or tool-call intent. That caveat is about failures
/// that may land *after* generation started; a rejection received before any
/// of the response has been decoded is a different case and is retried --
/// see [`super::retry::retryable_rejection`].
const PROVIDER_STREAM_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum silence between response-stream chunks, including the wait for
/// the first chunk after the HTTP response stream has been established.
///
/// A trip of this bound stays fatal, and the asymmetry with the retried
/// rejections below is deliberate: silence proves nothing about whether the
/// provider is already generating, so a retry could duplicate a generation
/// that is merely slow to reach us.
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
#[derive(Clone, Debug)]
pub(super) struct ToolCallDescriptor {
    pub(super) identity: crate::contract::ToolCallIdentity,
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
    pub(super) final_text: Option<String>,
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
    /// Input tokens the provider reported for this turn's request, when it
    /// reported usage at all. Fed to `ClearingState::record_input_tokens` so
    /// Tier 1's trigger runs off the provider's own measurement rather than
    /// a byte heuristic (`docs/agent-compaction-design.md`; crush and
    /// opencode both drive the same decision off actual usage tokens).
    pub(super) input_tokens: Option<u64>,
    /// The provider started streaming one or more tool calls but never
    /// finalized them — the response was truncated mid-stream (rig's
    /// `take_finalized_tool_calls` dropped the incomplete calls with only
    /// a `tracing::debug`). Distinct from `failed` (the request itself
    /// errored) and from an empty `requested_tool_call_ids` (which could
    /// be a normal text-only reply): a truncated turn must not be misread
    /// as `Completed`.
    pub(super) truncated: bool,
    /// How many tool calls the provider started but never finalized, when
    /// `truncated` is true. Zero otherwise.
    pub(super) truncated_tool_call_count: usize,
    /// Output tokens the provider reported for this turn's request, when it
    /// reported usage at all. `None` when no `Final` chunk arrived (the
    /// stream ended without usage — see [`output_cap_truncated`]'s doc
    /// comment for why that matters).
    pub(super) output_tokens: Option<u64>,
    /// The provider generated exactly `max_output_tokens` of output — the
    /// response was almost certainly truncated at the output ceiling (see
    /// [`output_cap_truncated`]). Distinct from `truncated` (tool calls cut
    /// mid-stream): a cap-truncated turn may have no started tool calls at
    /// all (e.g. reasoning consumed the entire budget). Like `truncated`,
    /// this is suppressed for a cancelled turn.
    pub(super) cap_truncated: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn complete_rig_turn(
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    extra_sections: &[String],
    rig_history: &mut Vec<Message>,
    prompt: Message,
    events_tx: &Sender<ProviderEvent>,
    clearing: &mut ClearingState,
    memory: Option<&crate::tools::MemoryDocument>,
    moa: Option<&(usize, Message)>,
    fallback: impl FnOnce() -> Message,
    token: &CancellationToken,
) -> TurnCompletion {
    // Tier 1's one execution point: between provider rounds, right before a
    // request is built (`docs/agent-compaction-design.md`). Being here is
    // what gives the design's turn-semantics requirements for free -- a
    // session parked in `WaitingForApproval` is not building a request, and
    // `Event::TurnEnded` is emitted by the session loop, untouched by
    // anything below. Ahead of the provider branch rather than inside it:
    // the deterministic fallback responder stands in for a provider request
    // too, and a state that never sees reported usage can never cross the
    // threshold anyway (`ClearingState::over_threshold`).
    if let Some(cleared) = clearing.run_pass(rig_history) {
        let _ = events_tx.send(Event::HistoryCleared(cleared).into());
    }
    if config.api_key_present {
        match rig_provider_turn_with_retry(
            config,
            environment,
            extra_sections,
            &prompt,
            history_for_provider_request(rig_history, clearing.cleared(), memory, moa),
            events_tx,
            token,
        )
        .await
        {
            Ok((assistant_message, completion)) => {
                if let Some(input_tokens) = completion.input_tokens {
                    clearing.record_input_tokens(input_tokens);
                }
                rig_history.push(prompt);
                rig_history.push(assistant_message);
                return completion;
            }
            Err(error) => {
                let _ = events_tx.send(
                    Event::Error(Error {
                        message: format!("Rig completion failed: {error}"),
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
    let final_text = events
        .iter()
        .filter_map(|event| match &event.event {
            Event::MessageCommitted(message) if message.role == MessageRole::Assistant => {
                Some(message.text.clone())
            }
            _ => None,
        })
        .next_back();
    for event in events {
        let _ = events_tx.send(event);
    }
    TurnCompletion {
        final_text,
        requested_tool_call_ids,
        requested_tool_calls,
        cancelled: false,
        failed: false,
        input_tokens: None,
        output_tokens: None,
        truncated: false,
        truncated_tool_call_count: 0,
        cap_truncated: false,
    }
}

/// Runs one turn's provider request under the pre-generation retry policy.
///
/// Each attempt is a genuinely new request and emits its own
/// `ProviderRequestSent`/`ProviderRequestFinished` pair, which is what a
/// turn with several provider rounds already looks like in the log. Nothing
/// is retried once a chunk has been decoded, so no attempt can have emitted
/// transcript content before the next one starts.
async fn rig_provider_turn_with_retry(
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    extra_sections: &[String],
    prompt: &Message,
    history: Vec<Message>,
    events_tx: &Sender<ProviderEvent>,
    token: &CancellationToken,
) -> anyhow::Result<(Message, TurnCompletion)> {
    let outcome = with_pre_generation_retry(
        token,
        || async {
            let mut durable_output_emitted = false;
            // One client per attempt, exactly like the single-kind
            // predecessor: the request is new even when rig's HTTP client
            // could be reused, and the entry's API-key variable is re-read
            // per attempt (never stored).
            let result = match completion_client(config) {
                Ok(client) => {
                    rig_provider_turn_streaming(
                        config,
                        client,
                        environment,
                        extra_sections,
                        prompt.clone(),
                        history.clone(),
                        events_tx.clone(),
                        token,
                        &mut durable_output_emitted,
                    )
                    .await
                }
                Err(error) => Err(error),
            };
            Attempt {
                result,
                durable_output_emitted,
            }
        },
        |number, rejection, backoff| {
            let _ = events_tx.send(
                Event::ProviderRateLimited(ProviderRateLimited {
                    status: rejection.status,
                    attempt: number,
                    backoff_ms: backoff.as_millis() as u64,
                })
                .into(),
            );
        },
    )
    .await;

    match outcome {
        Retried::Ok(turn) => Ok(turn),
        // Same shape the establishment-phase cancel returns: a cancel is a
        // stop reason, not an error.
        Retried::Cancelled => Ok((
            partial_assistant_message(None, "", Vec::new()),
            TurnCompletion {
                cancelled: true,
                ..TurnCompletion::default()
            },
        )),
        Retried::Failed(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn rig_provider_turn_streaming(
    config: &RigAgentConfig,
    client: RigCompletionClient,
    environment: &SessionEnvironment,
    extra_sections: &[String],
    prompt: Message,
    history: Vec<Message>,
    events_tx: Sender<ProviderEvent>,
    token: &CancellationToken,
    durable_output_emitted: &mut bool,
) -> anyhow::Result<(Message, TurnCompletion)> {
    match client {
        RigCompletionClient::OpenAi(client) => {
            run_provider_stream(
                config,
                client,
                environment,
                extra_sections,
                prompt,
                history,
                events_tx,
                token,
                durable_output_emitted,
            )
            .await
        }
        RigCompletionClient::Anthropic(client) => {
            run_provider_stream(
                config,
                client,
                environment,
                extra_sections,
                prompt,
                history,
                events_tx,
                token,
                durable_output_emitted,
            )
            .await
        }
    }
}

/// One streamed provider turn, generic over whichever bundled rig client
/// the turn's kind built — every type below the client construction
/// (`CompletionModel`, the request builder, `StreamingCompletionResponse`,
/// `StreamedAssistantContent`) is rig-common, so both kinds share this
/// body unchanged.
#[allow(clippy::too_many_arguments)]
// `Clone` is the `completion_request` builder's own bound; both bundled
// clients' models (`openai`/`anthropic` `GenericCompletionModel`) satisfy it.
async fn run_provider_stream<C>(
    config: &RigAgentConfig,
    client: C,
    environment: &SessionEnvironment,
    extra_sections: &[String],
    prompt: Message,
    history: Vec<Message>,
    events_tx: Sender<ProviderEvent>,
    token: &CancellationToken,
    durable_output_emitted: &mut bool,
) -> anyhow::Result<(Message, TurnCompletion)>
where
    C: CompletionClient,
    // `Clone` is the `completion_request` builder's own bound; both bundled
    // clients' models (`openai`/`anthropic` `GenericCompletionModel`)
    // satisfy it.
    C::CompletionModel: Clone,
{
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
    // `history` is the *provider view* of canonical history, already
    // projected through the Tier 1 clearing seam by the caller
    // (`super::clearing::history_for_provider_request`). Nothing is dropped:
    // the projection only replaces the bodies of tool results the session's
    // frozen cleared set names, keeping every call/result pair intact, and
    // `rig_history` itself still holds the originals.
    let stream_request = model
        .completion_request(prompt)
        .messages(history)
        .tools(rig_tool_definitions(
            config.allowed_tool_ids.as_deref(),
            config.trusted_project,
        ))
        .preamble(system_prompt(environment, extra_sections))
        .max_tokens(config.max_output_tokens)
        .additional_params(provider_additional_params(config.kind))
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

    let mut response = response::ResponseCollector::new(config, events_tx, durable_output_emitted);
    let cancelled = loop {
        let chunk = match await_provider_phase(
            stream.next(),
            token,
            PROVIDER_STREAM_IDLE_TIMEOUT,
            "response stream",
        )
        .await?
        {
            ProviderWait::Cancelled => break true,
            ProviderWait::Ready(None) => break false,
            ProviderWait::Ready(Some(chunk)) => chunk?,
        };
        // Decode before marking first-token or durable output. OpenAI may
        // deliver a rejected HTTP request on the stream's first poll.
        response.push(chunk);
    };

    // End the request span before final deltas and the committed message.
    // Errors instead drop both the span and the uncommitted response.
    request_span.finish();
    Ok(response.finish(cancelled, stream.message_id.clone(), stream.choice.clone()))
}

pub(super) fn provider_request_usage_event_from_stream_final(
    final_record: &rig_core::streaming::StreamFinal,
) -> Event {
    // rig's normalized `Usage` already folds the provider's per-field
    // reporting (OpenAI's `prompt_tokens` and
    // `prompt_tokens_details.cached_tokens` included) into plain counters.
    let usage = &final_record.usage;
    let input_tokens = usage.input_tokens;
    let total_tokens = usage.total_tokens;
    Event::ProviderRequestUsage(ProviderRequestUsage {
        input_tokens,
        output_tokens: total_tokens.saturating_sub(input_tokens),
        total_tokens,
        cached_input_tokens: usage.cached_input_tokens,
    })
}

/// Detects output-cap truncation by comparing the provider-reported output
/// token count against the configured ceiling (`config.max_output_tokens`).
///
/// rig 0.42's streaming terminal element (`StreamFinal`) does carry a
/// normalized `finish_reason`, but adopting it here is a behavior change this
/// migration deliberately defers: the token-count heuristic below was
/// validated against live traffic, and switching the detector to
/// `finish_reason` deserves its own measured change. So truncation stays
/// *inferred* from the token count.
///
/// The heuristic is `output_tokens == Some(cap)`: the turn produced exactly
/// as many tokens as the ceiling allowed. Measurement backs this up — across
/// ~6,700 provider requests the cap-exact count appeared only twice (both
/// genuine truncations) and the 28,000–32,767 band was empty (next-highest
/// was 25,674), so a false positive is very unlikely.
///
/// Two holes remain, both inherent to the approach:
///
/// 1. **No usage event may arrive.** When the stream ends on an error or
///    cancellation before the `Final` chunk, `FinalResponse` is never
///    delivered and `output_tokens` stays `None` — this detector cannot
///    fire (syn:large:text saw this in 2.3% = 60/2,591 of requests).
///
/// 2. **Zero-usage responses are indistinguishable from small turns.** If
///    the provider omits usage entirely, rig substitutes `Usage::default()`,
///    yielding `output_tokens: 0`, which this detector correctly treats as
///    "not truncated" — but a genuinely truncated turn that also reported
///    zero usage would be missed the same way.
pub(super) fn output_cap_truncated(output_tokens: Option<u64>, cap: u64, cancelled: bool) -> bool {
    !cancelled && output_tokens == Some(cap)
}

/// OpenAI defaults this to true, but Horizon also supports configurable
/// OpenAI-compatible endpoints. Sending the flag explicitly makes the
/// intended contract stable across those backends: one assistant response
/// may request several independent tools, while `session::fold_batched_tool_result`
/// still waits for every result before the next completion.
pub(super) fn provider_additional_params(kind: ProviderKind) -> serde_json::Value {
    match kind {
        // OpenAI-compatible: one assistant response may request several
        // independent tools, while `session::fold_batched_tool_result` still
        // waits for every result before the next completion. Sending the
        // flag explicitly makes the intended contract stable across
        // configurable backends.
        ProviderKind::OpenAiCompatible => serde_json::json!({ "parallel_tool_calls": true }),
        // Anthropic: nothing openai-specific to send.
        ProviderKind::Anthropic => serde_json::Value::Object(serde_json::Map::new()),
    }
}

/// Either bundled rig client a turn's kind can build.
///
/// rig-core 0.42 bundles both with no feature gates; the enum (rather than
/// a trait object) is what lets `run_provider_stream` stay generic over the
/// concrete `CompletionClient` impl without dyn-safe contortions.
#[derive(Debug)]
pub(super) enum RigCompletionClient {
    OpenAi(openai::CompletionsClient),
    Anthropic(anthropic::Client),
}

/// Builds the rig completion client for a turn — generalized from
/// `openai_completions_client` (the single-kind predecessor) by `[[providers]]`:
/// `kind` dispatches between rig's bundled clients, constructed the same
/// way either kind.
///
/// The API key is always read straight from the environment variable
/// **named** by `config.api_key_env` — secrets never flow through the
/// config file (`agent::config`'s module doc) — so this can't just call
/// `from_env()`: rig's own env helpers also read their base-URL variables,
/// which would silently ignore Horizon's own precedence. The base URL
/// comes from `config.base_url`, already resolved with the right
/// precedence (the kind's base-URL env var > the entry's `base_url`);
/// `None` leaves rig's own default (`https://api.openai.com/v1` /
/// `https://api.anthropic.com`) in place by simply not calling
/// `.base_url(..)` on the builder. The variable is re-read per turn —
/// `config.api_key_env` is a name, and a key appearing (or disappearing)
/// in the environment mid-session is honored at the next turn boundary.
fn completion_client(config: &RigAgentConfig) -> anyhow::Result<RigCompletionClient> {
    let api_key = std::env::var(&config.api_key_env)
        .map_err(|_| anyhow::anyhow!("{} is not set", config.api_key_env))?;

    match config.kind {
        ProviderKind::OpenAiCompatible => {
            let mut builder = openai::CompletionsClient::builder().api_key(&api_key);
            if let Some(base_url) = &config.base_url {
                builder = builder.base_url(base_url);
            }
            Ok(RigCompletionClient::OpenAi(builder.build()?))
        }
        ProviderKind::Anthropic => {
            let mut builder = anthropic::Client::builder().api_key(&api_key);
            if let Some(base_url) = &config.base_url {
                builder = builder.base_url(base_url);
            }
            Ok(RigCompletionClient::Anthropic(builder.build()?))
        }
    }
}

/// Decodes a double-encoded tool-call `arguments` value in place: a JSON
/// *string* whose content is itself a JSON object becomes that object.
///
/// Observed 2026-07-27 from `MiniMaxAI/MiniMax-M3` (session `12fd8d14`),
/// which emitted a streamed tool call with `arguments` as a string holding
/// the object. Decoding it lets the call execute exactly as intended
/// instead of failing input validation for something the model did not
/// really get wrong.
///
/// Anything else is left untouched: a string that does not decode to an
/// object is a genuinely malformed emission, and the tool's own validation
/// error is the feedback the model needs.
pub(super) fn repair_double_encoded_tool_arguments(arguments: &mut serde_json::Value) {
    let serde_json::Value::String(encoded) = &*arguments else {
        return;
    };
    if let Ok(decoded @ serde_json::Value::Object(_)) =
        serde_json::from_str::<serde_json::Value>(encoded)
    {
        *arguments = decoded;
    }
}

/// Forces a tool call's arguments into a JSON object for *history*: the
/// decoding repair above first, then a bare `{}` for whatever is still not
/// an object.
///
/// The `{}` substitution trades history fidelity for session survival, and
/// the trade is deliberate. Serving layers render replayed tool calls
/// through a chat template that iterates `arguments` as a mapping —
/// MiniMax-M3's has no string branch at all — so one malformed emission
/// stored verbatim makes *every* later request in that session fail with a
/// provider 400 (`'str object' has no attribute 'items'`). That is
/// unrecoverable, because the poison sits in the persisted history: it is
/// how session `12fd8d14` died on 2026-07-27. Only the provider-facing
/// replay is rewritten — the model already learned that its input was
/// malformed from the tool's error result.
pub(super) fn replay_safe_tool_arguments(arguments: &mut serde_json::Value) {
    repair_double_encoded_tool_arguments(arguments);
    if !arguments.is_object() {
        *arguments = serde_json::Value::Object(serde_json::Map::new());
    }
}

/// Applies [`replay_safe_tool_arguments`] to every tool call in an
/// assistant history message.
pub(super) fn make_tool_call_arguments_replay_safe(content: &mut [AssistantContent]) {
    for item in content.iter_mut() {
        if let AssistantContent::ToolCall(call) = item {
            replay_safe_tool_arguments(&mut call.function.arguments);
        }
    }
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

    if content.is_empty() {
        content.push(AssistantContent::Text(Text::new(String::new())));
    }
    make_tool_call_arguments_replay_safe(&mut content);

    Message::Assistant {
        id: message_id,
        content,
    }
}

/// Line prefix the deterministic fallback reads an `fs.read` target from;
/// the rest of the line is the path, verbatim (case-sensitive, unlike the
/// keyword triggers below).
pub(super) const READ_PATH_TRIGGER: &str = "fs.read path: ";

pub(super) fn deterministic_rig_response(text: &str) -> Message {
    let lower = text.to_ascii_lowercase();
    if let Some(path) = text
        .lines()
        .find_map(|line| line.trim().strip_prefix(READ_PATH_TRIGGER))
    {
        // Deterministic hook for driving a real `fs.read` -- including one
        // whose path leaves the workspace root, so the approval/refusal
        // routing is exercisable without a network provider.
        return Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(rig_fs_read_call(path.trim()))],
        };
    }
    if lower.contains("multi tool") {
        // Deterministic hook for exercising a parallel-tool-call batch (see
        // `rig_multi_snapshot_calls`'s doc comment) without a network
        // provider.
        multi_tool_call_message(MULTI_TOOL_TEST_BATCH_SIZE)
    } else if lower.contains("snapshot") {
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(rig_workspace_snapshot_call())],
        }
    } else {
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::Text(Text::new(format!(
                "rig-core fallback response: {text}"
            )))],
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
            content: vec![AssistantContent::ToolCall(rig_workspace_snapshot_call())],
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
        content: vec![AssistantContent::Text(Text::new(format!(
            "Tool result received for {}.",
            result.call_id.0
        )))],
    }
}

fn multi_tool_call_message(count: usize) -> Message {
    Message::Assistant {
        id: None,
        content: rig_multi_snapshot_calls(count)
            .into_iter()
            .map(AssistantContent::ToolCall)
            .collect(),
    }
}

/// Converts the catalog's tool definitions to rig's `ToolDefinition` shape,
/// optionally restricted to `allowed_tool_ids` (`RigAgentConfig::
/// allowed_tool_ids` — see that field's doc comment). `None` is the current,
/// unrestricted behavior: every tool in `tools::definitions()` is advertised
/// to the provider, unchanged from before this parameter existed.
///
/// Three tools are filtered beyond the allowlist. `task_output` is advertised
/// only when `task` itself is (`docs/agent-async-task-design.md` decision
/// 3, "advertise it only alongside `task`"). It is the same conditional
/// seam `prompt::DELEGATION_ROUTING_SECTION` rides on
/// (`session::advertises_task_tool`), decided from the same allowlist —
/// a session that cannot launch a task can never own one to read, so
/// offering the fetch tool would only be a call it must fail. `web_search`
/// is advertised only when `EXA_API_KEY` is set in the process environment:
/// without the key the Exa adapter can only return a "not configured" error,
/// so advertising it buys a round that cannot succeed (`web_fetch` needs no
/// key and stays advertised). `knowledge.read`/`knowledge.write` are
/// advertised only when `trusted_project` is true — an untrusted session
/// gets no project-knowledge index in its prompt and no way to call
/// through to the user-side store (see `knowledge`'s module doc).
pub(super) fn rig_tool_definitions(
    allowed_tool_ids: Option<&[String]>,
    trusted_project: bool,
) -> Vec<ToolDefinition> {
    let allows = |id: &str| match allowed_tool_ids {
        Some(allowed) => allowed.iter().any(|allowed| allowed == id),
        None => true,
    };
    let advertises_task = allows(crate::tools::TASK_TOOL_ID);
    let exa_configured = std::env::var(crate::config::EXA_API_KEY_VAR)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    definitions()
        .into_iter()
        .filter(|definition| {
            allows(&definition.id)
                && (advertises_task || definition.id != crate::tools::TASK_OUTPUT_TOOL_ID)
                && (exa_configured || definition.id != "web_search")
                && (trusted_project || !is_knowledge_tool(&definition.id))
                // `memory.update` is a standing-role-only tool: a role-less
                // session (`allowed_tool_ids == None`, i.e. "all tools") must
                // never see it, and non-standing roles don't list it in their
                // allowlist so `allows` already excludes them. This filter
                // closes the `None` gap.
                && (allowed_tool_ids.is_some() || definition.id != "memory.update")
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

/// Whether `tool_id` is one of the two knowledge tools that are
/// withheld from untrusted sessions — see `rig_tool_definitions`'s
/// filter.
fn is_knowledge_tool(tool_id: &str) -> bool {
    tool_id == "knowledge.read" || tool_id == "knowledge.write"
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
                    identity: request.identity(),
                    tool_id: request.tool_id.clone(),
                    args: request.input.0.clone(),
                },
            )),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
