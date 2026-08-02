//! Ad-hoc append-projection micro-benchmark, kept separate from the
//! correctness suite in [`super::tests`].
//!
//! `bench_append_projection_costs` is a `#[test]` gated behind `#[ignore]`, so
//! the normal test run skips it. Run it explicitly with `--ignored --nocapture`
//! to see the timing report on stderr, e.g.
//! `cargo nextest run -p horizon-agent --run-ignored all --nocapture
//! bench_append_projection_costs` (or `cargo test -p horizon-agent
//! bench_append_projection_costs -- --ignored --nocapture`). Set
//! `HORIZON_AGENT_DUCKDB_BENCH_EVENTS=<n>` to override the default 1 000-event
//! workload.

use super::*;
use crate::contract::{
    ApprovalKind, ApprovalRequest, Event, Message, MessageDelta, MessageRole, ProviderId,
    ToolCallId, ToolCallRequest, ToolCallResult,
};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[test]
#[ignore = "micro benchmark; run with --ignored --nocapture"]
fn bench_append_projection_costs() {
    let event_count = std::env::var("HORIZON_AGENT_DUCKDB_BENCH_EVENTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1_000);

    run_append_projection_bench(
        "in-memory deltas",
        Store::open_in_memory().expect("open in-memory store"),
        event_count,
        bench_delta_event,
        None,
    );

    run_append_projection_bench(
        "in-memory mixed turn",
        Store::open_in_memory().expect("open in-memory store"),
        event_count,
        bench_mixed_turn_event,
        None,
    );

    let path = std::env::temp_dir().join(format!(
        "horizon-agent-duckdb-bench-{}.duckdb",
        Uuid::new_v4()
    ));
    run_append_projection_bench(
        "file-backed deltas",
        Store::open(&path).expect("open file-backed store"),
        event_count,
        bench_delta_event,
        Some(path),
    );
}

fn run_append_projection_bench(
    label: &str,
    store: Store,
    event_count: usize,
    event_at: impl Fn(usize) -> Event,
    cleanup_path: Option<std::path::PathBuf>,
) {
    let session_id = SessionId::new();
    let provider_id = Some(ProviderId("bench.agent".to_string()));
    let mut append_durations = Vec::with_capacity(event_count);

    let total_start = Instant::now();
    for index in 0..event_count {
        let start = Instant::now();
        store
            .append_event(AppendEvent {
                session_id,
                turn_id: Some(format!("turn-{}", index / 100)),
                provider_id: provider_id.clone(),
                role_id: None,
                event: event_at(index),
                provider_payload: None,
            })
            .expect("append bench event");
        append_durations.push(start.elapsed());
    }
    let total_append = total_start.elapsed();

    let events_query = elapsed(|| store.events_for_session(session_id).expect("events"));
    let messages_query = elapsed(|| store.messages_for_session(session_id).expect("messages"));
    let frame_query = elapsed(|| store.frame_for_session(session_id).expect("frame"));

    let stats = DurationStats::from_samples(&append_durations);
    eprintln!(
        "agent_duckdb bench: {label}; events={event_count}; append_total={}; append_avg={}; append_p50={}; append_p95={}; append_max={}; events_query={}; messages_query={}; frame_query={}",
        format_duration(total_append),
        format_duration(stats.avg),
        format_duration(stats.p50),
        format_duration(stats.p95),
        format_duration(stats.max),
        format_duration(events_query.0),
        format_duration(messages_query.0),
        format_duration(frame_query.0),
    );

    if let Some(path) = cleanup_path {
        let _ = std::fs::remove_file(path);
    }
}

fn bench_delta_event(index: usize) -> Event {
    if index.is_multiple_of(2) {
        Event::ReasoningDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: format!("reasoning delta {index}\n"),
        })
    } else {
        Event::AssistantTextDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: format!("assistant delta {index}\n"),
        })
    }
}

fn bench_mixed_turn_event(index: usize) -> Event {
    match index % 10 {
        0 => Event::MessageCommitted(Message {
            role: MessageRole::User,
            text: format!("user message {index}"),
        }),
        1 | 2 => Event::ReasoningDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: format!("thinking chunk {index}\n"),
        }),
        3..=5 => Event::AssistantTextDelta(MessageDelta {
            role: MessageRole::Assistant,
            text: format!("assistant chunk {index}\n"),
        }),
        6 => Event::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId(format!("call-{index}")),
            tool_id: "workspace.snapshot".to_string(),
            input: serde_json::json!({ "index": index }).into(),
            occurrence_id: None,
        }),
        7 => Event::ApprovalRequested(ApprovalRequest {
            call_id: ToolCallId(format!("call-{}", index - 1)),
            reason: "benchmark approval".to_string(),
            kind: ApprovalKind::Standard,
            occurrence_id: None,
        }),
        8 => Event::ToolCallFinished(ToolCallResult::new(
            ToolCallId(format!("call-{}", index - 2)),
            None,
            serde_json::json!({ "ok": true, "index": index }),
        )),
        _ => Event::MessageCommitted(Message {
            role: MessageRole::Assistant,
            text: format!("assistant final {index}"),
        }),
    }
}

fn elapsed<T>(f: impl FnOnce() -> T) -> (Duration, T) {
    let start = Instant::now();
    let value = f();
    (start.elapsed(), value)
}

struct DurationStats {
    avg: Duration,
    p50: Duration,
    p95: Duration,
    max: Duration,
}

impl DurationStats {
    fn from_samples(samples: &[Duration]) -> Self {
        let mut sorted = samples.to_vec();
        sorted.sort();
        let total = sorted.iter().copied().sum::<Duration>();
        Self {
            avg: total / sorted.len() as u32,
            p50: percentile(&sorted, 50),
            p95: percentile(&sorted, 95),
            max: *sorted.last().expect("samples"),
        }
    }
}

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    let index = ((sorted.len().saturating_sub(1)) * percentile) / 100;
    sorted[index]
}

fn format_duration(duration: Duration) -> String {
    format!("{:.3}ms", duration.as_secs_f64() * 1_000.0)
}
