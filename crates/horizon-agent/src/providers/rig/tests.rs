use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use super::completion::{
    await_provider_phase, openai_turn_additional_params, partial_assistant_message,
    provider_request_usage_event_from_openai_final, retry_backoff, retryable_rejection,
    rig_tool_definitions, sleep_unless_cancelled, with_pre_generation_retry, Attempt,
    ProviderRequestSpan, ProviderWait, Retried, TurnCompletion, MULTI_TOOL_TEST_BATCH_SIZE,
    PROVIDER_REQUEST_MAX_ATTEMPTS, PROVIDER_RETRY_MAX_BACKOFF,
};
use super::mapping::{
    horizon_events_from_rig_message, horizon_provider_events_from_rig_message,
    horizon_tool_definition_from_rig, repair_replayed_message_pairing,
    rig_messages_from_horizon_events, rig_tool_call_provider_payload, rig_tool_call_request,
    rig_workspace_snapshot_call, rig_workspace_snapshot_call_with_provider_metadata,
    RIG_PROVIDER_PAYLOAD_SCHEMA, RIG_PROVIDER_PAYLOAD_VERSION,
};
use super::session::{
    append_cancelled_tool_results_to_history, apply_turn_outcome, fold_batched_tool_result,
    halt_turn_loop, session_environment, session_extra_sections, tool_result_fingerprint,
    BatchStep, GuardHalt, TurnLoopGuard,
};
use super::*;
use crate::config::RigAgentConfig;
use crate::roles::{resolve, RoleId};

/// Mirrors the built-in defaults in `agent::config` (`DEFAULT_ITERATION_CAP`/
/// `DEFAULT_DOOM_LOOP_WINDOW`) for these guard-logic unit tests, which
/// exercise `TurnLoopGuard` directly rather than through config precedence
/// (that precedence is covered in `agent::config`'s own tests).
const TEST_ITERATION_CAP: u32 = 100;
const TEST_DOOM_LOOP_WINDOW: usize = 5;
use crate::contract::SessionId;
use crate::contract::{
    Command, Event, Message as AgentMessage, MessageDelta, MessageRole, Provider as AgentProvider,
    ProviderEvent, ProviderId, ProviderRequestUsage, SessionState, StartSession, ToolCallId,
    ToolCallRequest, ToolCallResult, ToolPermission, TurnEndReason,
};
use rig_core::{
    completion::{
        message::{Text, ToolCall, ToolFunction, ToolResultContent, UserContent},
        AssistantContent, Message as RigMessage, ToolDefinition,
    },
    OneOrMany,
};

fn recv(rx: &crossbeam_channel::Receiver<ProviderEvent>) -> ProviderEvent {
    rx.recv_timeout(std::time::Duration::from_secs(1))
        .expect("expected a provider event within timeout")
}

#[tokio::test]
async fn provider_phase_times_out_instead_of_waiting_forever() {
    let token = tokio_util::sync::CancellationToken::new();
    let error = await_provider_phase(
        std::future::pending::<()>(),
        &token,
        std::time::Duration::from_millis(1),
        "test phase",
    )
    .await
    .expect_err("a silent provider phase must time out");

    assert_eq!(error.to_string(), "provider test phase timed out after 1ms");
}

#[tokio::test]
async fn provider_phase_cancellation_interrupts_a_pending_wait() {
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();

    assert_eq!(
        await_provider_phase(
            std::future::pending::<()>(),
            &token,
            std::time::Duration::from_secs(60),
            "test phase",
        )
        .await
        .expect("cancellation is a normal provider wait outcome"),
        ProviderWait::Cancelled
    );
}

#[test]
fn provider_request_span_finishes_when_an_error_path_drops_it() {
    let (tx, rx) = crossbeam_channel::unbounded();
    drop(ProviderRequestSpan::new(tx));

    assert!(matches!(recv(&rx).event, Event::ProviderRequestFinished));
}

/// The exact shape rig renders a non-2xx streaming response as: the status
/// arrives as the response stream's first item (rig sends the HTTP request
/// lazily), wrapped in a `ProviderError`.
fn rejected_with(status: &str, body: &str) -> String {
    format!("ProviderError: Invalid status code {status} with message: {body}")
}

/// The retry classifier, tested directly: `horizon-agent` has no way to
/// drive a real provider from a unit test, so the decision function is the
/// testable surface (the loop it feeds is covered separately below).
///
/// Retryable only when the provider said "not now" *and* no durable output
/// had been emitted yet — a rejection before any `ToolCallRequested` or
/// `MessageCommitted` is provably safe to re-send (reasoning and text deltas
/// are volatile, never entered history), so re-sending cannot duplicate a
/// generation.
#[test]
fn rejections_before_generation_are_classified_as_retryable() {
    let rate_limited = retryable_rejection(
        1,
        false,
        &rejected_with(
            "429 Too Many Requests",
            r#"{"error":{"message":"Too many concurrent requests"}}"#,
        ),
    )
    .expect("a pre-generation 429 is retryable");
    assert_eq!(rate_limited.status, Some(429));
    assert_eq!(rate_limited.retry_after, None);

    // 500 rides with the gateway failures: synthetic.new reports an
    // upstream hiccup that way (an incident on 2026-07-30 killed two
    // sessions with `{"error":"Error from inference backend: ..."}`), and
    // before any durable output a repeat cannot duplicate a generation.
    for status in [
        "500 Internal Server Error",
        "502 Bad Gateway",
        "503 Service Unavailable",
        "504 Gateway Timeout",
    ] {
        assert!(
            retryable_rejection(1, false, &rejected_with(status, "upstream is unwell")).is_some(),
            "{status} is a transient gateway failure"
        );
    }

    // The connection-level shape: no status ever came back, so the request
    // never reached the model.
    let transport = retryable_rejection(
        1,
        false,
        "ProviderError: Http client error: error sending request for url \
         (https://example.invalid/v1/chat/completions)",
    )
    .expect("a connection failure is retryable");
    assert_eq!(transport.status, None);
    assert!(!transport.mid_stream, "pre-send is not mid-stream");
}

/// A mid-stream transport failure (the response started but a body decode
/// failed — r30's `error decoding response body`) is retryable when no
/// durable output was emitted: reasoning and text deltas are volatile, so
/// re-sending cannot duplicate a generation.
#[test]
fn a_mid_stream_transport_failure_with_no_durable_output_is_retryable() {
    let rejection = retryable_rejection(
        1,
        false, // no ToolCallRequested or MessageCommitted
        "ProviderError: Http client error: error decoding response body: \
         trailing comma at line 1 column 42",
    )
    .expect("a mid-stream failure with no durable output is retryable");
    assert_eq!(rejection.status, None);
    assert!(
        rejection.mid_stream,
        "error decoding response body is mid-stream"
    );
}

/// The same mid-stream failure is NOT retryable once durable output was
/// emitted — a tool call or committed message is already in history, and a
/// retry could duplicate it.
#[test]
fn a_mid_stream_transport_failure_after_durable_output_is_not_retried() {
    assert!(
        retryable_rejection(
            1,
            true, // a ToolCallRequested was emitted
            "ProviderError: Http client error: error decoding response body: \
             trailing comma at line 1 column 42",
        )
        .is_none(),
        "a mid-stream failure after durable output must not be retried"
    );
}

/// Everything else stays fatal exactly as before: a 4xx that is not 429
/// describes the request itself, a failure after durable output was
/// emitted could be duplicated by a retry, the attempt budget is finite,
/// and the response-stream timeout is deliberately excluded (silence
/// proves nothing about whether durable output was emitted — see
/// `PROVIDER_STREAM_IDLE_TIMEOUT`).
#[test]
fn rejections_that_could_duplicate_or_repeat_are_not_retried() {
    for status in [
        "400 Bad Request",
        "401 Unauthorized",
        "403 Forbidden",
        "404 Not Found",
        "422 Unprocessable Entity",
    ] {
        assert!(
            retryable_rejection(1, false, &rejected_with(status, "no")).is_none(),
            "{status} would fail identically on a retry"
        );
    }

    assert!(
        retryable_rejection(
            1,
            true,
            &rejected_with("429 Too Many Requests", "slow down")
        )
        .is_none(),
        "a rejection after durable output must never be retried"
    );

    assert!(
        retryable_rejection(
            PROVIDER_REQUEST_MAX_ATTEMPTS,
            false,
            &rejected_with("429 Too Many Requests", "slow down"),
        )
        .is_none(),
        "the attempt budget is finite"
    );

    assert!(
        retryable_rejection(1, false, "provider response stream timed out after 120s").is_none(),
        "a stream timeout may already have generated tokens"
    );
}

/// `Retry-After` is honoured when the provider names one. rig surfaces no
/// response headers, so the only place it can be read from is the error
/// body the provider echoed it into.
#[test]
fn a_named_retry_after_is_read_out_of_the_rejection() {
    let rejection = retryable_rejection(
        1,
        false,
        &rejected_with(
            "429 Too Many Requests",
            r#"{"error":{"message":"rate limited","retry_after":7}}"#,
        ),
    )
    .expect("retryable");

    assert_eq!(rejection.retry_after, Some(Duration::from_secs(7)));
    assert_eq!(
        retry_backoff(1, rejection.retry_after, 0),
        Duration::from_secs(3) + Duration::from_millis(500),
        "the provider's own window replaces the exponential term"
    );
}

/// Equal jitter over a doubling window, clamped at the ceiling: the low end
/// of each window is half of it, the high end the whole window.
#[test]
fn retry_backoff_grows_per_attempt_and_stays_bounded() {
    assert_eq!(retry_backoff(1, None, 0), Duration::from_millis(500));
    assert_eq!(retry_backoff(1, None, 1_000), Duration::from_secs(1));
    assert_eq!(retry_backoff(2, None, 0), Duration::from_secs(1));
    assert_eq!(retry_backoff(3, None, 0), Duration::from_secs(2));
    assert_eq!(
        retry_backoff(1, Some(Duration::from_secs(600)), 1_000),
        PROVIDER_RETRY_MAX_BACKOFF,
        "a provider asking for ten minutes is still clamped"
    );
}

/// The loop itself, with a scripted attempt standing in for the provider: a
/// pre-generation 503 is re-sent and the second attempt's response is the
/// turn's response.
#[tokio::test]
async fn a_transient_rejection_is_retried_and_the_second_attempt_wins() {
    let token = tokio_util::sync::CancellationToken::new();
    let attempts = std::cell::Cell::new(0u32);

    let outcome = with_pre_generation_retry(&token, || async {
        attempts.set(attempts.get() + 1);
        if attempts.get() == 1 {
            Attempt {
                result: Err(anyhow::anyhow!(rejected_with(
                    "503 Service Unavailable",
                    "upstream is unwell"
                ))),
                durable_output_emitted: false,
            }
        } else {
            Attempt {
                result: Ok("the answer"),
                durable_output_emitted: true,
            }
        }
    })
    .await;

    assert!(matches!(outcome, Retried::Ok("the answer")));
    assert_eq!(attempts.get(), 2);
}

/// A failure after the provider produced durable output ends the turn
/// on the first attempt — the standing no-duplicate-generation rule.
#[tokio::test]
async fn a_failure_after_durable_output_emitted_ends_the_turn_without_retrying() {
    let token = tokio_util::sync::CancellationToken::new();
    let attempts = std::cell::Cell::new(0u32);

    let outcome = with_pre_generation_retry::<(), _, _>(&token, || async {
        attempts.set(attempts.get() + 1);
        Attempt {
            result: Err(anyhow::anyhow!(rejected_with(
                "429 Too Many Requests",
                "slow down"
            ))),
            durable_output_emitted: true,
        }
    })
    .await;

    assert!(matches!(outcome, Retried::Failed(_)));
    assert_eq!(attempts.get(), 1);
}

/// A cancel wins over a pending retry immediately: the backoff is never
/// waited out, and no further attempt is made.
#[tokio::test]
async fn a_cancel_during_backoff_wins_over_the_pending_retry() {
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let attempts = std::cell::Cell::new(0u32);

    let started = std::time::Instant::now();
    let outcome = with_pre_generation_retry::<(), _, _>(&token, || async {
        attempts.set(attempts.get() + 1);
        Attempt {
            result: Err(anyhow::anyhow!(rejected_with(
                "429 Too Many Requests",
                "slow down"
            ))),
            durable_output_emitted: false,
        }
    })
    .await;

    assert!(matches!(outcome, Retried::Cancelled));
    assert_eq!(attempts.get(), 1);
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "a cancelled turn must not wait out the backoff"
    );
    assert!(!sleep_unless_cancelled(Duration::from_secs(60), &token).await);
}

#[test]
fn openai_turns_explicitly_enable_parallel_tool_calls() {
    assert_eq!(
        openai_turn_additional_params()["parallel_tool_calls"],
        serde_json::Value::Bool(true)
    );
}

/// Exercises the same `rig_core` builder call
/// `rig_openai_turn_streaming` makes (`model.completion_request(..)
/// .max_tokens(config.max_output_tokens)`) with a locally-built client --
/// pure object construction, no network I/O -- to prove the explicit
/// `max_tokens` (`DEFAULT_AGENT_MAX_OUTPUT_TOKENS`, `agent::config`) is
/// actually carried onto the request rig-core builds, not just present on
/// `RigAgentConfig`. Before the 2026-07-27 audit this was unset entirely;
/// see `docs/research/agent-ceiling-death-autopsy-2026-07-26.md` for why
/// that mattered.
#[test]
fn openai_turn_completion_request_carries_the_explicit_max_tokens() {
    use rig_core::client::CompletionClient;
    use rig_core::completion::CompletionModel;

    let config = RigAgentConfig {
        openai_enabled: true,
        model: "test-model".to_string(),
        ..Default::default()
    };
    let client = rig_core::providers::openai::CompletionsClient::builder()
        .api_key("test-key")
        .build()
        .expect("client construction performs no network I/O");
    let model = client.completion_model(&config.model);
    let request = model
        .completion_request(RigMessage::user("hi"))
        .max_tokens(config.max_output_tokens)
        .build();

    assert_eq!(
        request.max_tokens,
        Some(crate::config::DEFAULT_AGENT_MAX_OUTPUT_TOKENS)
    );
}

#[test]
fn openai_stream_final_usage_emits_cached_input_event() {
    let response =
        rig_core::providers::openai::completion::streaming::StreamingCompletionResponse {
            usage: rig_core::providers::openai::completion::Usage {
                prompt_tokens: 100,
                total_tokens: 125,
                prompt_tokens_details: Some(
                    rig_core::providers::openai::completion::PromptTokensDetails {
                        cached_tokens: 80,
                    },
                ),
            },
        };

    assert_eq!(
        provider_request_usage_event_from_openai_final(&response),
        Event::ProviderRequestUsage(ProviderRequestUsage {
            input_tokens: 100,
            output_tokens: 25,
            total_tokens: 125,
            cached_input_tokens: 80,
        })
    );
}

#[test]
fn provider_usage_event_persists_through_the_generic_duckdb_record() {
    let store = crate::persistence::projection::duckdb::Store::open_in_memory().expect("store");
    let session_id = SessionId::new();
    let usage = ProviderRequestUsage {
        input_tokens: 100,
        output_tokens: 25,
        total_tokens: 125,
        cached_input_tokens: 80,
    };

    store
        .append_event(crate::persistence::projection::duckdb::AppendEvent {
            session_id,
            turn_id: Some("turn-1".to_string()),
            provider_id: Some(ProviderId("builtin.agent.rig".to_string())),
            role_id: None,
            event: Event::ProviderRequestUsage(usage),
            provider_payload: None,
        })
        .expect("append usage event");

    let events = store.events_for_session(session_id).expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, Event::ProviderRequestUsage(usage));
    assert_eq!(events[0].event_kind, "provider_request_usage");
}

#[test]
fn converts_rig_assistant_text_to_horizon_message() {
    let events = horizon_events_from_rig_message(RigMessage::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text::new("hello"))),
    });

    assert!(matches!(
        events.as_slice(),
        [Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text,
        })] if text == "hello"
    ));
}

#[test]
fn emits_rig_reasoning_before_assistant_text() {
    let events = horizon_events_from_rig_message(RigMessage::Assistant {
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
            Event::ReasoningDelta(delta),
            Event::MessageCommitted(AgentMessage {
                role: MessageRole::Assistant,
                text,
            }),
        ] if delta.text == "thinking first" && text == "final answer"
    ));
}

#[test]
fn converts_rig_tool_call_to_horizon_tool_request() {
    let events = horizon_events_from_rig_message(RigMessage::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(rig_workspace_snapshot_call())),
    });

    assert!(matches!(
        events.as_slice(),
        [Event::ToolCallRequested(request)]
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
    let events = horizon_provider_events_from_rig_message(RigMessage::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(
            rig_workspace_snapshot_call_with_provider_metadata(),
        )),
    });

    assert!(matches!(
        events.as_slice(),
        [ProviderEvent {
            event: Event::ToolCallRequested(request),
            provider_payload: Some(payload),
            ..
        }] if request.call_id.0 == "provider-call-1"
            && payload["schema"] == RIG_PROVIDER_PAYLOAD_SCHEMA
            && payload["rig"]["tool_call"]["id"] == "rig-workspace-snapshot-1"
    ));
}

#[test]
fn tool_call_delta_buffer_emits_progress_and_final_tool_call_still_works_unchanged() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut buffer = ToolCallProgressBuffer::new(tx, &RigAgentConfig::default());

    // A name chunk flushes immediately, before any arguments have streamed.
    buffer.note_name("internal-call-1", "fs.write".to_string());
    let progress = recv(&rx)
        .tool_call_progress
        .expect("name chunk produces a progress tick");
    assert_eq!(progress.key, "internal-call-1");
    assert_eq!(progress.tool_id.as_deref(), Some("fs.write"));
    assert_eq!(progress.bytes, 0);

    // Argument chunks accumulate bytes; `flush_for_tests` bypasses the
    // normal time-gated cadence so the test doesn't need to sleep.
    buffer.note_delta("internal-call-1", "{\"path\":\"/tmp/x\"}");
    buffer.flush_for_tests();
    let progress = recv(&rx)
        .tool_call_progress
        .expect("delta chunk produces a progress tick");
    assert_eq!(progress.tool_id.as_deref(), Some("fs.write"));
    assert_eq!(progress.bytes, "{\"path\":\"/tmp/x\"}".len());

    // The buffer is purely a side channel: a complete, non-streamed tool
    // call still maps to a normal `Event::ToolCallRequested`, not a
    // progress event.
    let events = horizon_events_from_rig_message(RigMessage::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(rig_workspace_snapshot_call())),
    });
    assert!(matches!(
        events.as_slice(),
        [Event::ToolCallRequested(request)] if request.tool_id == "workspace.snapshot"
    ));
}

/// The truncation detector: a call that received streaming deltas
/// (`note_name`/`note_delta`) but was never finalized (`note_finalized`)
/// appears in `truncated_ids`. This is the r29 shape — the provider
/// started streaming a tool call's arguments but hit its output ceiling
/// mid-argument, and rig's `take_finalized_tool_calls` dropped the
/// incomplete call.
#[test]
fn tool_call_progress_buffer_detects_truncation_when_a_started_call_is_never_finalized() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer = ToolCallProgressBuffer::new(tx, &RigAgentConfig::default());

    // Two calls received deltas.
    buffer.note_name("call-a", "fs.write".to_string());
    buffer.note_delta("call-a", "{\"path\":\"/tmp/x\"");
    buffer.note_name("call-b", "fs.read".to_string());
    buffer.note_delta("call-b", "{\"path\":\"/tmp/y\"");

    // Only call-a was finalized; call-b was truncated.
    buffer.note_finalized("call-a");

    let truncated = buffer.truncated_ids();
    assert_eq!(truncated, vec!["call-b".to_string()]);
}

/// No truncation when every started call was finalized — the normal
/// stream shape.
#[test]
fn tool_call_progress_buffer_reports_no_truncation_when_all_started_calls_are_finalized() {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut buffer = ToolCallProgressBuffer::new(tx, &RigAgentConfig::default());

    buffer.note_name("call-a", "fs.write".to_string());
    buffer.note_delta("call-a", "{\"path\":\"/tmp/x\"}");
    buffer.note_finalized("call-a");

    buffer.note_name("call-b", "fs.read".to_string());
    buffer.note_delta("call-b", "{\"path\":\"/tmp/y\"}");
    buffer.note_finalized("call-b");

    assert!(buffer.truncated_ids().is_empty());
}

/// The minting site is what makes every per-occurrence consumer possible
/// (`contract::OccurrenceId`): two calls that share a provider `call_id` --
/// the `functions.fs.edit:66` reuse shape -- must still come out with
/// distinct occurrences, or the transcript, approval, and analytics all
/// collapse them again.
#[test]
fn rig_tool_call_request_mints_a_distinct_occurrence_per_call() {
    let mint = || {
        rig_tool_call_request(ToolCall::new(
            "functions.fs.edit:66".to_string(),
            ToolFunction::new(
                "fs.edit".to_string(),
                serde_json::json!({ "path": "a.txt" }),
            ),
        ))
    };
    let first = mint();
    let second = mint();

    assert_eq!(first.call_id, second.call_id);
    assert!(first.occurrence_id.is_some());
    assert_ne!(
        first.occurrence_id, second.occurrence_id,
        "two calls sharing a provider call_id must still be separable"
    );
}

#[test]
fn duckdb_store_preserves_rig_provider_payload_for_tool_call() {
    let store = crate::persistence::projection::duckdb::Store::open_in_memory().expect("store");
    let session_id = crate::contract::SessionId::new();
    let call = rig_workspace_snapshot_call_with_provider_metadata();
    let provider_payload = rig_tool_call_provider_payload(&call);
    let event = Event::ToolCallRequested(rig_tool_call_request(call));

    store
        .append_event(crate::persistence::projection::duckdb::AppendEvent {
            session_id,
            turn_id: Some("turn-1".to_string()),
            provider_id: Some(ProviderId("builtin.agent.rig".to_string())),
            role_id: None,
            event,
            provider_payload: Some(provider_payload.clone()),
        })
        .expect("append rig payload event");

    let events = store.events_for_session(session_id).expect("events");
    assert_eq!(
        events[0].provider_id,
        Some(ProviderId("builtin.agent.rig".to_string()))
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
        ToolPermission::AutoAllowRead,
    );

    assert_eq!(definition.id, "workspace.snapshot");
    assert_eq!(definition.permission, ToolPermission::AutoAllowRead);
}

#[test]
fn rebuilds_rig_memory_messages_from_horizon_transcript_events() {
    let events = vec![
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "snapshot please".to_string(),
        }),
        Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "workspace.snapshot".to_string(),
            input: serde_json::json!({}).into(),
            occurrence_id: None,
        }),
        Event::ToolCallFinished(ToolCallResult::new(
            ToolCallId("call-1".to_string()),
            None,
            serde_json::json!({ "tab_count": 1 }),
        )),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "There is one tab.".to_string(),
        }),
    ];

    let messages = rig_messages_from_horizon_events(&events);

    assert!(matches!(&messages[0], RigMessage::User { .. }));
    assert!(matches!(
        &messages[1],
        RigMessage::Assistant { content, .. }
            if matches!(content.first_ref(), AssistantContent::ToolCall(call)
                if call.id == "call-1" && call.function.name == "workspace.snapshot")
    ));
    assert!(matches!(&messages[2], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == "call-1"
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("tab_count")))));
    assert!(matches!(&messages[3], RigMessage::Assistant { .. }));
}

/// `load_rig_history` reads through the *shared* `Arc<Mutex<Store>>` handle
/// -- never a fresh `Store::open` of the same path (see that function's and
/// `SharedDuckdbStore`'s doc comments for why a second independent open is
/// unsound with DuckDB's relaxed durability). This appends through the same
/// `Arc` `load_rig_history` reads through, exactly mirroring how
/// `event_log::writer`'s background thread and a resumed session's rig
/// thread now share one instance in production.
#[test]
fn loads_initial_rig_history_from_duckdb_projection() {
    use crate::persistence::projection::duckdb::DuckdbStoreHandle;

    let path = std::env::temp_dir().join(format!(
        "horizon-rig-memory-{}.duckdb",
        uuid::Uuid::new_v4()
    ));
    let session_id = crate::contract::SessionId::new();
    let events = vec![
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "hello".to_string(),
        }),
        Event::AssistantTextDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: "streaming ignored".to_string(),
        }),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "hi".to_string(),
        }),
    ];

    let store = crate::persistence::projection::duckdb::Store::open(&path).expect("open store");
    store
        .append_events(
            session_id,
            Some(ProviderId("builtin.agent.rig".to_string())),
            events.clone(),
        )
        .expect("append events");
    let shared_store = DuckdbStoreHandle::new(store);

    let persisted = load_rig_session_history(Some(&shared_store), session_id, &[]);
    assert_eq!(
        persisted.messages,
        rig_messages_from_horizon_events(&events)
    );
    assert!(
        persisted.cleared_call_ids.is_empty(),
        "a log with no clearing pass restores an empty cleared set"
    );

    drop(shared_store);
    let _ = std::fs::remove_file(path);
}

/// Issue 012: when the DuckDB projection store is unavailable (`None` — it
/// failed to open or rebuild, e.g. lock contention from a stale daemon),
/// `load_rig_session_history` must not silently return an empty history.
/// Instead it rebuilds from the JSONL event log's events the resume path
/// threads through `StartSession::history`, using the same reconstruction
/// the store path uses. This test proves the fallback: `None` store +
/// non-empty fallback events → the same messages and cleared set the store
/// would have produced.
#[test]
fn load_rig_session_history_falls_back_to_event_log_when_store_is_unavailable() {
    let session_id = crate::contract::SessionId::new();
    let events = vec![
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "what is 2+2?".to_string(),
        }),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "4".to_string(),
        }),
    ];

    let persisted = load_rig_session_history(None, session_id, &events);

    assert_eq!(
        persisted.messages,
        rig_messages_from_horizon_events(&events),
        "a None store must rebuild the same messages from the fallback events"
    );
    assert!(
        persisted.cleared_call_ids.is_empty(),
        "no clearing events → empty cleared set"
    );
}

/// Issue 012: when the store is `None` and the fallback events are also
/// empty (a fresh `Control::SessionNew`, or persistence unavailable in a
/// test), the original empty-history return is preserved — no spurious
/// reconstruction, no error.
#[test]
fn load_rig_session_history_returns_empty_when_store_and_events_are_both_empty() {
    let session_id = crate::contract::SessionId::new();
    let persisted = load_rig_session_history(None, session_id, &[]);
    assert!(
        persisted.messages.is_empty(),
        "no store and no events → empty messages"
    );
    assert!(
        persisted.cleared_call_ids.is_empty(),
        "no store and no events → empty cleared set"
    );
}

/// The resume path end to end: a session whose persisted events carry the
/// `b182c25b` shape (a streamed assistant text between a tool call and its
/// result, plus a tool call the interrupted turn never answered) comes back
/// from the DuckDB projection as a pairing-valid history, not one a strict
/// chat template rejects forever.
#[test]
fn resumed_history_from_duckdb_is_pairing_valid() {
    use crate::persistence::projection::duckdb::DuckdbStoreHandle;

    let path = std::env::temp_dir().join(format!(
        "horizon-rig-pairing-{}.duckdb",
        uuid::Uuid::new_v4()
    ));
    let session_id = crate::contract::SessionId::new();
    let events = vec![
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "how many tabs?".to_string(),
        }),
        tool_call_request("call-1"),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "Let me check.".to_string(),
        }),
        tool_call_finished("call-1"),
        tool_call_request("call-2"),
    ];

    let store = crate::persistence::projection::duckdb::Store::open(&path).expect("open store");
    store
        .append_events(
            session_id,
            Some(ProviderId("builtin.agent.rig".to_string())),
            events,
        )
        .expect("append events");
    let shared_store = DuckdbStoreHandle::new(store);

    let persisted = load_rig_session_history(Some(&shared_store), session_id, &[]);
    // user, assistant(call-1 + "Let me check."), tool(call-1),
    // assistant(call-2), tool(cancelled call-2).
    assert_pairing_valid(&persisted.messages);
    assert_eq!(persisted.messages.len(), 5);

    drop(shared_store);
    let _ = std::fs::remove_file(path);
}

#[test]
fn horizon_mediated_tool_result_can_continue_as_rig_history() {
    let tool_call = rig_workspace_snapshot_call();
    let mut events = horizon_events_from_rig_message(RigMessage::from(tool_call));
    let request = match events.first().expect("tool request") {
        Event::ToolCallRequested(request) => request.clone(),
        other => panic!("expected tool request, got {other:?}"),
    };

    events.push(Event::ToolCallStarted(request.call_id.clone()));
    events.push(Event::ToolCallFinished(ToolCallResult::new(
        request.call_id.clone(),
        None,
        serde_json::json!({
            "tab_count": 1,
            "active_title": "Agent #1",
        }),
    )));

    let messages = rig_messages_from_horizon_events(&events);

    assert_eq!(messages.len(), 2);
    assert!(matches!(
        &messages[0],
        RigMessage::Assistant { content, .. }
            if matches!(content.first_ref(), AssistantContent::ToolCall(call)
                if call.id == request.call_id.0)
    ));
    assert!(matches!(&messages[1], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == request.call_id.0)));
}

/// A persisted background-`task` notification replays to the provider as a
/// plain user-role text message -- exactly what the live injection sent
/// (`session::inject_task_notification`). Replaying it as anything else
/// would change the model's view of its own past between a live turn and a
/// resumed one, and the *only* reason the role is distinct at all is
/// persistence and the transcript.
#[test]
fn a_task_notification_replays_to_the_provider_as_a_user_message() {
    let events = vec![Event::MessageCommitted(AgentMessage {
        role: MessageRole::TaskNotification,
        text: "task \"map the emit sites\" completed".to_string(),
    })];

    let messages = rig_messages_from_horizon_events(&events);

    assert_eq!(messages.len(), 1);
    assert!(
        matches!(&messages[0], RigMessage::User { content }
            if matches!(content.first_ref(), UserContent::Text(text)
                if text.text == "task \"map the emit sites\" completed")),
        "got {:?}",
        messages[0]
    );
}

// --- Rebuilt-history tool-call pairing --------------------------------
//
// The 2026-07-28 session death (session `b182c25b`): the runtime was
// reloaded mid-turn, and the history rebuilt from the event log carried a
// tool-role message that no *immediately preceding* assistant message
// announced. MiniMax-M3's chat template rejects that outright ("Message has
// tool role, but there was no previous assistant message with a tool call!")
// and, because the shape sits in the rebuilt history, every later request in
// the session 400s too.

/// The invariant a strict chat template enforces, asserted over a rebuilt
/// sequence: every tool-role message's call id was announced by a preceding
/// assistant message, and the nearest preceding assistant message carries at
/// least one tool call (no text-only assistant message separates a call from
/// its result).
fn assert_pairing_valid(messages: &[RigMessage]) {
    let mut announced: HashSet<String> = HashSet::new();
    let mut nearest_assistant_announced = false;
    for (index, message) in messages.iter().enumerate() {
        match message {
            RigMessage::Assistant { content, .. } => {
                let calls: Vec<String> = content
                    .iter()
                    .filter_map(|item| match item {
                        AssistantContent::ToolCall(call) => Some(call.id.clone()),
                        _ => None,
                    })
                    .collect();
                nearest_assistant_announced = !calls.is_empty();
                announced.extend(calls);
            }
            RigMessage::User { content } => {
                let UserContent::ToolResult(result) = content.first_ref() else {
                    continue;
                };
                assert!(
                    announced.contains(&result.id),
                    "message {index} answers unannounced call {}: {messages:?}",
                    result.id
                );
                assert!(
                    nearest_assistant_announced,
                    "message {index} is a tool result, but the nearest assistant message \
                     announces no tool call: {messages:?}"
                );
            }
            RigMessage::System { .. } => {}
        }
    }
}

fn tool_call_request(call_id: &str) -> Event {
    Event::ToolCallRequested(ToolCallRequest {
        call_id: ToolCallId(call_id.to_string()),
        tool_id: "workspace.snapshot".to_string(),
        input: serde_json::json!({}).into(),
        occurrence_id: None,
    })
}

fn tool_call_finished(call_id: &str) -> Event {
    Event::ToolCallFinished(ToolCallResult::new(
        ToolCallId(call_id.to_string()),
        None,
        serde_json::json!({ "tab_count": 1 }),
    ))
}

/// The `b182c25b` shape itself: a tool call's `MessageCommitted` text event
/// is only emitted once the response stream ends, so a tool that finishes
/// later persists its result *behind* that text. Replayed naively, the
/// text-only assistant message separates the call from its result.
#[test]
fn a_streamed_assistant_text_never_separates_a_tool_call_from_its_result() {
    let events = vec![
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "how many tabs?".to_string(),
        }),
        tool_call_request("call-1"),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "Let me check.".to_string(),
        }),
        tool_call_finished("call-1"),
    ];

    let messages = rig_messages_from_horizon_events(&events);

    assert_pairing_valid(&messages);
    // The call and the text were one provider response and replay as one
    // assistant message, exactly as live history holds them.
    assert_eq!(messages.len(), 3);
    let RigMessage::Assistant { content, .. } = &messages[1] else {
        panic!("expected an assistant message, got {:?}", messages[1]);
    };
    let items = content.iter().collect::<Vec<_>>();
    assert_eq!(items.len(), 2);
    assert!(matches!(items[0], AssistantContent::ToolCall(call) if call.id == "call-1"));
    assert!(matches!(items[1], AssistantContent::Text(text) if text.text == "Let me check."));
}

/// A tool result whose call no assistant message ever announced is dropped
/// rather than replayed -- and nothing is synthesized in its place, because
/// the provider never saw a call to answer.
#[test]
fn an_orphaned_tool_result_is_dropped_when_history_is_rebuilt() {
    let events = vec![
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "how many tabs?".to_string(),
        }),
        tool_call_finished("call-lost"),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "There is one tab.".to_string(),
        }),
    ];

    let messages = rig_messages_from_horizon_events(&events);

    assert_pairing_valid(&messages);
    assert_eq!(messages.len(), 2);
    assert!(matches!(&messages[0], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::Text(text)
            if text.text == "how many tabs?")));
    assert!(matches!(&messages[1], RigMessage::Assistant { .. }));
}

/// The inverse direction: a call announced with no result anywhere behind it
/// (a turn cut short whose cancelled-result fixup hasn't reached the DuckDB
/// projection the rebuild reads) is closed with the same cancelled result
/// the live cancel path appends -- placed where the real result would have
/// gone, not at the tail.
#[test]
fn an_unanswered_tool_call_is_closed_with_a_cancelled_result_on_rebuild() {
    let events = vec![
        tool_call_request("call-1"),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "never mind".to_string(),
        }),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "ok".to_string(),
        }),
    ];

    let messages = rig_messages_from_horizon_events(&events);

    assert_pairing_valid(&messages);
    assert_eq!(messages.len(), 4);
    assert!(matches!(&messages[1], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == "call-1"
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("cancelled")))));
    assert!(matches!(&messages[2], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::Text(text)
            if text.text == "never mind")));
}

/// Replaying an out-of-order parallel batch keeps every result paired: the
/// batch's calls fold into one assistant message and each result still finds
/// an announcement behind it.
#[test]
fn an_out_of_order_parallel_tool_batch_replays_paired() {
    let events = vec![
        tool_call_request("call-a"),
        tool_call_request("call-b"),
        tool_call_finished("call-b"),
        tool_call_finished("call-a"),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "done".to_string(),
        }),
    ];

    let messages = rig_messages_from_horizon_events(&events);

    assert_pairing_valid(&messages);
    assert_eq!(messages.len(), 4);
}

/// The repair must be a fixed point: running it over an already-repaired
/// sequence changes nothing, so a session resumed twice sees the same
/// history both times.
#[test]
fn rebuilt_history_pairing_repair_is_idempotent() {
    let events = vec![
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "how many tabs?".to_string(),
        }),
        tool_call_finished("call-lost"),
        tool_call_request("call-1"),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            text: "Let me check.".to_string(),
        }),
        tool_call_finished("call-1"),
        tool_call_request("call-2"),
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text: "stop".to_string(),
        }),
    ];

    let messages = rig_messages_from_horizon_events(&events);
    assert_pairing_valid(&messages);

    assert_eq!(repair_replayed_message_pairing(messages.clone()), messages);
}

#[test]
fn appends_cancelled_tool_results_after_assistant_tool_call_message() {
    let tool_call = rig_workspace_snapshot_call();
    let call_id = ToolCallId(tool_call.id.clone());
    let mut history = vec![
        RigMessage::user("snapshot please"),
        RigMessage::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(tool_call)),
        },
    ];

    append_cancelled_tool_results_to_history(&mut history, std::slice::from_ref(&call_id));

    // The assistant tool_calls message must be followed by one tool-result
    // message per cancelled call, or the next API request is rejected.
    assert_eq!(history.len(), 3);
    assert!(matches!(&history[2], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == call_id.0
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("cancelled")))));
}

#[test]
fn cancel_without_tool_calls_appends_no_history_tool_results() {
    let mut history = vec![
        RigMessage::user("hello"),
        RigMessage::assistant("partial answer"),
    ];

    append_cancelled_tool_results_to_history(&mut history, &[]);

    assert_eq!(history.len(), 2);
    assert!(matches!(&history[1], RigMessage::Assistant { content, .. }
        if matches!(content.first_ref(), AssistantContent::Text(text)
            if text.text == "partial answer")));
}

#[test]
fn cancelled_partial_assistant_message_keeps_streamed_text_and_tool_calls() {
    let message =
        partial_assistant_message(None, "partial text", vec![rig_workspace_snapshot_call()]);

    let RigMessage::Assistant { content, .. } = message else {
        panic!("expected an assistant message");
    };
    let items = content.into_iter().collect::<Vec<_>>();
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], AssistantContent::Text(text) if text.text == "partial text"));
    assert!(matches!(&items[1], AssistantContent::ToolCall(call)
        if call.id == "rig-workspace-snapshot-1"));
}

// --- Double-encoded tool-call arguments -------------------------------
//
// The 2026-07-27 session death (session `12fd8d14`): MiniMax-M3 emitted a
// tool call whose `arguments` was a JSON *string* holding the JSON object.
// Stored verbatim, it made every later request in that session fail with a
// provider 400 (`'str object' has no attribute 'items'`) from the serving
// layer's chat template, which iterates `arguments` as a mapping.

/// The `HostTools` seam is irrelevant to these tests -- they dispatch
/// `fs.read`, which this crate implements itself -- so nothing is handled
/// here and every call falls through to `tools::fs`.
struct NoHostTools;

impl crate::tools::HostTools for NoHostTools {
    fn execute_auto(
        &self,
        _tool_id: &str,
        _input: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        None
    }
}

fn streamed_tool_call(arguments: serde_json::Value) -> ToolCall {
    ToolCall::new(
        "call-1".to_string(),
        ToolFunction::new("fs.read".to_string(), arguments),
    )
}

/// Dispatches a streamed tool call the way `rig_openai_turn_streaming`
/// does (repair, then `rig_tool_call_request`) and returns the tool's
/// output.
fn dispatch_repaired_tool_call(root: &std::path::Path, mut call: ToolCall) -> serde_json::Value {
    super::completion::repair_double_encoded_tool_arguments(&mut call.function.arguments);
    let request = rig_tool_call_request(call);
    let execution = crate::tools::execute_agent_tool(
        &NoHostTools,
        &crate::tools::ToolSessionState::new(root.to_path_buf()),
        SessionId::new(),
        &request,
    );
    let crate::tools::Execution::Auto(events) = execution else {
        panic!("fs.read must execute synchronously");
    };
    events
        .into_iter()
        .find_map(|event| match event {
            Event::ToolCallFinished(result) => Some(result.output.0),
            _ => None,
        })
        .expect("expected a ToolCallFinished event")
}

fn temp_read_fixture(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("horizon-rig-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("create temp dir");
    let root = root.canonicalize().expect("canonicalize temp dir");
    let file = root.join("file.txt");
    std::fs::write(&file, "hello").expect("write fixture file");
    (root, file)
}

fn history_tool_call_arguments(message: &RigMessage) -> Vec<serde_json::Value> {
    let RigMessage::Assistant { content, .. } = message else {
        panic!("expected an assistant message");
    };
    let serialized = serde_json::to_value(content).expect("serialize assistant content");
    serialized
        .as_array()
        .expect("assistant content serializes as a sequence")
        .iter()
        .filter_map(|item| item.get("function")?.get("arguments").cloned())
        .collect()
}

#[test]
fn double_encoded_tool_arguments_are_repaired_for_dispatch_and_history() {
    let (root, file) = temp_read_fixture("double-encoded");
    let encoded = serde_json::json!({ "path": file.display().to_string() }).to_string();
    let call = streamed_tool_call(serde_json::Value::String(encoded));

    // Dispatch: the repaired object executes instead of failing input
    // validation.
    let output = dispatch_repaired_tool_call(&root, call.clone());
    assert_ne!(output["is_error"], serde_json::json!(true), "{output}");
    assert!(output["content"].as_str().unwrap().contains("hello"));

    // History: the assistant message carries the decoded object, not the
    // string the provider emitted.
    let message = partial_assistant_message(None, "", vec![call]);
    assert_eq!(
        history_tool_call_arguments(&message),
        vec![serde_json::json!({ "path": file.display().to_string() })]
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn plain_object_tool_arguments_are_left_byte_identical() {
    let (root, file) = temp_read_fixture("plain-object");
    let arguments = serde_json::json!({ "path": file.display().to_string() });
    let call = streamed_tool_call(arguments.clone());

    let mut repaired = call.clone();
    super::completion::repair_double_encoded_tool_arguments(&mut repaired.function.arguments);
    assert_eq!(repaired, call, "a well-formed call must pass through");

    let output = dispatch_repaired_tool_call(&root, call.clone());
    assert_ne!(output["is_error"], serde_json::json!(true), "{output}");
    assert!(output["content"].as_str().unwrap().contains("hello"));

    let message = partial_assistant_message(None, "", vec![call]);
    assert_eq!(history_tool_call_arguments(&message), vec![arguments]);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn unparseable_string_tool_arguments_error_the_tool_but_replay_as_an_empty_object() {
    let (root, _file) = temp_read_fixture("unparseable");
    let call = streamed_tool_call(serde_json::Value::String("not json at all".to_string()));

    // The tool reports the malformed input to the model, which is the only
    // feedback that should reach it.
    let output = dispatch_repaired_tool_call(&root, call.clone());
    assert_eq!(output["is_error"], serde_json::json!(true), "{output}");

    // The provider-facing replay is still a mapping, so the next request in
    // this session survives its chat template.
    let message = partial_assistant_message(None, "", vec![call.clone()]);
    assert_eq!(
        history_tool_call_arguments(&message),
        vec![serde_json::json!({})]
    );

    // Same for the streaming aggregation path, which assembles history from
    // rig's own `stream.choice` rather than the streamed calls.
    let mut content = OneOrMany::one(AssistantContent::ToolCall(call));
    super::completion::make_tool_call_arguments_replay_safe(&mut content);
    assert_eq!(
        history_tool_call_arguments(&RigMessage::Assistant { id: None, content }),
        vec![serde_json::json!({})]
    );

    std::fs::remove_dir_all(&root).ok();
}

/// The history-reload path (`load_rig_history` ->
/// `rig_messages_from_horizon_events`) normalizes the same way, so a
/// session whose events were persisted before this repair existed cannot
/// re-poison itself on resume.
#[test]
fn replayed_tool_call_events_are_normalized_when_history_is_rebuilt() {
    let events = vec![
        Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "fs.read".to_string(),
            input: serde_json::Value::String("{\"path\":\"/tmp/x\"}".to_string()).into(),
            occurrence_id: None,
        }),
        Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId("call-2".to_string()),
            tool_id: "fs.read".to_string(),
            input: serde_json::Value::String("not json at all".to_string()).into(),
            occurrence_id: None,
        }),
    ];

    let messages = rig_messages_from_horizon_events(&events);

    // Both calls came from one provider response, so the rebuild folds them
    // back into a single assistant message (see
    // `mapping::repair_replayed_message_pairing`); neither was ever
    // answered, so each also gets a cancelled result behind it.
    assert_eq!(
        history_tool_call_arguments(&messages[0]),
        vec![
            serde_json::json!({ "path": "/tmp/x" }),
            serde_json::json!({})
        ]
    );
    assert_eq!(messages.len(), 3);
    assert_pairing_valid(&messages);
}

// --- Turn-loop guards -------------------------------------------------

#[test]
fn turn_loop_guard_iteration_cap_triggers_at_boundary() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);

    for _ in 0..TEST_ITERATION_CAP {
        assert_eq!(guard.record_tool_turn(), None);
    }

    assert_eq!(
        guard.record_tool_turn(),
        Some(GuardHalt::IterationCapExceeded)
    );
}

#[test]
fn turn_loop_guard_iteration_cap_resets_on_reset() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    for _ in 0..TEST_ITERATION_CAP {
        guard.record_tool_turn();
    }

    guard.reset();

    for _ in 0..TEST_ITERATION_CAP {
        assert_eq!(guard.record_tool_turn(), None);
    }
    assert_eq!(
        guard.record_tool_turn(),
        Some(GuardHalt::IterationCapExceeded)
    );
}

#[test]
fn turn_loop_guard_fingerprint_triggers_at_the_window_boundary() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    let fingerprint = 0xABCDu64;

    for _ in 0..TEST_DOOM_LOOP_WINDOW - 1 {
        assert_eq!(guard.record_fingerprint(fingerprint), None);
    }
    assert_eq!(
        guard.record_fingerprint(fingerprint),
        Some(GuardHalt::DoomLoopDetected)
    );
}

#[test]
fn turn_loop_guard_fingerprint_does_not_trigger_on_varying_fingerprints() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);

    for fingerprint in 0..(TEST_DOOM_LOOP_WINDOW as u64 * 2) {
        assert_eq!(guard.record_fingerprint(fingerprint), None);
    }
}

#[test]
fn turn_loop_guard_reset_clears_fingerprint_window() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    let fingerprint = 42u64;
    for _ in 0..TEST_DOOM_LOOP_WINDOW - 1 {
        guard.record_fingerprint(fingerprint);
    }

    guard.reset();

    // If the window had survived the reset, the next identical fingerprint
    // at the boundary would immediately trip the guard; it must not.
    for _ in 0..TEST_DOOM_LOOP_WINDOW - 1 {
        assert_eq!(guard.record_fingerprint(fingerprint), None);
    }
    assert_eq!(
        guard.record_fingerprint(fingerprint),
        Some(GuardHalt::DoomLoopDetected)
    );
}

/// The truncation auto-continue cap: `record_truncation_continue` returns
/// `true` for the first three calls and `false` on the fourth.
#[test]
fn turn_loop_guard_truncation_continue_caps_at_three() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);

    assert!(
        guard.record_truncation_continue(),
        "first truncation continue"
    );
    assert!(guard.record_truncation_continue(), "second");
    assert!(guard.record_truncation_continue(), "third");
    assert!(
        !guard.record_truncation_continue(),
        "fourth truncation must stop auto-continuing"
    );
}

/// The truncation counter resets alongside the rest of the guard — a
/// fresh interaction (`guard.reset()`) clears the streak.
#[test]
fn turn_loop_guard_truncation_counter_resets_on_reset() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);

    guard.record_truncation_continue();
    guard.record_truncation_continue();

    guard.reset();

    assert!(guard.record_truncation_continue(), "first after reset");
    assert!(guard.record_truncation_continue(), "second after reset");
    assert!(guard.record_truncation_continue(), "third after reset");
    assert!(
        !guard.record_truncation_continue(),
        "fourth after reset must stop"
    );
}

/// `reset_truncation_counter` alone (without resetting the whole guard)
/// breaks the streak — called when a turn completes without truncation.
#[test]
fn turn_loop_guard_reset_truncation_counter_breaks_the_streak() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);

    guard.record_truncation_continue();
    guard.record_truncation_continue();

    guard.reset_truncation_counter();

    assert!(guard.record_truncation_continue(), "first after reset");
    assert!(guard.record_truncation_continue(), "second after reset");
    assert!(guard.record_truncation_continue(), "third after reset");
    assert!(
        !guard.record_truncation_continue(),
        "fourth after reset must stop"
    );
}

#[tokio::test]
async fn halt_turn_loop_stashes_real_result_and_cancels_only_other_pending_calls() {
    // Assistant turn requested two tool calls: A (whose real result just
    // arrived and tripped the guard) and B (still outstanding).
    let call_a = rig_workspace_snapshot_call();
    let call_b = ToolCall::new(
        "call-b".to_string(),
        ToolFunction::new("fs.read".to_string(), serde_json::json!({ "path": "/x" })),
    );
    let id_a = ToolCallId(call_a.id.clone());
    let id_b = ToolCallId(call_b.id.clone());
    let mut history = vec![
        RigMessage::user("snapshot please"),
        RigMessage::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::ToolCall(call_a),
                AssistantContent::ToolCall(call_b),
            ])
            .expect("assistant content"),
        },
    ];
    // The session loop removes the arrived call from pending (to look up
    // its descriptor) before halting; only B is still pending here.
    let mut pending: HashMap<ToolCallId, ToolCallDescriptor> = HashMap::from([(
        id_b.clone(),
        ToolCallDescriptor {
            tool_id: "fs.read".to_string(),
            args: serde_json::json!({ "path": "/x" }),
        },
    )]);
    let mut cancelled: HashSet<ToolCallId> = HashSet::new();
    let mut pending_halt_result: Option<ToolCallResult> = None;
    let arrived = ToolCallResult::new(id_a.clone(), None, serde_json::json!({ "tab_count": 2 }));
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    for _ in 0..=TEST_ITERATION_CAP {
        guard.record_tool_turn();
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    // Unused by this test: `role` is `None`, so `halt_turn_loop` never
    // reaches the code that would actually run a turn on these.
    let (_commands_tx, mut commands) = tokio::sync::mpsc::unbounded_channel::<Command>();
    let mut inbox: VecDeque<Command> = VecDeque::new();
    let config = RigAgentConfig::default();
    let environment = crate::prompt::SessionEnvironment::for_workspace_root(None);
    let extra_sections: Vec<String> = Vec::new();

    halt_turn_loop(
        GuardHalt::IterationCapExceeded,
        &mut guard,
        &mut commands,
        &mut inbox,
        &config,
        &environment,
        &extra_sections,
        None,
        &tx,
        &mut pending_halt_result,
        &mut history,
        &mut ClearingState::disabled(),
        &arrived,
        &mut pending,
        &mut cancelled,
    )
    .await;

    // The arrived result is *not* folded into history here -- it's stashed
    // for `Command::ContinueTurn`/a later `Command::UserMessage` to fold in
    // exactly like an ordinary tool-driven turn's last-landed result (see
    // `halt_turn_loop`'s doc comment). Only B's synthesized cancellation is
    // appended immediately, since it never gets a second chance to land.
    assert_eq!(
        pending_halt_result,
        Some(arrived.clone()),
        "the real, already-executed result must be stashed for Continue/a new user message"
    );
    assert_eq!(history.len(), 3);
    assert!(matches!(&history[2], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == id_b.0
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("cancelled")))));

    assert!(pending.is_empty());
    assert!(cancelled.contains(&id_b));
    assert!(
        !cancelled.contains(&id_a),
        "the real, already-executed result must not be marked cancelled"
    );

    match recv(&rx).event {
        Event::ToolCallFinished(result) => {
            assert_eq!(
                result.call_id, id_b,
                "no contradictory cancelled ToolCallFinished for the arrived result"
            );
            assert_eq!(result.output["cancelled"], true);
        }
        other => panic!("expected ToolCallFinished, got {other:?}"),
    }
    assert_eq!(
        recv(&rx).event,
        Event::TurnEnded(TurnEndReason::HaltedByIterationCap)
    );
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
    assert!(
        rx.try_recv().is_err(),
        "halt must emit exactly one cancelled finish for B, TurnEnded(HaltedByIterationCap), \
         and WaitingForUser -- no Error event"
    );

    // The guard was reset: a fresh allowance of tool turns is available.
    for _ in 0..TEST_ITERATION_CAP {
        assert_eq!(guard.record_tool_turn(), None);
    }
}

/// Unit coverage for `Event::TurnEnded`'s fourth stop reason (`Failed`) --
/// the one path the other three (`Completed`/`Cancelled`/`Halted`) don't
/// exercise through a live session handle above, since triggering it for
/// real needs the rig OpenAI completion call to fail (`complete_rig_turn`'s
/// `Err` branch), not something worth wiring a real/fake network call for
/// here. `apply_turn_outcome` is where every rig turn's `TurnCompletion`
/// funnels through regardless of *why* it produced `failed: true`, so
/// driving it directly with that flag set proves the wiring
/// (`TurnEnded(Failed)` then `WaitingForUser`, nothing else) without needing
/// to reach the network-dependent code that sets the flag in production.
#[test]
fn apply_turn_outcome_emits_turn_ended_failed_for_a_failed_provider_request() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut rig_history = Vec::new();
    let mut pending_tool_calls = HashMap::new();
    let mut cancelled_call_ids = HashSet::new();

    apply_turn_outcome(
        TurnCompletion {
            failed: true,
            ..TurnCompletion::default()
        },
        &tx,
        &mut rig_history,
        &mut pending_tool_calls,
        &mut cancelled_call_ids,
    );

    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Failed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
    assert!(
        rx.try_recv().is_err(),
        "a failed turn must emit exactly TurnEnded(Failed) then WaitingForUser"
    );
}

fn start_fallback_rig_session() -> (
    crossbeam_channel::Sender<Command>,
    crossbeam_channel::Receiver<ProviderEvent>,
) {
    start_fallback_rig_session_with_config(RigAgentConfig {
        openai_enabled: false,
        model: "unused-in-fallback-mode".to_string(),
        ..Default::default()
    })
}

fn start_fallback_rig_session_with_config(
    config: RigAgentConfig,
) -> (
    crossbeam_channel::Sender<Command>,
    crossbeam_channel::Receiver<ProviderEvent>,
) {
    start_fallback_rig_session_with_role(config, None)
}

/// Like [`start_fallback_rig_session_with_config`], but also resolves
/// `role_id` through the real `Provider::start_session` -- the entry point
/// `role_adjusted_config` runs on, so a role's overrides (iteration cap,
/// `summarize_on_cap`) actually apply to the spawned session's turn loop.
fn start_fallback_rig_session_with_role(
    config: RigAgentConfig,
    role_id: Option<RoleId>,
) -> (
    crossbeam_channel::Sender<Command>,
    crossbeam_channel::Receiver<ProviderEvent>,
) {
    start_fallback_rig_session_as(config, role_id, SessionId::new())
}

/// Like [`start_fallback_rig_session_with_role`], but with the session's
/// own id chosen by the caller -- what the background-`task` delivery tests
/// need, since a notification is queued *against* a session id and the
/// session loop drains it by that id.
fn start_fallback_rig_session_as(
    config: RigAgentConfig,
    role_id: Option<RoleId>,
    session_id: SessionId,
) -> (
    crossbeam_channel::Sender<Command>,
    crossbeam_channel::Receiver<ProviderEvent>,
) {
    let provider = Provider::new(
        config,
        crate::persistence::projection::duckdb::SharedDuckdbStore::unavailable(),
    );
    let handle = AgentProvider::start_session(
        &provider,
        StartSession {
            session_id,
            provider_id: AgentProvider::provider_id(&provider),
            role_id,
            workspace_root: None,
            history: Vec::new(),
        },
    );
    let tx = handle.sender();
    let rx = handle.events();

    // Drain session-startup events (Created, init message, WaitingForUser).
    for _ in 0..3 {
        recv(&rx);
    }
    (tx, rx)
}

#[test]
fn rig_session_iteration_cap_halts_tool_loop_and_session_recovers() {
    let (tx, rx) = start_fallback_rig_session();

    // "snapshot" makes the deterministic fallback request a tool call, so
    // the session has a genuinely pending call to feed results into.
    let _ = tx.send(Command::UserMessage {
        text: "snapshot please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));
    let call_id = match recv(&rx).event {
        Event::ToolCallRequested(request) => request.call_id,
        other => panic!("expected a tool call request, got {other:?}"),
    };

    // Each result asks the fallback responder (via `loop_again`) to request
    // the tool again — a self-sustaining tool loop, exactly what the cap
    // exists to stop. Distinct outputs keep doom-loop detection out of the
    // way so the iteration cap is what trips.
    for i in 0..TEST_ITERATION_CAP {
        let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
            call_id.clone(),
            None,
            serde_json::json!({ "loop_again": true, "n": i }),
        )));
        assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
        assert!(matches!(
            recv(&rx).event,
            Event::ToolCallRequested(request) if request.call_id == call_id
        ));
    }

    // The next tool-driven turn exceeds the cap: the session halts instead
    // of running it, as a pause rather than an error -- no `Event::Error`
    // at all, just `TurnEnded(HaltedByIterationCap)` then `WaitingForUser`.
    // The arrived result's REAL output is stashed, not folded into
    // rig_history yet (asserted directly in the halt_turn_loop unit test —
    // history is not observable through the session handle) and, since it
    // already finished for real app-side, no contradictory cancelled
    // ToolCallFinished may be emitted for it.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
        call_id.clone(),
        None,
        serde_json::json!({ "loop_again": true, "n": "final" }),
    )));
    assert_eq!(
        recv(&rx).event,
        Event::TurnEnded(TurnEndReason::HaltedByIterationCap)
    );
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser),
        "no cancelled ToolCallFinished may be emitted for the real, already-executed result"
    );

    // `Command::ContinueTurn` resumes without a new user message: the
    // stashed result becomes the next turn's prompt. It still carries
    // `loop_again: true`, so the fallback responder legitimately requests
    // the tool again -- proving Continue actually re-entered the turn loop
    // rather than just clearing the halt, exactly as if the guard had
    // never intervened.
    let _ = tx.send(Command::ContinueTurn);
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::ToolCallRequested(request) if request.call_id == call_id
    ));

    // Resolve that call with a plain (non-looping) result so the turn
    // completes normally, proving the resumed session is fully healthy.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
        call_id.clone(),
        None,
        serde_json::json!({ "done": true }),
    )));
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );

    // The session is still usable: a fresh user message runs a normal turn.
    let _ = tx.send(Command::UserMessage {
        text: "hello again".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text,
        }) if text == "hello again"
    ));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}

/// `docs/agent-explore-design.md`'s 2026-07-27 addendum, end to end through
/// the real session loop: a role that opts in
/// (`RoleDefinition::summarize_on_cap`, set for `EXPLORE_ROLE`) gets one
/// forced, tools-disabled completion when it hits its iteration cap instead
/// of halting cold -- the summary lands as an ordinary assistant message
/// right before `TurnEnded(HaltedByIterationCap)`, and nothing is left
/// stashed for `Command::ContinueTurn` to resume (the forced turn already
/// settled the halt).
#[test]
fn rig_session_forces_a_summary_when_the_explore_role_hits_its_cap() {
    let (tx, rx) = start_fallback_rig_session_with_role(
        RigAgentConfig {
            openai_enabled: false,
            model: "unused-in-fallback-mode".to_string(),
            ..Default::default()
        },
        Some(RoleId(crate::roles::EXPLORE_ROLE_ID.to_string())),
    );
    let cap = crate::roles::EXPLORE_ROLE
        .iteration_cap
        .expect("the explore role sets a tighter cap");

    let _ = tx.send(Command::UserMessage {
        text: "snapshot please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));
    let call_id = match recv(&rx).event {
        Event::ToolCallRequested(request) => request.call_id,
        other => panic!("expected a tool call request, got {other:?}"),
    };

    for i in 0..cap {
        let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
            call_id.clone(),
            None,
            serde_json::json!({ "loop_again": true, "n": i }),
        )));
        assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
        assert!(matches!(
            recv(&rx).event,
            Event::ToolCallRequested(request) if request.call_id == call_id
        ));
    }

    // The next tool-driven turn exceeds the cap. `summarize_on_cap` runs a
    // forced completion (tools disabled) with the real, already-executed
    // result folded in first -- the deterministic fallback responder never
    // sees "snapshot"/"multi tool" in the injected instruction, so it falls
    // through to its plain-text reply, which becomes the report.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
        call_id.clone(),
        None,
        serde_json::json!({ "loop_again": true, "n": "final" }),
    )));
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(
        matches!(
            recv(&rx).event,
            Event::MessageCommitted(AgentMessage { role: MessageRole::Assistant, text })
                if text.contains("turn limit")
        ),
        "the forced wrap-up's summary must be committed as an ordinary assistant message"
    );
    assert_eq!(
        recv(&rx).event,
        Event::TurnEnded(TurnEndReason::HaltedByIterationCap)
    );
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );

    // Nothing was stashed -- the forced summary already resolved the halt,
    // so Continue has nothing to resume.
    let _ = tx.send(Command::ContinueTurn);
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "a halt that already ran its forced summary must leave nothing for Continue to resume"
    );
}

/// `docs/issues/002-agent-iteration-cap-halts-real-work.md`'s resolution,
/// replay-safety requirement: a session that ended halted must not
/// auto-resume once restarted/replayed. `pending_halt_result` (the state
/// `Command::ContinueTurn` consumes) is purely in-memory session-loop
/// state, never persisted and never reconstructed from `rig_history` --
/// every freshly spawned session loop starts with it `None`, regardless of
/// what the loaded history looks like, exactly as if bootstrap had replayed
/// a persisted halted turn. So a stray `Command::ContinueTurn` reaching a
/// just-started session (e.g. a UI that still shows a stale Continue
/// button right after a restart, before any new interaction) is a safe
/// no-op, not an accidental resume.
#[test]
fn continue_turn_on_a_freshly_started_session_is_a_no_op_not_an_auto_resume() {
    let (tx, rx) = start_fallback_rig_session();

    let _ = tx.send(Command::ContinueTurn);
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "Continue must be a silent no-op when nothing is halted -- in particular, a freshly \
         started/replayed session must never auto-resume"
    );

    // The session is unaffected: a normal user turn still works afterward.
    let _ = tx.send(Command::UserMessage {
        text: "hello".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text,
        }) if text == "hello"
    ));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}

#[test]
fn rig_session_drops_unsolicited_tool_result_without_running_a_turn() {
    let (tx, rx) = start_fallback_rig_session();

    // No tool call was ever requested, so this result is unsolicited: it
    // must not start a turn (which would append an orphan tool-result
    // message to rig_history) and must not advance the loop guards.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
        ToolCallId("never-requested".to_string()),
        None,
        serde_json::json!({ "ok": true }),
    )));
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "an unsolicited tool result must be dropped silently, producing no events"
    );

    // The session is unaffected: a normal user turn still works.
    let _ = tx.send(Command::UserMessage {
        text: "hello".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text,
        }) if text == "hello"
    ));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}

#[test]
fn doom_loop_does_not_trip_on_identical_outputs_with_different_args() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    let empty_matches = serde_json::json!({ "matches": [] });

    // Three distinct greps that all found nothing: identical outputs, but
    // different args — productive, non-looping calls per the design doc's
    // (tool, args, result) fingerprint.
    for pattern in ["alpha", "beta", "gamma"] {
        let fingerprint = tool_result_fingerprint(
            "fs.grep",
            &serde_json::json!({ "pattern": pattern }),
            &empty_matches,
        );
        assert_eq!(guard.record_fingerprint(fingerprint), None);
    }
}

#[test]
fn doom_loop_trips_on_identical_tool_args_output_fingerprints_at_the_window_boundary() {
    let mut guard = TurnLoopGuard::new(TEST_ITERATION_CAP, TEST_DOOM_LOOP_WINDOW);
    let args = serde_json::json!({ "pattern": "alpha" });
    let output = serde_json::json!({ "matches": [] });

    let fingerprint = tool_result_fingerprint("fs.grep", &args, &output);
    for _ in 0..TEST_DOOM_LOOP_WINDOW - 1 {
        assert_eq!(guard.record_fingerprint(fingerprint), None);
    }
    assert_eq!(
        guard.record_fingerprint(fingerprint),
        Some(GuardHalt::DoomLoopDetected)
    );
}

// --- Parallel tool-call batching ---------------------------------------
//
// Regression coverage for the production incident (session 3aef2770) where
// a single completion requesting several parallel tool calls (MiniMax
// routinely requests 4 parallel `fs.read`s) made the session loop run one
// completion per *arriving result* instead of waiting for the whole batch:
// protocol-malformed history, a burst of stray "anything else?" turns, and
// the iteration-cap guard burning N times faster than intended.

#[test]
fn fold_batched_tool_result_holds_non_last_results_and_leaves_the_last_for_the_caller() {
    let call_a = ToolCallId("call-a".to_string());
    let call_b = ToolCallId("call-b".to_string());
    let call_c = ToolCallId("call-c".to_string());
    let mut history = vec![
        RigMessage::user("multi tool please"),
        RigMessage::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::ToolCall(ToolCall::new(
                    call_a.0.clone(),
                    ToolFunction::new("fs.read".to_string(), serde_json::json!({ "path": "/a" })),
                )),
                AssistantContent::ToolCall(ToolCall::new(
                    call_b.0.clone(),
                    ToolFunction::new("fs.read".to_string(), serde_json::json!({ "path": "/b" })),
                )),
                AssistantContent::ToolCall(ToolCall::new(
                    call_c.0.clone(),
                    ToolFunction::new("fs.read".to_string(), serde_json::json!({ "path": "/c" })),
                )),
            ])
            .expect("assistant content"),
        },
    ];
    let mut pending: HashMap<ToolCallId, ToolCallDescriptor> = HashMap::from([
        (
            call_a.clone(),
            ToolCallDescriptor {
                tool_id: "fs.read".to_string(),
                args: serde_json::json!({ "path": "/a" }),
            },
        ),
        (
            call_b.clone(),
            ToolCallDescriptor {
                tool_id: "fs.read".to_string(),
                args: serde_json::json!({ "path": "/b" }),
            },
        ),
        (
            call_c.clone(),
            ToolCallDescriptor {
                tool_id: "fs.read".to_string(),
                args: serde_json::json!({ "path": "/c" }),
            },
        ),
    ]);

    // First of three: two more calls are still outstanding, so the result
    // is folded directly into history (in arrival order) and no turn runs.
    pending.remove(&call_a);
    let result_a =
        ToolCallResult::new(call_a.clone(), None, serde_json::json!({ "contents": "a" }));
    assert_eq!(
        fold_batched_tool_result(&mut history, &pending, &result_a),
        BatchStep::Continue
    );
    assert_eq!(history.len(), 3);

    // Second of three: same story.
    pending.remove(&call_b);
    let result_b =
        ToolCallResult::new(call_b.clone(), None, serde_json::json!({ "contents": "b" }));
    assert_eq!(
        fold_batched_tool_result(&mut history, &pending, &result_b),
        BatchStep::Continue
    );
    assert_eq!(history.len(), 4);

    // Third and last: pending is now empty, so the caller must run a turn
    // with `result_c` as the prompt message — this function deliberately
    // leaves it out of history, so the normal turn plumbing
    // (`run_cancellable_turn`/`complete_rig_turn`) appends it right before
    // the resulting assistant message.
    pending.remove(&call_c);
    let result_c =
        ToolCallResult::new(call_c.clone(), None, serde_json::json!({ "contents": "c" }));
    assert_eq!(
        fold_batched_tool_result(&mut history, &pending, &result_c),
        BatchStep::RunTurn
    );
    assert_eq!(
        history.len(),
        4,
        "the last result is left for the caller to append via the normal turn plumbing"
    );

    // The two folded-in-advance results land in arrival order, right after
    // the assistant's tool_calls message.
    assert!(matches!(&history[2], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == call_a.0
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("\"a\"")))));
    assert!(matches!(&history[3], RigMessage::User { content }
        if matches!(content.first_ref(), UserContent::ToolResult(result)
            if result.id == call_b.0
                && matches!(result.content.first_ref(), ToolResultContent::Text(text)
                    if text.text.contains("\"b\"")))));
}

#[test]
fn rig_session_batches_parallel_tool_results_into_one_follow_up_completion() {
    let (tx, rx) = start_fallback_rig_session();

    let _ = tx.send(Command::UserMessage {
        text: "multi tool please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));

    let mut call_ids = Vec::new();
    for _ in 0..MULTI_TOOL_TEST_BATCH_SIZE {
        match recv(&rx).event {
            Event::ToolCallRequested(request) => call_ids.push(request.call_id),
            other => panic!("expected a tool call request, got {other:?}"),
        }
    }
    assert_eq!(call_ids.len(), MULTI_TOOL_TEST_BATCH_SIZE);

    // Deliver all but the batch's last result: no completion may run while
    // any of the batch is still outstanding.
    for call_id in &call_ids[..call_ids.len() - 1] {
        let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
            call_id.clone(),
            None,
            serde_json::json!({ "ok": true }),
        )));
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "no completion should run while results are still outstanding"
        );
    }

    // The batch's last result completes it: exactly one follow-up
    // completion fires.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
        call_ids[call_ids.len() - 1].clone(),
        None,
        serde_json::json!({ "ok": true }),
    )));
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
    assert!(
        rx.try_recv().is_err(),
        "exactly one follow-up completion should run for the whole batch"
    );
}

#[test]
fn fresh_user_message_retires_old_tool_batch_before_new_tool_turn() {
    let (tx, rx) = start_fallback_rig_session();

    let _ = tx.send(Command::UserMessage {
        text: "multi tool please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));

    let mut old_call_ids = HashSet::new();
    for _ in 0..MULTI_TOOL_TEST_BATCH_SIZE {
        match recv(&rx).event {
            Event::ToolCallRequested(request) => {
                old_call_ids.insert(request.call_id);
            }
            other => panic!("expected an old tool call request, got {other:?}"),
        }
    }

    // Reproduce the dogfood failure: submit a fresh instruction while the
    // previous turn's tool calls are still outstanding, then have that new
    // turn request a tool of its own.
    let _ = tx.send(Command::UserMessage {
        text: "snapshot please".to_string(),
    });

    let mut cancelled_call_ids = HashSet::new();
    for _ in 0..MULTI_TOOL_TEST_BATCH_SIZE {
        match recv(&rx).event {
            Event::ToolCallFinished(result) => {
                assert_eq!(result.output["cancelled"], true);
                cancelled_call_ids.insert(result.call_id);
            }
            other => panic!("expected an old call cancellation, got {other:?}"),
        }
    }
    assert_eq!(cancelled_call_ids, old_call_ids);
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Cancelled));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::Cancelled)
    );
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text,
        }) if text == "snapshot please"
    ));
    let new_call_id = match recv(&rx).event {
        Event::ToolCallRequested(request) => request.call_id,
        other => panic!("expected the new turn's tool call, got {other:?}"),
    };
    assert!(!old_call_ids.contains(&new_call_id));

    // Every tool implementation can finish late. Those old results must be
    // ignored rather than rejoining the new turn's normalized batch.
    for call_id in old_call_ids {
        let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
            call_id,
            None,
            serde_json::json!({ "late": true }),
        )));
    }
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "late results from the retired turn must be silent"
    );

    let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
        new_call_id,
        None,
        serde_json::json!({ "ok": true }),
    )));
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}

#[test]
fn rig_session_cancel_mid_batch_drops_remaining_results_and_recovers() {
    let (tx, rx) = start_fallback_rig_session();

    let _ = tx.send(Command::UserMessage {
        text: "multi tool please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));

    let mut call_ids = Vec::new();
    for _ in 0..MULTI_TOOL_TEST_BATCH_SIZE {
        match recv(&rx).event {
            Event::ToolCallRequested(request) => call_ids.push(request.call_id),
            other => panic!("expected a tool call request, got {other:?}"),
        }
    }

    // Only the first of the batch resolves before the user cancels.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
        call_ids[0].clone(),
        None,
        serde_json::json!({ "ok": true }),
    )));
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "no completion should run with results still outstanding"
    );

    let _ = tx.send(Command::Cancel { request_id: None });
    let remaining = &call_ids[1..];
    let mut cancelled_ids: HashSet<ToolCallId> = HashSet::new();
    for _ in remaining {
        match recv(&rx).event {
            Event::ToolCallFinished(result) => {
                assert_eq!(result.output["cancelled"], true);
                cancelled_ids.insert(result.call_id);
            }
            other => panic!("expected a cancelled ToolCallFinished, got {other:?}"),
        }
    }
    let remaining_ids: HashSet<ToolCallId> = remaining.iter().cloned().collect();
    assert_eq!(cancelled_ids, remaining_ids);
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Cancelled));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::Cancelled)
    );
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );

    // The real results for the cancelled calls arrive late: accepted and
    // dropped silently — no turn restart, nothing observable on the wire.
    for call_id in remaining {
        let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
            call_id.clone(),
            None,
            serde_json::json!({ "ok": true }),
        )));
    }
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "late results for cancelled calls must drop silently"
    );

    // The session recovers: a fresh user message runs a normal turn.
    let _ = tx.send(Command::UserMessage {
        text: "hello again".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            text,
        }) if text == "hello again"
    ));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}

#[test]
fn rig_session_iteration_cap_counts_one_tool_turn_per_batch() {
    // A large `doom_loop_window` keeps doom-loop detection out of the way:
    // the deterministic multi-tool fallback repeats the same (tool, args)
    // pairs batch after batch, which would otherwise trip doom-loop
    // detection first and mask what this test is actually checking.
    let (tx, rx) = start_fallback_rig_session_with_config(RigAgentConfig {
        openai_enabled: false,
        model: "unused-in-fallback-mode".to_string(),
        iteration_cap: 2,
        doom_loop_window: 1000,
        ..Default::default()
    });

    let _ = tx.send(Command::UserMessage {
        text: "multi tool please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));

    // Two consecutive batches (2 tool-driven completions total) must both
    // succeed under `iteration_cap: 2`. If the guard counted per *result*
    // instead of per *batch*, the very first 4-call batch would already
    // exceed the cap by its 3rd result, well before that batch even
    // finishes.
    for _ in 0..2 {
        let mut call_ids = Vec::new();
        for _ in 0..MULTI_TOOL_TEST_BATCH_SIZE {
            match recv(&rx).event {
                Event::ToolCallRequested(request) => call_ids.push(request.call_id),
                other => panic!("expected a tool call request, got {other:?}"),
            }
        }
        for (index, call_id) in call_ids.iter().enumerate() {
            let is_last = index == call_ids.len() - 1;
            let output = if is_last {
                serde_json::json!({ "loop_again_batch": MULTI_TOOL_TEST_BATCH_SIZE })
            } else {
                serde_json::json!({ "index": index })
            };
            let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
                call_id.clone(),
                None,
                output,
            )));
            if is_last {
                assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
            } else {
                assert!(
                    rx.recv_timeout(std::time::Duration::from_millis(200))
                        .is_err(),
                    "no completion should run while results are still outstanding"
                );
            }
        }
    }

    // The 3rd tool-driven completion exceeds the cap: it must halt instead
    // of running.
    let mut call_ids = Vec::new();
    for _ in 0..MULTI_TOOL_TEST_BATCH_SIZE {
        match recv(&rx).event {
            Event::ToolCallRequested(request) => call_ids.push(request.call_id),
            other => panic!("expected a tool call request, got {other:?}"),
        }
    }
    for (index, call_id) in call_ids.iter().enumerate() {
        let is_last = index == call_ids.len() - 1;
        let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
            call_id.clone(),
            None,
            serde_json::json!({ "index": index }),
        )));
        if !is_last {
            assert!(
                rx.recv_timeout(std::time::Duration::from_millis(200))
                    .is_err(),
                "no completion should run while results are still outstanding"
            );
        }
    }
    assert_eq!(
        recv(&rx).event,
        Event::TurnEnded(TurnEndReason::HaltedByIterationCap),
        "a guard halt is a pause, not an Event::Error"
    );
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}

// --- rig_tool_definitions' allowed_tool_ids extension point ----------------
//
// `docs/research/agent-prompting.md` Part 2.5: a back-compatible allowlist
// for which tools get advertised to the provider, with `None` reproducing
// today's "every tool in the catalog" behavior exactly.

#[test]
fn rig_tool_definitions_with_no_allow_list_returns_every_catalog_tool() {
    // `web_search` is environment-gated on `EXA_API_KEY` (see
    // `rig_tool_definitions`); set it so this test's "every catalog tool"
    // assertion is independent of the host's environment.
    std::env::set_var(crate::config::EXA_API_KEY_VAR, "test-key");
    let all = crate::tools::definitions();

    let definitions = rig_tool_definitions(None);

    assert_eq!(definitions.len(), all.len());
    for definition in &all {
        assert!(
            definitions.iter().any(|d| d.name == definition.id),
            "expected `{}` to be present with no allow list",
            definition.id
        );
    }
}

#[test]
fn rig_tool_definitions_with_an_allow_list_is_restricted_to_it() {
    let allowed = vec!["fs.read".to_string(), "fs.glob".to_string()];

    let definitions = rig_tool_definitions(Some(&allowed));

    assert_eq!(definitions.len(), 2);
    let names: HashSet<&str> = definitions.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        HashSet::from(["fs.read", "fs.glob"]),
        "only the allow-listed tool ids should be advertised"
    );
}

#[test]
fn rig_tool_definitions_with_an_empty_allow_list_returns_no_tools() {
    let allowed: Vec<String> = Vec::new();

    let definitions = rig_tool_definitions(Some(&allowed));

    assert!(definitions.is_empty());
}

// --- web_search advertise gating on EXA_API_KEY -------------------------
//
// `web_search` is the one tool whose adapter cannot run without a secret
// in the process environment. Advertising it when the key is absent only buys
// a "not configured" error round, so `rig_tool_definitions` drops it unless
// `EXA_API_KEY` is set. `web_fetch` needs no key and stays advertised.

#[test]
fn web_search_is_not_advertised_when_exa_api_key_is_unset() {
    std::env::remove_var(crate::config::EXA_API_KEY_VAR);

    let definitions = rig_tool_definitions(None);
    let names: HashSet<&str> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    assert!(
        !names.contains("web_search"),
        "web_search must not be advertised when EXA_API_KEY is unset: {names:?}"
    );
    assert!(
        names.contains("web_fetch"),
        "web_fetch is keyless and must remain advertised: {names:?}"
    );
}

#[test]
fn web_search_is_advertised_when_exa_api_key_is_set() {
    std::env::set_var(crate::config::EXA_API_KEY_VAR, "test-key");

    let definitions = rig_tool_definitions(None);
    let names: HashSet<&str> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    assert!(
        names.contains("web_search"),
        "web_search must be advertised when EXA_API_KEY is set: {names:?}"
    );
}

// --- role_adjusted_config: a role narrows the process-wide RigAgentConfig -

#[test]
fn config_role_start_session_advertises_only_its_three_allowed_tools() {
    let provider = Provider::new(
        RigAgentConfig {
            openai_enabled: false,
            ..Default::default()
        },
        crate::persistence::projection::duckdb::SharedDuckdbStore::unavailable(),
    );

    let handle = AgentProvider::start_session(
        &provider,
        StartSession {
            session_id: SessionId::new(),
            provider_id: AgentProvider::provider_id(&provider),
            role_id: Some(RoleId("config".to_string())),
            workspace_root: None,
            history: Vec::new(),
        },
    );

    // Drain session-startup events (Created, init message, WaitingForUser) --
    // this session never receives a completion request, so proving the
    // role took effect has to happen through `rig_tool_definitions` /
    // `role_adjusted_config` directly (below); this call just proves
    // `start_session` accepts a role without erroring.
    let rx = handle.events();
    for _ in 0..3 {
        recv(&rx);
    }
}

#[test]
fn role_adjusted_config_restricts_allowed_tool_ids_to_the_roles_list() {
    let base = RigAgentConfig::default();
    let role = resolve(&RoleId("config".to_string())).expect("config role must resolve");

    let config = role_adjusted_config(&base, Some(role));

    let allowed = config
        .allowed_tool_ids
        .expect("config role must set an allow list");
    assert_eq!(allowed, vec!["skill.read", "config.read", "config.write"]);
    let definitions = rig_tool_definitions(Some(&allowed));
    assert_eq!(definitions.len(), 3);
}

/// `docs/agent-explore-design.md`'s first test requirement, at the point
/// the restriction actually reaches the model: an exploration session
/// advertises exactly `fs.read`/`fs.grep`/`fs.glob` and nothing else --
/// notably not `task` itself, which is what makes recursion
/// impossible to express rather than merely discouraged.
#[test]
fn an_exploration_session_advertises_exactly_the_read_only_toolset() {
    let base = RigAgentConfig::default();
    let role = resolve(&RoleId(crate::roles::EXPLORE_ROLE_ID.to_string()))
        .expect("the explore role must resolve");

    let config = role_adjusted_config(&base, Some(role));

    let allowed = config
        .allowed_tool_ids
        .expect("the explore role must set an allow list");
    let advertised = rig_tool_definitions(Some(&allowed))
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert_eq!(advertised, vec!["fs.read", "fs.glob", "fs.grep"]);
    assert_eq!(
        config.iteration_cap, 25,
        "an exploration answers one question and runs under a tighter turn cap"
    );
    assert_eq!(
        config.model, base.model,
        "the exploration runs on the requester's own model (decision: no cheap-model override in v1)"
    );
}

#[test]
fn role_adjusted_config_is_unchanged_for_a_role_less_session() {
    let base = RigAgentConfig::default();

    let config = role_adjusted_config(&base, None);

    assert_eq!(config, base);
}

// --- Provider::resolved_model: session-start model, ahead of any turn -----

#[test]
fn resolved_model_reports_the_base_model_for_a_role_less_session() {
    let provider = Provider::new(
        RigAgentConfig {
            openai_enabled: true,
            model: "test-model".to_string(),
            ..Default::default()
        },
        crate::persistence::projection::duckdb::SharedDuckdbStore::unavailable(),
    );

    assert_eq!(
        AgentProvider::resolved_model(&provider, None),
        Some("test-model".to_string())
    );
}

#[test]
fn resolved_model_reports_the_base_model_for_the_config_role_since_it_has_no_override() {
    // `roles::CONFIG_ROLE::model` is `None` (`config_role_uses_the_provider_
    // default_model` in `roles.rs`'s own tests) -- resolving a role that
    // doesn't override the model must fall back to the base config, not
    // report no model at all.
    let provider = Provider::new(
        RigAgentConfig {
            openai_enabled: true,
            model: "test-model".to_string(),
            ..Default::default()
        },
        crate::persistence::projection::duckdb::SharedDuckdbStore::unavailable(),
    );

    assert_eq!(
        AgentProvider::resolved_model(&provider, Some(&RoleId("config".to_string()))),
        Some("test-model".to_string())
    );
}

#[test]
fn resolved_model_is_none_in_deterministic_fallback_mode() {
    // No `OPENAI_API_KEY` (`openai_enabled: false`): every turn runs the
    // deterministic fallback responder and never emits
    // `Event::ProviderRequestSent` at all (`completion::complete_rig_turn`),
    // so reporting a model here would claim one is in play when none is.
    let provider = Provider::new(
        RigAgentConfig {
            openai_enabled: false,
            model: "test-model".to_string(),
            ..Default::default()
        },
        crate::persistence::projection::duckdb::SharedDuckdbStore::unavailable(),
    );

    assert_eq!(AgentProvider::resolved_model(&provider, None), None);
}

// --- session_environment: StartSession.workspace_root reaches the prompt --
//
// The 2026-07-19 dogfooding bug: an isolated session's system prompt
// reported the daemon process's own `cwd` (the root repository checkout) as
// its working directory, so the model tried to write files there instead of
// into its actual isolated worktree. `spawn_rig_session` builds its
// `SessionEnvironment` via `session_environment` below, so these tests
// exercise that exact seam directly rather than spinning up a real session
// thread (whose environment isn't otherwise observable without a live
// OpenAI-shaped request -- see `complete_rig_turn`'s doc comment on why the
// deterministic fallback path never builds a system prompt at all).

#[test]
fn session_environment_uses_the_start_session_workspace_root_for_an_isolated_session() {
    let isolated_root = std::env::temp_dir().join(format!(
        "horizon-agent-isolated-worktree-{}",
        uuid::Uuid::new_v4()
    ));
    let request = StartSession {
        session_id: SessionId::new(),
        provider_id: ProviderId("builtin.agent.rig".to_string()),
        role_id: None,
        workspace_root: Some(isolated_root.clone()),
        history: Vec::new(),
    };

    let environment = session_environment(&request);

    assert_eq!(environment.cwd, isolated_root);
    assert_ne!(
        environment.cwd,
        std::env::current_dir().unwrap(),
        "an isolated session's environment must never fall back to the daemon's own cwd"
    );
}

#[test]
fn session_environment_falls_back_to_process_cwd_when_no_workspace_root_is_known() {
    let request = StartSession {
        session_id: SessionId::new(),
        provider_id: ProviderId("builtin.agent.rig".to_string()),
        role_id: None,
        workspace_root: None,
        history: Vec::new(),
    };

    let environment = session_environment(&request);

    assert_eq!(environment.cwd, std::env::current_dir().unwrap());
}

// --- session_extra_sections: role/skills/repository-instructions ordering -

fn git_repo_with_agents_md(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "horizon-agent-rig-session-extra-sections-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join("AGENTS.md"), "REPO_MARKER").unwrap();
    dir
}

fn test_environment(cwd: std::path::PathBuf) -> crate::prompt::SessionEnvironment {
    crate::prompt::SessionEnvironment {
        cwd,
        os: "linux",
        git_repo: true,
    }
}

#[test]
fn session_extra_sections_lists_every_skill_then_repository_instructions_for_a_role_less_session() {
    let cwd = git_repo_with_agents_md("role-less");
    let environment = test_environment(cwd.clone());
    let config = RigAgentConfig::default();

    let sections = session_extra_sections(&environment, &config, None);
    let expected_instructions = crate::instructions::extra_sections(
        &environment.cwd,
        config.repository_instructions_cap_chars,
    );

    assert_eq!(
        sections.len(),
        3,
        "expected exactly a delegation-routing section, a skills section, and a \
         repository-instructions section, got: {sections:?}"
    );
    assert_eq!(
        sections[0],
        crate::prompt::DELEGATION_ROUTING_SECTION,
        "the delegation-routing block must come first for a session that has `task`"
    );
    assert!(
        sections[1].contains("horizon-config")
            && sections[1].contains("horizon-cli")
            && sections[1].contains("skill.read"),
        "a role-less session must list every available skill, got: {:?}",
        sections[1]
    );
    assert_eq!(
        sections[2..],
        expected_instructions[..],
        "a role-less session's repository instructions must match \
         instructions::extra_sections exactly"
    );

    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn session_extra_sections_lists_a_repository_skill_discovered_from_cwd_for_a_role_less_session() {
    let cwd = git_repo_with_agents_md("role-less-repo-skill");
    let skill_dir = cwd.join(".horizon").join("skills").join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-skill\ndescription: A repository skill.\n---\nBody.\n",
    )
    .unwrap();
    let environment = test_environment(cwd.clone());
    let config = RigAgentConfig::default();

    let sections = session_extra_sections(&environment, &config, None);

    // [0] is the delegation-routing block; the skills section follows it.
    assert!(
        sections[1].contains("my-skill") && sections[1].contains("A repository skill."),
        "expected the repository skill in the skills section, got: {:?}",
        sections[1]
    );

    let _ = std::fs::remove_dir_all(&cwd);
}

/// The delegation-routing block (`prompt::DELEGATION_ROUTING_SECTION`,
/// measured as cells C5/C7b in `docs/research/agent-delegation-and-
/// batching-probes-2026-07-27.md`) is worded unconditionally -- "your FIRST
/// action must be task" -- so the *inclusion* has to carry the
/// conditionality. An ordinary session gets it; an exploration session,
/// whose role allows three read-only tools and deliberately not `task`,
/// must never be told to make a call it cannot make.
#[test]
fn session_extra_sections_includes_the_delegation_block_only_when_task_is_advertised() {
    let cwd = git_repo_with_agents_md("delegation-routing");
    let environment = test_environment(cwd.clone());

    let role_less = session_extra_sections(&environment, &RigAgentConfig::default(), None);
    assert!(
        role_less.contains(&crate::prompt::DELEGATION_ROUTING_SECTION.to_string()),
        "a role-less session advertises `task`, so it must be routed to it: {role_less:?}"
    );

    let explore_role = resolve(&RoleId(crate::roles::EXPLORE_ROLE_ID.to_string()))
        .expect("the explore role must resolve");
    let explore_config = role_adjusted_config(&RigAgentConfig::default(), Some(explore_role));
    let explore_sections =
        session_extra_sections(&environment, &explore_config, Some(explore_role));
    assert!(
        !explore_sections
            .iter()
            .any(|section| section.contains("FIRST action")),
        "an exploration session has no `task` tool and must not be told to delegate first: \
         {explore_sections:?}"
    );

    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn session_extra_sections_orders_role_then_skills_and_excludes_repository_instructions_for_config_role(
) {
    let cwd = git_repo_with_agents_md("config-role");
    let environment = test_environment(cwd.clone());
    let role = resolve(&RoleId("config".to_string())).expect("config role must resolve");
    // The config a role-bearing session actually runs with -- production
    // applies this before `spawn_rig_session`, and its `allowed_tool_ids`
    // is what decides whether the delegation-routing block is included.
    let config = role_adjusted_config(&RigAgentConfig::default(), Some(role));

    let sections = session_extra_sections(&environment, &config, Some(role));

    assert_eq!(
        sections.len(),
        2,
        "expected exactly a role section and a skills section, got: {sections:?}"
    );
    assert!(
        sections[0].contains("configuration assistant"),
        "the role section must come first, got: {:?}",
        sections[0]
    );
    assert!(
        sections[1].contains("horizon-config") && sections[1].contains("skill.read"),
        "the skills section must come second, got: {:?}",
        sections[1]
    );
    assert!(
        !sections
            .iter()
            .any(|section| section.contains("REPO_MARKER")),
        "the config role must not ingest repository instructions, got: {sections:?}"
    );

    let _ = std::fs::remove_dir_all(&cwd);
}

/// `task_output` rides the same conditional seam as `task` itself
/// (`docs/agent-async-task-design.md` decision 3, "advertise it only
/// alongside `task`"): a session that cannot launch a task can never own
/// one to read, so the fetch tool must not be offered to it either.
#[test]
fn task_output_is_advertised_only_alongside_task() {
    let names = |allowed: Option<&[String]>| {
        rig_tool_definitions(allowed)
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>()
    };

    let unrestricted = names(None);
    assert!(unrestricted.iter().any(|name| name == "task"));
    assert!(unrestricted.iter().any(|name| name == "task_output"));

    let explore_role = resolve(&RoleId(crate::roles::EXPLORE_ROLE_ID.to_string()))
        .expect("the explore role must resolve");
    let explore_config = role_adjusted_config(&RigAgentConfig::default(), Some(explore_role));
    let explore = names(explore_config.allowed_tool_ids.as_deref());
    assert!(
        !explore.iter().any(|name| name == "task"),
        "a task child cannot recurse: {explore:?}"
    );
    assert!(
        !explore.iter().any(|name| name == "task_output"),
        "and it owns no tasks to read either: {explore:?}"
    );

    // An allowlist that names `task_output` but not `task` still gets
    // neither -- the rule is about ownership, not about spelling.
    let inconsistent = names(Some(&["fs.read".to_string(), "task_output".to_string()]));
    assert_eq!(inconsistent, vec!["fs.read".to_string()]);
}

// --- Background `task` delivery (docs/agent-async-task-design.md) ---------

/// Queues a finished background `task` child against `session_id` exactly
/// the way `tools::explore`'s waiter thread does, and wakes its loop.
fn deliver_task(session_id: SessionId, description: &str, report: &str) {
    let child = SessionId::new();
    crate::tools::explore::deliver_test_completion(
        session_id,
        child,
        description,
        serde_json::json!({
            "session_id": child.as_uuid().to_string(),
            "description": description,
            "report": report,
        }),
    );
}

/// Decision 2, mid-turn shape: a child finishing while the requester's turn
/// is still running is injected into that turn's *next* provider round as
/// exactly one message, and several landing between rounds coalesce into
/// that one block rather than one message each.
///
/// The injected message's role is `MessageRole::TaskNotification`, not
/// `User`: the provider is sent plain user-role text (see
/// `tools::explore::notify`'s module doc for why that shape and not a
/// synthetic tool call), but the event log must not record it as words the
/// human typed.
#[test]
fn a_mid_turn_task_completion_injects_exactly_one_coalesced_notification() {
    let session_id = SessionId::new();
    let (tx, rx) = start_fallback_rig_session_as(
        RigAgentConfig {
            openai_enabled: false,
            model: "unused-in-fallback-mode".to_string(),
            ..Default::default()
        },
        None,
        session_id,
    );

    // A turn that requests a tool call, so there is a next round to ride.
    let _ = tx.send(Command::UserMessage {
        text: "snapshot please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));
    let call_id = match recv(&rx).event {
        Event::ToolCallRequested(request) => request.call_id,
        other => panic!("expected a tool call request, got {other:?}"),
    };

    deliver_task(
        session_id,
        "map the emit sites",
        "Emitted at session.rs:1747.",
    );
    deliver_task(session_id, "list the consumers", "Consumed at frame.rs:88.");

    let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
        call_id,
        None,
        serde_json::json!({ "done": true }),
    )));
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    let notification = match recv(&rx).event {
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::TaskNotification,
            text,
        }) => text,
        other => panic!("expected one task notification message, got {other:?}"),
    };
    assert!(
        notification.contains("map the emit sites"),
        "{notification}"
    );
    assert!(
        notification.contains("list the consumers"),
        "{notification}"
    );
    assert!(
        notification.contains("Emitted at session.rs:1747."),
        "{notification}"
    );
    assert!(
        notification.contains("Consumed at frame.rs:88."),
        "{notification}"
    );

    // The round then runs with that notification as its prompt -- the
    // deterministic fallback echoes whatever it was sent, which is how this
    // test sees what actually reached the provider.
    assert!(
        matches!(
            recv(&rx).event,
            Event::MessageCommitted(AgentMessage { role: MessageRole::Assistant, text })
                if text.contains("map the emit sites") && text.contains("list the consumers")
        ),
        "the notification must be the message the provider round actually carried"
    );
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "a drained queue must not produce a second notification for the same completions"
    );
}

/// Decision 2, turn-already-ended shape: the completion starts a new turn
/// automatically, with the notification as that turn's synthetic input.
/// `Event::TurnEnded` still bounds it exactly like any other turn, so an
/// external monitor watching only `turn_ended` sees nothing unusual.
#[test]
fn a_task_completing_after_the_turn_ended_starts_an_auto_turn() {
    let session_id = SessionId::new();
    let (tx, rx) = start_fallback_rig_session_as(
        RigAgentConfig {
            openai_enabled: false,
            model: "unused-in-fallback-mode".to_string(),
            ..Default::default()
        },
        None,
        session_id,
    );

    let _ = tx.send(Command::UserMessage {
        text: "hello".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::Assistant,
            ..
        })
    ));
    assert_eq!(recv(&rx).event, Event::TurnEnded(TurnEndReason::Completed));
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );

    deliver_task(
        session_id,
        "map the emit sites",
        "Emitted at session.rs:1747.",
    );

    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::Running),
        "a completion with no round left to ride on must start one"
    );
    let notification = match recv(&rx).event {
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::TaskNotification,
            text,
        }) => text,
        other => panic!("expected a task notification message, got {other:?}"),
    };
    assert!(
        notification.contains("Emitted at session.rs:1747."),
        "{notification}"
    );
    assert!(
        matches!(
            recv(&rx).event,
            Event::MessageCommitted(AgentMessage { role: MessageRole::Assistant, text })
                if text.contains("Emitted at session.rs:1747.")
        ),
        "the auto-turn's input must be the notification itself"
    );
    assert_eq!(
        recv(&rx).event,
        Event::TurnEnded(TurnEndReason::Completed),
        "an auto-started turn is an ordinary turn: TurnEnded still bounds it"
    );
    assert_eq!(
        recv(&rx).event,
        Event::StateChanged(SessionState::WaitingForUser)
    );
}

/// Delivery waits while a tool call is still outstanding. This is exactly
/// the `WaitingForApproval` case the design doc's turn-semantics notes call
/// out: a requester parked on an approval has its batch outstanding until
/// the human resolves it, so no auto-turn may start alongside the approval
/// state machine -- the notification rides the round that the approved
/// call's result triggers instead.
#[test]
fn a_task_completion_is_deferred_while_a_tool_call_is_still_outstanding() {
    let session_id = SessionId::new();
    let (tx, rx) = start_fallback_rig_session_as(
        RigAgentConfig {
            openai_enabled: false,
            model: "unused-in-fallback-mode".to_string(),
            ..Default::default()
        },
        None,
        session_id,
    );

    let _ = tx.send(Command::UserMessage {
        text: "snapshot please".to_string(),
    });
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::User,
            ..
        })
    ));
    let call_id = match recv(&rx).event {
        Event::ToolCallRequested(request) => request.call_id,
        other => panic!("expected a tool call request, got {other:?}"),
    };

    deliver_task(
        session_id,
        "map the emit sites",
        "Emitted at session.rs:1747.",
    );
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "nothing may be delivered while the requester still owes a tool result"
    );

    // Resolving the call is what lets it through.
    let _ = tx.send(Command::ToolCallResult(ToolCallResult::new(
        call_id,
        None,
        serde_json::json!({ "done": true }),
    )));
    assert_eq!(recv(&rx).event, Event::StateChanged(SessionState::Running));
    assert!(matches!(
        recv(&rx).event,
        Event::MessageCommitted(AgentMessage {
            role: MessageRole::TaskNotification,
            ..
        })
    ));
}
