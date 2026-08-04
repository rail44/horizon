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
    clearing::{history_for_provider_request, ClearingState},
    mapping::{
        horizon_provider_events_from_rig_message, rig_multi_snapshot_calls,
        rig_tool_call_provider_payload, rig_tool_call_request,
    },
    rig_workspace_snapshot_call, StreamDeltaBuffer, StreamDeltaKind, ToolCallProgressBuffer,
};

/// Bounds the HTTP/request setup phase before rig yields a response stream.
///
/// A request that has crossed the network boundary is deliberately not
/// retried on timeout: Horizon cannot know whether retrying would duplicate
/// generation, billing, or tool-call intent. That caveat is about failures
/// that may land *after* generation started; a rejection received before any
/// of the response has been decoded is a different case and is retried --
/// see [`retryable_rejection`].
const PROVIDER_STREAM_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum silence between response-stream chunks, including the wait for
/// the first chunk after the HTTP response stream has been established.
///
/// A trip of this bound stays fatal, and the asymmetry with the retried
/// rejections below is deliberate: silence proves nothing about whether the
/// provider is already generating, so a retry could duplicate a generation
/// that is merely slow to reach us.
const PROVIDER_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How many times one provider request may be sent when the provider keeps
/// rejecting it before generating anything: the first send plus two retries.
pub(super) const PROVIDER_REQUEST_MAX_ATTEMPTS: u32 = 3;

/// First backoff window; it doubles per attempt (~1s, ~2s, ~4s).
const PROVIDER_RETRY_BASE_BACKOFF: Duration = Duration::from_secs(1);

/// Ceiling on any single backoff, including one the provider asked for.
pub(super) const PROVIDER_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Statuses with which a provider says "not now" rather than "not ever":
/// rate limiting, the three gateway-level failures, and a bare 500. Every
/// other 4xx describes the request itself and would fail identically on a
/// retry.
///
/// 500 earns its place empirically: synthetic.new fronts its models with a
/// gateway that reports an upstream hiccup as `500 Internal Server Error`
/// with an `{"error":"Error from inference backend: ..."}` body, and on
/// 2026-07-30 one such incident killed two sessions within a minute of each
/// other. Including it is safe for the same reason the rest of this list is:
/// [`retryable_rejection`] only ever fires before any durable output, so the
/// request provably never reached generation and a repeat cannot duplicate
/// one. A 500 that is genuinely deterministic still terminates the turn --
/// it just costs [`PROVIDER_REQUEST_MAX_ATTEMPTS`] attempts first.
const RETRYABLE_STATUSES: [u16; 5] = [429, 500, 502, 503, 504];

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

/// A provider rejection that arrived before any of the response had been
/// decoded, so re-sending the request cannot duplicate a generation: the
/// provider answered "no" (or never answered at all) instead of starting to
/// produce tokens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TransientRejection {
    /// The status the provider answered with. `None` is the
    /// connection/handshake-level shape (rig renders it as `Http client
    /// error: error sending request for url ...`), where the request never
    /// reached the model at all.
    pub(super) status: Option<u16>,
    /// A `Retry-After` the provider named. rig surfaces no response headers,
    /// so this only fires when the provider repeats the hint in its error
    /// body; absent, the exponential window below is used instead. A 429
    /// body that names the concrete concurrency limit would be the natural
    /// input for tuning the launch ceiling in `tools::explore::children`
    /// dynamically -- deliberately not built now.
    pub(super) retry_after: Option<Duration>,
    /// Whether a transport failure (`status: None`) occurred mid-stream —
    /// the response had started but a body decode failed (rig renders it
    /// as `Http client error: error decoding response body: ...`) — rather
    /// than pre-send (the request never reached the model). Only meaningful
    /// when `status` is `None`; always `false` for status-based rejections.
    pub(super) mid_stream: bool,
}

impl TransientRejection {
    fn describe(&self) -> String {
        match self.status {
            Some(status) => format!("HTTP {status}"),
            None if self.mid_stream => "transport failure (mid-stream decode)".to_string(),
            None => "transport failure (pre-send)".to_string(),
        }
    }
}

/// One attempt's outcome as [`with_pre_generation_retry`] needs to see it.
pub(super) struct Attempt<T> {
    pub(super) result: anyhow::Result<T>,
    /// Whether this attempt produced durable output — a `ToolCallRequested`
    /// or `MessageCommitted` event — by the time it ended. Once true, no
    /// failure of this attempt may be retried: a retry could duplicate the
    /// committed tool call or message in history. Reasoning and text
    /// deltas are volatile (never entered history), so an attempt that
    /// only streamed those is still safe to retry.
    pub(super) durable_output_emitted: bool,
}

/// How a retried provider request finally ended.
pub(super) enum Retried<T> {
    Ok(T),
    /// The turn was cancelled while a backoff was being waited out. Cancel
    /// wins over a pending retry, always.
    Cancelled,
    Failed(anyhow::Error),
}

/// The retry decision, kept free of I/O so the classification is directly
/// testable: `Some` exactly when this attempt may be sent again.
///
/// Three independent gates, all of which must pass. The request must not
/// have reached generation (anything after the provider started answering
/// could be duplicated by a retry); the attempt budget must not be spent;
/// and the failure must be one of the transient shapes above rather than a
/// contract error or a stream timeout.
pub(super) fn retryable_rejection(
    attempt: u32,
    durable_output_emitted: bool,
    message: &str,
) -> Option<TransientRejection> {
    if durable_output_emitted || attempt >= PROVIDER_REQUEST_MAX_ATTEMPTS {
        return None;
    }
    if let Some(status) = rejected_status(message) {
        return RETRYABLE_STATUSES
            .contains(&status)
            .then(|| TransientRejection {
                status: Some(status),
                retry_after: named_retry_after(message),
                mid_stream: false,
            });
    }
    message
        .contains(TRANSPORT_FAILURE_MARKER)
        .then_some(TransientRejection {
            status: None,
            retry_after: None,
            mid_stream: message.contains("error decoding response body"),
        })
}

/// rig renders a non-2xx response as `Invalid status code <status> <reason>
/// with message: <body>` (`rig_core::http_client::Error`), wrapped in a
/// `ProviderError`.
fn rejected_status(message: &str) -> Option<u16> {
    const MARKER: &str = "Invalid status code ";
    let index = message.find(MARKER)?;
    message[index + MARKER.len()..]
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// rig's rendering of a connection/handshake-level failure -- the client
/// never got a status back.
const TRANSPORT_FAILURE_MARKER: &str = "Http client error:";

/// Reads a `Retry-After`-style hint out of the provider's error text, in
/// whole seconds. Only the first number following the hint within a short
/// window counts, so an unrelated number further down the body cannot be
/// mistaken for one.
fn named_retry_after(message: &str) -> Option<Duration> {
    let lowered = message.to_ascii_lowercase();
    let index = lowered
        .find("retry-after")
        .or_else(|| lowered.find("retry_after"))?;
    let window: String = lowered[index..].chars().take(64).collect();
    let seconds: String = window
        .chars()
        .skip_while(|character| !character.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    seconds.parse().ok().map(Duration::from_secs)
}

/// Exponential backoff with equal jitter: the wait is drawn from the upper
/// half of a window that doubles per attempt, so sessions rejected at the
/// same instant do not all come back at the same instant either. A
/// provider-supplied `Retry-After` replaces the exponential term -- the
/// provider knows its own window better than this does -- and both are
/// clamped to [`PROVIDER_RETRY_MAX_BACKOFF`].
pub(super) fn retry_backoff(
    attempt: u32,
    retry_after: Option<Duration>,
    jitter_permille: u32,
) -> Duration {
    let window = retry_after
        .unwrap_or_else(|| {
            PROVIDER_RETRY_BASE_BACKOFF
                .checked_mul(1u32 << attempt.saturating_sub(1).min(16))
                .unwrap_or(PROVIDER_RETRY_MAX_BACKOFF)
        })
        .min(PROVIDER_RETRY_MAX_BACKOFF);
    let half = window / 2;
    half + half * jitter_permille.min(1000) / 1000
}

/// Jitter from the wall clock's sub-second component: enough to break
/// lockstep between concurrent sessions, and not worth a new dependency
/// for -- nothing here is security-sensitive.
fn jitter_permille() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.subsec_nanos() % 1_000)
        .unwrap_or(500)
}

/// Waits out a retry backoff, returning `false` the moment the turn is
/// cancelled. `biased` makes an already-cancelled token win without even
/// arming the timer.
pub(super) async fn sleep_unless_cancelled(delay: Duration, token: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        _ = token.cancelled() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

/// Sends one provider request, re-sending it while the provider keeps
/// rejecting it before producing any durable output.
///
/// Generic over the attempt so the loop itself is testable without a
/// provider: the real caller passes a closure that runs one
/// `rig_openai_turn_streaming`.
pub(super) async fn with_pre_generation_retry<T, F, Fut>(
    token: &CancellationToken,
    mut attempt: F,
) -> Retried<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Attempt<T>>,
{
    let mut number = 1;
    loop {
        let Attempt {
            result,
            durable_output_emitted,
        } = attempt().await;
        let error = match result {
            Ok(value) => return Retried::Ok(value),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        let Some(rejection) = retryable_rejection(number, durable_output_emitted, &message) else {
            return Retried::Failed(error);
        };
        let backoff = retry_backoff(number, rejection.retry_after, jitter_permille());
        // The 2026-07-28 investigation had to infer this whole failure class
        // from its absence in the log; one line per retry is what makes it
        // legible next time.
        eprintln!(
            "horizon-agent: provider rejected attempt {number}/{PROVIDER_REQUEST_MAX_ATTEMPTS} \
             before any durable output ({}); retrying in {backoff:?}: {}",
            rejection.describe(),
            truncate_for_log(&message),
        );
        if !sleep_unless_cancelled(backoff, token).await {
            return Retried::Cancelled;
        }
        number += 1;
    }
}

/// Keeps a provider error body (which can be arbitrarily long) from
/// dominating agentd's stderr.
fn truncate_for_log(message: &str) -> String {
    const LIMIT: usize = 300;
    let (head, truncated) = crate::transcript::truncate_chars(message, LIMIT);
    if truncated {
        format!("{head}…")
    } else {
        head
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
    if config.openai_enabled {
        match rig_openai_turn_with_retry(
            config,
            environment,
            extra_sections,
            &prompt,
            history_for_provider_request(rig_history, clearing.cleared()),
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
async fn rig_openai_turn_with_retry(
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    extra_sections: &[String],
    prompt: &Message,
    history: Vec<Message>,
    events_tx: &Sender<ProviderEvent>,
    token: &CancellationToken,
) -> anyhow::Result<(Message, TurnCompletion)> {
    let outcome = with_pre_generation_retry(token, || async {
        let mut durable_output_emitted = false;
        let result = rig_openai_turn_streaming(
            config,
            environment,
            extra_sections,
            prompt.clone(),
            history.clone(),
            events_tx.clone(),
            token,
            &mut durable_output_emitted,
        )
        .await;
        Attempt {
            result,
            durable_output_emitted,
        }
    })
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
async fn rig_openai_turn_streaming(
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    extra_sections: &[String],
    prompt: Message,
    history: Vec<Message>,
    events_tx: Sender<ProviderEvent>,
    token: &CancellationToken,
    durable_output_emitted: &mut bool,
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
    // `history` is the *provider view* of canonical history, already
    // projected through the Tier 1 clearing seam by the caller
    // (`super::clearing::history_for_provider_request`). Nothing is dropped:
    // the projection only replaces the bodies of tool results the session's
    // frozen cleared set names, keeping every call/result pair intact, and
    // `rig_history` itself still holds the originals.
    let stream_request = model
        .completion_request(prompt)
        .messages(history)
        .tools(rig_tool_definitions(config.allowed_tool_ids.as_deref()))
        .preamble(system_prompt(environment, extra_sections))
        .max_tokens(config.max_output_tokens)
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
    let mut input_tokens = None;
    let mut output_tokens: Option<u64> = None;
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
        // Decoded before anything is marked: an error chunk is not a token,
        // and an OpenAI-compatible endpoint delivers a rejected request's
        // status here rather than from stream establishment (rig sends the
        // HTTP request lazily, on the stream's first poll).
        let chunk = chunk?;
        if !first_token_seen {
            first_token_seen = true;
            // The gap between `ProviderRequestSent` above and this event is
            // provider time-to-first-byte, regardless of what kind of chunk
            // arrived first (text, reasoning, or a tool-call delta).
            let _ = events_tx.send(Event::ProviderRequestFirstToken.into());
        }

        match chunk {
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
            StreamedAssistantContent::ToolCall {
                mut tool_call,
                internal_call_id,
            } => {
                reasoning_buffer.flush();
                text_buffer.flush();
                // The provider payload records the provider's raw emission
                // (a double-encoded `arguments` string included) as the
                // forensic record, so it is captured before the repair
                // below, which only changes what Horizon executes and
                // replays.
                let provider_payload = rig_tool_call_provider_payload(&tool_call);
                repair_double_encoded_tool_arguments(&mut tool_call.function.arguments);
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
                    provider_payload,
                ));
                tool_call_progress.note_finalized(&internal_call_id);
                *durable_output_emitted = true;
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
                let usage = provider_request_usage_event_from_openai_final(&response);
                if let Event::ProviderRequestUsage(usage) = &usage {
                    input_tokens = Some(usage.input_tokens);
                    output_tokens = Some(usage.output_tokens);
                }
                let _ = events_tx.send(usage.into());
            }
        }
    }

    // The provider's response stream is done, either exhausted normally or
    // cut short by cancellation — either way, the request's wall-clock span
    // ends here, before the resulting message/tool-call events below.
    request_span.finish();

    reasoning_buffer.flush();
    text_buffer.flush();

    // One provider response persists as *several* events: a
    // `ToolCallRequested` per tool call, emitted above as its chunks arrive
    // (so Horizon can start executing it while the stream continues), plus
    // this one `MessageCommitted` for the accumulated text, which can only
    // be emitted now that the stream is done. In-memory history keeps them
    // together as the single assistant message assembled below, but a replay
    // of the log sees the text event *after* the calls -- and after any
    // result that landed before the stream ended. Rebuilds therefore fold
    // adjacent assistant messages back into one
    // (`mapping::repair_replayed_message_pairing`); nothing is missing from
    // the log, the grouping simply isn't recorded.
    if !text.is_empty() {
        let _ = events_tx.send(
            Event::MessageCommitted(AgentMessage {
                role: MessageRole::Assistant,
                text: text.clone(),
            })
            .into(),
        );
        *durable_output_emitted = true;
    }

    // `stream.choice` is only aggregated when the stream runs to its end;
    // on cancellation it is still the empty placeholder, so the history
    // message must be assembled from the chunks observed before the cancel —
    // otherwise the streamed partial (text and especially tool calls) would
    // be lost from history and cancelled tool results would dangle.
    let assistant_message = if cancelled {
        partial_assistant_message(stream.message_id.clone(), &text, tool_calls)
    } else {
        // `stream.choice` is rig's own aggregation of the same chunks, built
        // independently of the repaired `tool_calls` above, so it carries the
        // provider's raw arguments and needs the same normalization before it
        // becomes history.
        let mut content = stream.choice.clone();
        make_tool_call_arguments_replay_safe(&mut content);
        Message::Assistant {
            id: stream.message_id.clone(),
            content,
        }
    };

    let truncated_ids = tool_call_progress.truncated_ids();
    let truncated = !cancelled && !truncated_ids.is_empty();
    let cap_truncated = output_cap_truncated(output_tokens, config.max_output_tokens, cancelled);

    Ok((
        assistant_message,
        TurnCompletion {
            requested_tool_call_ids,
            requested_tool_calls,
            cancelled,
            failed: false,
            input_tokens,
            output_tokens,
            truncated,
            truncated_tool_call_count: truncated_ids.len(),
            cap_truncated,
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

/// Detects output-cap truncation by comparing the provider-reported output
/// token count against the configured ceiling (`config.max_output_tokens`).
///
/// `stop_reason`/`finish_reason` is not available: rig 0.39 parses
/// `finish_reason` (including `Length`) internally but discards it after
/// using it only for tool-call detection, and the streaming terminal element
/// (`StreamingCompletionResponse`) carries only `usage`. So truncation must
/// be *inferred* from the token count.
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
pub(super) fn make_tool_call_arguments_replay_safe(content: &mut OneOrMany<AssistantContent>) {
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

    let mut content = OneOrMany::many(content)
        .unwrap_or_else(|_| OneOrMany::one(AssistantContent::Text(Text::new(String::new()))));
    make_tool_call_arguments_replay_safe(&mut content);

    Message::Assistant {
        id: message_id,
        content,
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
///
/// Two tools are filtered beyond the allowlist. `task_output` is advertised
/// only when `task` itself is (`docs/agent-async-task-design.md` decision
/// 3, "advertise it only alongside `task`"). It is the same conditional
/// seam `prompt::DELEGATION_ROUTING_SECTION` rides on
/// (`session::advertises_task_tool`), decided from the same allowlist —
/// a session that cannot launch a task can never own one to read, so
/// offering the fetch tool would only be a call it must fail. `web_search`
/// is advertised only when `EXA_API_KEY` is set in the process environment:
/// without the key the Exa adapter can only return a "not configured" error,
/// so advertising it buys a round that cannot succeed (`web_fetch` needs no
/// key and stays advertised).
pub(super) fn rig_tool_definitions(allowed_tool_ids: Option<&[String]>) -> Vec<ToolDefinition> {
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
