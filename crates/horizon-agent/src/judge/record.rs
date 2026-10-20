//! Durable, `call_id`-keyed enforcing-judge audit records. They use the same
//! event log/DuckDB projection pipeline as agent events, so they survive a
//! projection rebuild and remain JSON-queryable.
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
//! (`JUDGE_VERDICT_EVENT_KIND`) rather than the derived
//! `"provider_request_finished"`, so a query can filter on it directly.
use uuid::Uuid;

use crate::contract::{Event, SessionId};
use crate::persistence::event_log::{
    Record, WriterHandle, AGENT_EVENT_LOG_SCHEMA, AGENT_EVENT_LOG_VERSION,
};

use super::{JudgeBackendKind, JudgeDecision, JudgeFallbackReason, JudgeInput, JudgeVerdict};

/// Current enforcing-mode event label.
pub(crate) const JUDGE_VERDICT_EVENT_KIND: &str = "judge_verdict";

/// Writes one real verdict's calibration record. `threshold` is the
/// decision backend's ask threshold, `None` on the chat backend; it, the
/// stage, and the ask probability are written as JSON null where they do
/// not apply.
pub(super) fn write_verdict(
    writer: &WriterHandle,
    session_id: SessionId,
    input: &JudgeInput,
    model: &str,
    verdict: &JudgeVerdict,
    threshold: Option<f64>,
    latency_ms: u64,
) {
    let payload = serde_json::json!({
        "call_id": input.identity.call_id.0,
        "occurrence_id": input.identity.occurrence_id.0,
        "tool_id": input.tool_id,
        "judge_model": model,
        "judge_backend": verdict.backend.label(),
        "mode": "enforcing",
        "judge_decision": decision_label(verdict.decision),
        "judge_stage": verdict.stage,
        "judge_ask_probability": verdict.ask_probability,
        "judge_threshold": threshold,
        "judge_confidence": verdict.confidence,
        "fallback_reason": verdict.fallback_reason.map(fallback_reason_label),
        "fallback_error": verdict.fallback_error,
        "latency_ms": latency_ms,
        "requested_filesystem_grants": input.requested_filesystem_grants,
        "requested_domains": input.requested_domains,
        "approval_scope": approval_scope(input),
    });
    append(writer, session_id, payload);
}

/// Writes a record for a call the rate limiter skipped -- never reached the
/// model at all. It records the enforced fail-closed escalation and why,
/// with no model stage/probability/confidence/latency.
pub(super) fn write_skipped(
    writer: &WriterHandle,
    session_id: SessionId,
    input: &JudgeInput,
    model: &str,
    backend: JudgeBackendKind,
    reason: &str,
) {
    let payload = serde_json::json!({
        "call_id": input.identity.call_id.0,
        "occurrence_id": input.identity.occurrence_id.0,
        "tool_id": input.tool_id,
        "judge_model": model,
        "judge_backend": backend.label(),
        "mode": "enforcing",
        "judge_decision": "escalate",
        "fallback_reason": reason,
        "skipped_reason": reason,
        "requested_filesystem_grants": input.requested_filesystem_grants,
        "requested_domains": input.requested_domains,
        "approval_scope": approval_scope(input),
    });
    append(writer, session_id, payload);
}

fn approval_scope(input: &JudgeInput) -> &'static str {
    if input.host_execution_requested {
        "host_execution_once"
    } else {
        "bounded"
    }
}

fn decision_label(decision: JudgeDecision) -> &'static str {
    match decision {
        JudgeDecision::AutoApprove => "auto_approve",
        JudgeDecision::Escalate => "escalate",
    }
}

fn fallback_reason_label(reason: JudgeFallbackReason) -> &'static str {
    match reason {
        JudgeFallbackReason::ClientError => "client_error",
        JudgeFallbackReason::Unparseable => "unparseable",
        JudgeFallbackReason::Timeout => "timeout",
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
        session_context: None,
        event_kind: JUDGE_VERDICT_EVENT_KIND.to_string(),
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
            identity: crate::contract::ToolCallIdentity {
                call_id: crate::contract::ToolCallId(call_id.into()),
                occurrence_id: crate::contract::OccurrenceId("judge-occ".into()),
            },
            tool_id: "mock.boundary_crossing".to_string(),
            args: serde_json::json!({}),
            tool_description: None,
            prior_user_messages: Vec::new(),
            requested_filesystem_grants: Vec::new(),
            requested_domains: Vec::new(),
            host_execution_requested: false,
            git_metadata_operation: false,
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
            "clef:cloud",
            &JudgeVerdict {
                decision: JudgeDecision::Escalate,
                backend: JudgeBackendKind::Decision,
                stage: None,
                ask_probability: Some(0.75),
                confidence: Some(0.75),
                fallback_reason: None,
                fallback_error: None,
            },
            Some(0.2),
            512,
        );
        writer.flush().expect("flush");

        let report = event_log::read(&path).expect("read");
        assert_eq!(report.records.len(), 1);
        let record = &report.records[0];
        assert_eq!(record.event_kind, JUDGE_VERDICT_EVENT_KIND);
        assert_eq!(record.event, Event::ProviderRequestFinished);
        let payload = record.provider_payload.as_ref().expect("payload present");
        assert_eq!(payload["call_id"], "call-1");
        assert_eq!(payload["occurrence_id"], "judge-occ");
        assert_eq!(payload["judge_decision"], "escalate");
        assert_eq!(payload["judge_backend"], "decision");
        assert_eq!(payload["judge_ask_probability"], 0.75);
        assert_eq!(payload["judge_threshold"], 0.2);
        assert_eq!(payload["judge_confidence"], 0.75);
        assert_eq!(payload["judge_stage"], serde_json::Value::Null);
        assert_eq!(payload["latency_ms"], 512);
        assert_eq!(payload["mode"], "enforcing");
        assert_eq!(payload["fallback_reason"], serde_json::Value::Null);
        assert_eq!(payload["fallback_error"], serde_json::Value::Null);
        assert_eq!(payload["approval_scope"], "bounded");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn client_error_verdict_records_the_error_text() {
        let path = temp_log_path("client-error");
        let (writer, _init_rx) = WriterHandle::open(&path);
        let session_id = SessionId::new();
        let input = judge_input("call-err");

        write_verdict(
            &writer,
            session_id,
            &input,
            "clef:cloud",
            &JudgeVerdict {
                decision: JudgeDecision::Escalate,
                backend: JudgeBackendKind::Decision,
                stage: None,
                ask_probability: None,
                confidence: None,
                fallback_reason: Some(JudgeFallbackReason::ClientError),
                fallback_error: Some("judge request failed: 404 model not found".to_string()),
            },
            Some(0.2),
            30,
        );
        writer.flush().expect("flush");

        let report = event_log::read(&path).expect("read");
        assert_eq!(report.records.len(), 1);
        let payload = report.records[0]
            .provider_payload
            .as_ref()
            .expect("payload");
        assert_eq!(payload["fallback_reason"], "client_error");
        assert_eq!(payload["judge_ask_probability"], serde_json::Value::Null);
        assert_eq!(payload["judge_confidence"], serde_json::Value::Null);
        assert_eq!(
            payload["fallback_error"],
            "judge request failed: 404 model not found"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn host_execution_verdict_records_the_whole_call_scope() {
        let path = temp_log_path("host-scope");
        let (writer, _init_rx) = WriterHandle::open(&path);
        let session_id = SessionId::new();
        let mut input = judge_input("call-host");
        input.host_execution_requested = true;

        write_verdict(
            &writer,
            session_id,
            &input,
            "clef:cloud",
            &JudgeVerdict {
                decision: JudgeDecision::AutoApprove,
                backend: JudgeBackendKind::Decision,
                stage: None,
                ask_probability: Some(0.0),
                confidence: Some(1.0),
                fallback_reason: None,
                fallback_error: None,
            },
            Some(0.2),
            12,
        );
        writer.flush().expect("flush");

        let report = event_log::read(&path).expect("read");
        let payload = report.records[0]
            .provider_payload
            .as_ref()
            .expect("payload");
        assert_eq!(payload["approval_scope"], "host_execution_once");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn write_skipped_records_fail_closed_decision_and_reason() {
        let path = temp_log_path("skipped");
        let (writer, _init_rx) = WriterHandle::open(&path);
        let session_id = SessionId::new();
        let input = judge_input("call-2");

        write_skipped(
            &writer,
            session_id,
            &input,
            "clef:cloud",
            JudgeBackendKind::Decision,
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
        assert_eq!(payload["judge_backend"], "decision");
        assert_eq!(payload["fallback_reason"], "rate_limited");
        assert_eq!(payload["judge_decision"], "escalate");
        assert_eq!(payload["approval_scope"], "bounded");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_chat_verdict_records_its_backend_and_stage_with_nulls_for_the_rest() {
        let path = temp_log_path("chat");
        let (writer, _init_rx) = WriterHandle::open(&path);
        let session_id = SessionId::new();
        let input = judge_input("call-chat");

        write_verdict(
            &writer,
            session_id,
            &input,
            "small-chat",
            &JudgeVerdict {
                decision: JudgeDecision::AutoApprove,
                backend: JudgeBackendKind::Chat,
                stage: Some(1),
                ask_probability: None,
                confidence: Some(0.5),
                fallback_reason: None,
                fallback_error: None,
            },
            None,
            40,
        );
        write_skipped(
            &writer,
            session_id,
            &input,
            "small-chat",
            JudgeBackendKind::Chat,
            "rate_limited",
        );
        writer.flush().expect("flush");

        let report = event_log::read(&path).expect("read");
        assert_eq!(report.records.len(), 2);
        let payload = report.records[0]
            .provider_payload
            .as_ref()
            .expect("payload");
        assert_eq!(payload["judge_backend"], "chat");
        assert_eq!(payload["judge_model"], "small-chat");
        assert_eq!(payload["judge_decision"], "auto_approve");
        assert_eq!(payload["judge_stage"], 1);
        assert_eq!(payload["judge_confidence"], 0.5);
        for key in ["judge_ask_probability", "judge_threshold"] {
            assert_eq!(
                payload.get(key),
                Some(&serde_json::Value::Null),
                "{key} is present as null: {payload}"
            );
        }
        let skipped = report.records[1]
            .provider_payload
            .as_ref()
            .expect("payload");
        assert_eq!(skipped["judge_backend"], "chat");

        let _ = std::fs::remove_file(path);
    }

    /// The whole point of choosing `ProviderRequestFinished`: replaying this
    /// record through the ordinary frame reducer must be a complete no-op,
    /// never a spurious frame item in the session's transcript.
    #[test]
    fn the_judge_record_folds_as_a_no_op_into_the_session_frame() {
        let events = vec![Event::ProviderRequestFinished];
        let frame = crate::frame::agent_frame_from_events(&events);
        assert!(frame.items.is_empty());
    }
}
