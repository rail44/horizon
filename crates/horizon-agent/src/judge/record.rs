//! The shadow-mode calibration record: a lightweight, `call_id`-keyed diagnostic
//! logged for every judged (or rate-limit-skipped) boundary-crossing call, so
//! it can later be correlated against the human's real approve/deny decision
//! for that same call -- the ground-truth label the calibration protocol
//! needs (`docs/research/agent-approval-judge-prompt-2026-07-19.md`'s
//! "Calibration protocol").
//!
//! **Deliberately not the pinned enforcing-mode audit shape.**
//! `docs/agent-approval-design.md`'s "Judge design" pins the verdict riding
//! the gated tool call's own `output` JSON (the `is_error`-style additive
//! convention) -- that only works once the judge's verdict *is* the result.
//! In shadow mode the real result is produced later, by the human's
//! decision, not by the judge, so there is no tool-result JSON for the
//! verdict to ride yet. This module instead writes a standalone record
//! through the same event log/DuckDB projection pipeline every other agent
//! event goes through, so it's durable (survives a DuckDB rebuild from the
//! JSONL log, unlike a table populated only at insert time) and
//! `json_extract`-queryable the same way (`agent-inspect`'s own way of
//! reading `output_json`/`provider_payload_json` columns).
//!
//! **Why this rides `Event::ProviderRequestFinished`.** Horizon's `Event`
//! enum is closed and adding a variant for this ripples across four
//! exhaustive matches (`event_kind`, `apply_agent_event_to_frame`,
//! `project_event`, `rig_messages_from_horizon_events`) for a diagnostic
//! record that must never affect a session's live transcript, replayed
//! history, or the chat history resent to the model -- exactly the cost the
//! design doc's own "Audit" section already declined to pay once (for a
//! different reason). `ProviderRequestFinished` is a real, already-existing
//! variant that all four of those consumers already treat as a pure timing
//! marker with zero effect (confirmed by inspection, not assumed): the frame
//! fold and the rig-history/`project_event` dispatch all no-op on it, so
//! reusing it as this record's carrier is invisible everywhere except the
//! one place that matters -- `agent_events.provider_payload_json`, a
//! free-form JSON column already unconditionally stored regardless of the
//! `Event` variant. `event_kind` is overridden to a distinct string
//! (`SHADOW_VERDICT_EVENT_KIND`) rather than the derived
//! `"provider_request_finished"`, so a query can filter on it directly.
use uuid::Uuid;

use crate::contract::{Event, SessionId};
use crate::persistence::event_log::{
    Record, WriterHandle, AGENT_EVENT_LOG_SCHEMA, AGENT_EVENT_LOG_VERSION,
};

use super::{JudgeDecision, JudgeInput, JudgeVerdict};

/// The `agent_events.event_kind` label every shadow-judge record (verdict or
/// skipped) carries -- e.g. `SELECT * FROM agent_events WHERE event_kind =
/// 'judge_shadow_verdict'`, then `json_extract(provider_payload_json, '$.judge_decision')`
/// etc. over the matched rows.
pub const SHADOW_VERDICT_EVENT_KIND: &str = "judge_shadow_verdict";

/// Writes one real verdict's calibration record.
pub(super) fn write_verdict(
    writer: &WriterHandle,
    session_id: SessionId,
    input: &JudgeInput,
    model: &str,
    verdict: JudgeVerdict,
    latency_ms: u64,
) {
    let payload = serde_json::json!({
        "call_id": input.call_id,
        "tool_id": input.tool_id,
        "judge_model": model,
        "judge_decision": decision_label(verdict.decision),
        "judge_stage": verdict.stage,
        "judge_confidence": verdict.confidence,
        "latency_ms": latency_ms,
        "requested_filesystem_grants": input.requested_filesystem_grants,
    });
    append(writer, session_id, payload);
}

/// Writes a record for a call the rate limiter skipped -- never reached the
/// model at all, so there is no decision/stage/confidence/latency to
/// report, only that this `call_id` went unjudged and why.
pub(super) fn write_skipped(
    writer: &WriterHandle,
    session_id: SessionId,
    input: &JudgeInput,
    model: &str,
    reason: &str,
) {
    let payload = serde_json::json!({
        "call_id": input.call_id,
        "tool_id": input.tool_id,
        "judge_model": model,
        "skipped_reason": reason,
        "requested_filesystem_grants": input.requested_filesystem_grants,
    });
    append(writer, session_id, payload);
}

fn decision_label(decision: JudgeDecision) -> &'static str {
    match decision {
        JudgeDecision::AutoApprove => "auto_approve",
        JudgeDecision::Escalate => "escalate",
    }
}

fn append(writer: &WriterHandle, session_id: SessionId, payload: serde_json::Value) {
    let record = Record {
        schema: AGENT_EVENT_LOG_SCHEMA.to_string(),
        version: AGENT_EVENT_LOG_VERSION,
        event_id: Uuid::new_v4().to_string(),
        sequence: 0, // reassigned by the writer's background thread
        session_id,
        turn_id: None,
        provider_id: None,
        role_id: None,
        event_kind: SHADOW_VERDICT_EVENT_KIND.to_string(),
        event: Event::ProviderRequestFinished,
        provider_payload: Some(payload),
        created_at_unix_ms: unix_time_ms(),
    };
    // Best-effort: a diagnostic record that fails to enqueue (writer thread
    // gone) must never propagate as an error anywhere near the approval
    // flow -- this is calibration data, not the approval decision itself.
    let _ = writer.append(record);
}

fn unix_time_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::event_log;

    fn temp_log_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-agent-judge-record-{label}-{}.jsonl",
            Uuid::new_v4()
        ))
    }

    fn judge_input(call_id: &str) -> JudgeInput {
        JudgeInput {
            call_id: call_id.to_string(),
            tool_id: "mock.boundary_crossing".to_string(),
            args: serde_json::json!({}),
            tool_description: None,
            prior_user_messages: Vec::new(),
            requested_filesystem_grants: Vec::new(),
        }
    }

    #[test]
    fn write_verdict_lands_as_a_json_extract_queryable_provider_payload() {
        let path = temp_log_path("verdict");
        let (writer, _init_rx) = WriterHandle::open(&path);
        let session_id = SessionId::new();
        let input = judge_input("call-1");

        write_verdict(
            &writer,
            session_id,
            &input,
            "syn:small:text",
            JudgeVerdict {
                decision: JudgeDecision::Escalate,
                stage: 2,
                confidence: None,
            },
            512,
        );
        writer.flush().expect("flush");

        let report = event_log::read(&path).expect("read");
        assert_eq!(report.records.len(), 1);
        let record = &report.records[0];
        assert_eq!(record.event_kind, SHADOW_VERDICT_EVENT_KIND);
        assert_eq!(record.event, Event::ProviderRequestFinished);
        let payload = record.provider_payload.as_ref().expect("payload present");
        assert_eq!(payload["call_id"], "call-1");
        assert_eq!(payload["judge_decision"], "escalate");
        assert_eq!(payload["judge_stage"], 2);
        assert_eq!(payload["judge_confidence"], serde_json::Value::Null);
        assert_eq!(payload["latency_ms"], 512);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn write_skipped_records_the_reason_with_no_verdict_fields() {
        let path = temp_log_path("skipped");
        let (writer, _init_rx) = WriterHandle::open(&path);
        let session_id = SessionId::new();
        let input = judge_input("call-2");

        write_skipped(
            &writer,
            session_id,
            &input,
            "syn:small:text",
            "rate_limited",
        );
        writer.flush().expect("flush");

        let report = event_log::read(&path).expect("read");
        assert_eq!(report.records.len(), 1);
        let payload = report.records[0]
            .provider_payload
            .as_ref()
            .expect("payload");
        assert_eq!(payload["skipped_reason"], "rate_limited");
        assert_eq!(payload.get("judge_decision"), None);

        let _ = std::fs::remove_file(path);
    }

    /// The whole point of choosing `ProviderRequestFinished`: replaying this
    /// record through the ordinary frame reducer must be a complete no-op,
    /// never a spurious frame item in the session's transcript.
    #[test]
    fn the_shadow_record_folds_as_a_no_op_into_the_session_frame() {
        let events = vec![Event::ProviderRequestFinished];
        let frame = crate::frame::agent_frame_from_events(&events);
        assert!(frame.items.is_empty());
    }
}
