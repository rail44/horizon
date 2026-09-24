//! Verify the offline converter against the real Rust reader and projection.

use super::*;
use crate::contract::{ToolCallId, ToolCallRequest, ToolCallResult};
use crate::frame::{agent_frame_from_events, tool_call_occurrences};
use crate::persistence::projection::duckdb::Store;
use serde_json::{json, Value};

fn old_record(sequence: u64, session_id: SessionId, event: Value) -> Value {
    json!({
        "schema": AGENT_EVENT_LOG_SCHEMA,
        "version": 1,
        "event_id": format!("migration-{sequence}"),
        "sequence": sequence,
        "session_id": session_id,
        "turn_id": "turn-migration",
        "provider_id": "builtin.agent.rig",
        "role_id": null,
        "session_context": null,
        "event_kind": event.as_object().unwrap().keys().next().unwrap(),
        "event": event,
        "provider_payload": {"original": true},
        "created_at_unix_ms": 1_000 + sequence,
    })
}

#[test]
fn old_log_cannot_be_opened_for_append_before_explicit_conversion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let original = format!(
        "{}\n",
        old_record(42, SessionId::new(), json!({"TurnEnded":"Completed"}))
    );
    std::fs::write(&path, &original).unwrap();
    let error = read(&path).unwrap_err();
    assert!(error.to_string().contains("explicit format conversion"));
    let (_writer, ready) = WriterHandle::open(&path);
    let initialized = ready
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(matches!(initialized, WriterInit::Failed(_)));
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[test]
fn converted_history_rebuilds_exact_execution_rows_and_preserves_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("old.jsonl");
    let output = dir.path().join("converted");
    let session = SessionId::new();
    let archived_session = SessionId::new();
    let mut rows = Vec::new();
    for sequence in [40, 50] {
        rows.push(old_record(
            sequence,
            session,
            json!({"ToolCallRequested": {
                "call_id": "reused", "tool_id": "bash", "input": {"command": "pwd"},
                "occurrence_id": null,
            }}),
        ));
        rows.push(old_record(
            sequence + 1,
            session,
            json!({"ToolCallStarted": "reused"}),
        ));
        rows.push(old_record(
            sequence + 2,
            session,
            json!({"ToolCallFinished": {
                "call_id": "reused", "occurrence_id": null, "output": {"output": "ok"},
                "is_error": false, "denied": false,
            }}),
        ));
    }
    rows.push(old_record(
        60,
        archived_session,
        json!({"TurnEnded": "Halted"}),
    ));
    let original = rows
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    std::fs::write(&source, &original).unwrap();
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/migrate-agent-history.py");
    let result = std::process::Command::new("python3")
        .arg(script)
        .arg(&source)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(output.join("original.jsonl")).unwrap(),
        original
    );
    assert_eq!(
        crate::persistence::validate_history(output.join("events.jsonl")).unwrap(),
        6
    );
    let report = read(output.join("events.jsonl")).unwrap();
    assert_eq!(report.records.len(), 6);
    assert!(report.skipped_summary().is_none());
    assert_eq!(report.max_known_sequence, Some(52));
    let events = report
        .records
        .iter()
        .map(|record| record.event.clone())
        .collect::<Vec<_>>();
    let expected = agent_frame_from_events(&events);
    let calls = tool_call_occurrences(&expected.items);
    assert_eq!(calls.len(), 2);
    assert_ne!(
        calls[0].request.occurrence_id,
        calls[1].request.occurrence_id
    );
    assert!(calls
        .iter()
        .all(|call| call.started && call.result.is_some()));
    let store = Store::open_in_memory().unwrap();
    let imported = store
        .replace_from_event_log_records(report.records)
        .unwrap();
    assert_eq!(imported.applied, 6);
    assert_eq!(imported.skipped, 0, "{:?}", imported.first_skip_error);
    assert_eq!(store.frame_for_session(session).unwrap(), expected);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["archived_records"], 1);
}

#[test]
fn published_execution_events_reject_missing_or_null_identity() {
    let request = ToolCallRequest {
        call_id: ToolCallId("identity-required".into()),
        occurrence_id: crate::contract::OccurrenceId::new(),
        tool_id: "bash".into(),
        input: json!({"command": "pwd"}).into(),
    };
    let result = ToolCallResult::new(
        request.call_id.clone(),
        request.occurrence_id.clone(),
        json!({}),
    );
    for event in [
        Event::ToolCallRequested(request.clone()),
        Event::ToolCallStarted(request.identity()),
        Event::ToolCallFinished(result),
        Event::ApprovalRequested(crate::contract::ApprovalRequest {
            call_id: request.call_id.clone(),
            occurrence_id: request.occurrence_id.clone(),
            reason: "test".into(),
            kind: crate::contract::ApprovalKind::Standard,
        }),
        Event::ApprovalResolved(crate::contract::ApprovalResolved {
            call_id: request.call_id.clone(),
            occurrence_id: request.occurrence_id.clone(),
            decision: crate::contract::ApprovalDecisionPayload::Approve,
        }),
    ] {
        for missing in [true, false] {
            let mut value = serde_json::to_value(&event).unwrap();
            let payload = value
                .as_object_mut()
                .unwrap()
                .values_mut()
                .next()
                .unwrap()
                .as_object_mut()
                .unwrap();
            if missing {
                payload.remove("occurrence_id");
            } else {
                payload.insert("occurrence_id".into(), Value::Null);
            }
            assert!(serde_json::from_value::<Event>(value).is_err());
        }
    }
}

#[test]
fn conversion_preflight_rejects_skipped_events_and_projection_failures() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    assert!(crate::persistence::validate_history(&path).is_err());
    for event in [
        json!({"UnknownEvent": {}}),
        json!({"TurnEnded": "Completed"}),
    ] {
        let mut record = old_record(1, SessionId::new(), event);
        record["version"] = json!(2);
        record["turn_id"] = Value::Null;
        std::fs::write(&path, format!("{record}\n")).unwrap();
        assert!(crate::persistence::validate_history(&path).is_err());
    }
}
