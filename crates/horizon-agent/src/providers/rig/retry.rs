//! Provider rejection classification and cancellation-aware retry pacing.
//! Request construction, durable-output tracking and event delivery stay with completion.

use std::{future::Future, time::Duration};

use rig_core::completion::CompletionError;
use tokio_util::sync::CancellationToken;

/// How many times one provider request may be sent when the provider keeps
/// rejecting it before generating anything: the first send plus two retries.
///
/// Applies to 5xx gateway failures and transport-level failures. A 429
/// (rate limit) is exempt: it is a pre-generation rejection where re-sending
/// is always safe, so the harness paces on time (exponential backoff capped
/// at [`PROVIDER_RETRY_MAX_BACKOFF`]) and retries indefinitely — the turn
/// only ends via cancellation (`sleep_unless_cancelled`).
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
    /// body; absent, the exponential window below is used instead.
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
/// Gates that must pass: the request must not have reached generation
/// (anything after the provider started answering could be duplicated by a
/// retry); and the failure must be one of the transient shapes above rather
/// than a contract error or a stream timeout.
///
/// The attempt budget (`PROVIDER_REQUEST_MAX_ATTEMPTS`) applies to 5xx and
/// transport failures. A 429 rate-limit rejection is exempt: re-sending is
/// always safe before generation, so the harness paces on time and retries
/// indefinitely rather than giving up after a fixed attempt count.
pub(super) fn retryable_rejection(
    attempt: u32,
    durable_output_emitted: bool,
    error: &anyhow::Error,
) -> Option<TransientRejection> {
    if durable_output_emitted {
        return None;
    }
    let message = format!("{error:#}");
    let failure = classify_failure(error, &message);
    if let Some(status) = failure.status {
        if !RETRYABLE_STATUSES.contains(&status) {
            return None;
        }
        // 429 is a pre-generation rate-limit: re-sending is always safe, so it
        // retries on time rather than on an attempt count. 5xx stays under the
        // budget — a 500 can be deterministic, and the budget bounds that cost.
        if status != 429 && attempt >= PROVIDER_REQUEST_MAX_ATTEMPTS {
            return None;
        }
        return Some(TransientRejection {
            status: Some(status),
            retry_after: failure.retry_after,
            mid_stream: false,
        });
    }
    // Transport-level failure (no status came back): bounded by the attempt
    // budget, same as 5xx.
    if attempt >= PROVIDER_REQUEST_MAX_ATTEMPTS {
        return None;
    }
    failure.transport.then_some(TransientRejection {
        status: None,
        retry_after: None,
        mid_stream: failure.mid_stream,
    })
}

/// The signal a failure carries for the retry decision, read from rig's
/// typed error where it exists and from Display markers as a fallback.
struct ClassifiedFailure {
    status: Option<u16>,
    retry_after: Option<Duration>,
    transport: bool,
    mid_stream: bool,
}

/// rig 0.42 routes request-id-contract providers' failures (OpenAI among
/// them) through `CompletionError::ProviderResponse`, which carries the
/// status, the preserved headers (`Retry-After`), and the provider request
/// id as structured data; a 2026-09 regression showed the Display-only
/// classifier silently stopped retrying those. Text-only paths (providers
/// without the contract, or `ProviderError(String)`) keep the marker
/// fallback.
fn classify_failure(error: &anyhow::Error, message: &str) -> ClassifiedFailure {
    if let Some(response) = error.downcast_ref::<CompletionError>() {
        let status = response
            .provider_response_status()
            .map(|status| status.as_u16());
        let retry_after = response
            .provider_response_headers()
            .and_then(|headers| headers.get("retry-after"))
            .and_then(|value| value.to_str().ok())
            .and_then(retry_after_seconds)
            // Some providers echo the hint into the JSON body instead of a
            // header; the body scrape still covers that shape.
            .or_else(|| named_retry_after(message));
        if status.is_some() || retry_after.is_some() {
            return ClassifiedFailure {
                status,
                retry_after,
                transport: false,
                mid_stream: false,
            };
        }
    }
    ClassifiedFailure {
        status: rejected_status(message),
        retry_after: named_retry_after(message),
        transport: message.contains(TRANSPORT_FAILURE_MARKER),
        mid_stream: message.contains("error decoding response body"),
    }
}

/// Parses a whole-seconds `Retry-After` header value. The HTTP-date form is
/// deliberately not handled (None): the exponential backoff is a fine
/// stand-in, and date parsing does not earn its complexity here.
fn retry_after_seconds(value: &str) -> Option<Duration> {
    let trimmed = value.trim();
    if !trimmed.starts_with(|character: char| character.is_ascii_digit()) {
        return None;
    }
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().map(Duration::from_secs)
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
/// mistaken for one. Fallback only: request-id-contract providers carry a
/// real header on the typed error, which `classify_failure` prefers.
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
/// `rig_provider_turn_with_retry`.
pub(super) async fn with_pre_generation_retry<T, F, Fut>(
    token: &CancellationToken,
    mut attempt: F,
    mut on_retry: impl FnMut(u32, TransientRejection, Duration),
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
        let Some(rejection) = retryable_rejection(number, durable_output_emitted, &error) else {
            return Retried::Failed(error);
        };
        let backoff = retry_backoff(number, rejection.retry_after, jitter_permille());
        // The 2026-07-28 investigation had to infer this whole failure class
        // from its absence in the log; one line per retry is what makes it
        // legible next time. The provider request id (rig 0.42 preserves it
        // for request-id-contract providers) is what provider support asks
        // for, so it rides along when present.
        let request_suffix = provider_request_id_of(&error)
            .as_deref()
            .map(|id| format!("; request id {id}"))
            .unwrap_or_default();
        eprintln!(
            "horizon-agent: provider rejected attempt {number} before any durable output \
             ({}{}); retrying in {backoff:?}: {}",
            rejection.describe(),
            request_suffix,
            truncate_for_log(&message),
        );
        on_retry(number, rejection, backoff);
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

/// The provider's transport request id for a failed call, when rig captured
/// one (0.42 preserves it for request-id-contract providers such as OpenAI
/// -- the id provider support asks for when investigating a failure).
fn provider_request_id_of(error: &anyhow::Error) -> Option<String> {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<CompletionError>())
        .find_map(|error| error.provider_request_id().map(str::to_string))
}
