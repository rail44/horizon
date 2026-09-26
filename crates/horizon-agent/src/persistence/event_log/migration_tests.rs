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
    for version in [1, 2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut record = old_record(42, SessionId::new(), json!({"TurnEnded":"Completed"}));
        record["version"] = json!(version);
        let original = format!("{record}\n");
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
        std::fs::read_to_string(output.join("original.jsonl")).unwrap(),
        original
    );
    let v4 = dir.path().join("v4");
    let converted_count = convert_conversation_file(&output.join("events.jsonl"), &v4).unwrap();
    let report = read(v4.join("events.jsonl")).unwrap();
    assert_eq!(report.records.len(), converted_count);
    assert_eq!(
        crate::persistence::validate_history(v4.join("events.jsonl")).unwrap(),
        converted_count
    );
    assert!(report.skipped_summary().is_none());
    assert_eq!(report.max_known_sequence, Some(converted_count as u64));
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
    assert_eq!(imported.applied, converted_count);
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
        record["version"] = json!(AGENT_EVENT_LOG_VERSION);
        record["turn_id"] = Value::Null;
        std::fs::write(&path, format!("{record}\n")).unwrap();
        assert!(crate::persistence::validate_history(&path).is_err());
    }
}

#[test]
fn v2_outcomes_survive_conversion_projection_and_transcript_without_text_inference() {
    use crate::contract::ToolOutcome;
    use crate::transcript::{aggregate_receipt, build_tool_call_views, ApprovalState};

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("v2.jsonl");
    let output = dir.path().join("v3");
    let session = SessionId::new();
    let cases = [
        (
            json!({"ok": true}),
            false,
            false,
            ToolOutcome::Succeeded,
            ApprovalState::Approved,
            "approved",
        ),
        (
            json!({"message": "denied by user"}),
            true,
            false,
            ToolOutcome::Failed,
            ApprovalState::Approved,
            "approved",
        ),
        (
            json!({"is_error": false}),
            true,
            true,
            ToolOutcome::Denied,
            ApprovalState::Denied,
            "denied",
        ),
        (
            json!({"cancelled": true}),
            false,
            false,
            ToolOutcome::Cancelled,
            ApprovalState::Cancelled,
            "cancelled",
        ),
        (
            json!({"superseded_by_retry": true, "retry_occurrence_id": "retry", "message": "replaced"}),
            false,
            false,
            ToolOutcome::Superseded {
                retry_occurrence_id: crate::contract::OccurrenceId("retry".into()),
            },
            ApprovalState::Superseded,
            "superseded",
        ),
    ];
    let mut rows = vec![];
    let mut push = |event| {
        let mut row = old_record(rows.len() as u64, session, event);
        row["version"] = json!(2);
        rows.push(row);
    };
    for (index, (body, error, denied, _, _, _)) in cases.iter().enumerate() {
        let call = format!("call-{index}");
        let occurrence = format!("attempt-{index}");
        push(
            json!({"ToolCallRequested": {"call_id": call, "occurrence_id": occurrence,
            "tool_id": "bash", "input": {"command": "pwd"}}}),
        );
        push(
            json!({"ApprovalRequested": {"call_id": call, "occurrence_id": occurrence,
            "reason": "approval", "kind": "Standard"}}),
        );
        if index == 4 {
            push(
                json!({"ToolCallRequested": {"call_id": call, "occurrence_id": "retry",
                "tool_id": "bash", "input": {"command": "pwd"}}}),
            );
        }
        push(
            json!({"ToolCallFinished": {"call_id": call, "occurrence_id": occurrence,
            "output": body, "is_error": error, "denied": denied}}),
        );
    }
    let original = rows
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    std::fs::write(&source, &original).unwrap();
    let result = std::process::Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/migrate-agent-history.py"))
        .arg(&source)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read_to_string(source).unwrap(), original);
    let v4 = dir.path().join("v4");
    let converted_count = convert_conversation_file(&output.join("events.jsonl"), &v4).unwrap();
    let report = read(v4.join("events.jsonl")).unwrap();
    assert_eq!(report.records.len(), converted_count);
    assert!(report.skipped_summary().is_none());
    let store = Store::open_in_memory().unwrap();
    let imported = store
        .replace_from_event_log_records(report.records)
        .unwrap();
    assert_eq!(imported.skipped, 0, "{:?}", imported.first_skip_error);
    let frame = store.frame_for_session(session).unwrap();
    let views = build_tool_call_views(&frame.items);
    let approvals = store.approvals_for_session(session).unwrap();
    for (index, (_, _, _, outcome, approval, stored)) in cases.iter().enumerate() {
        assert_eq!(views[index].outcome.as_ref(), Some(outcome));
        assert_eq!(&views[index].approval, approval);
        assert_eq!(approvals[index].outcome.as_deref(), Some(*stored));
    }
    let receipt = aggregate_receipt(&views);
    assert_eq!(
        receipt.bash_count, 1,
        "only the successful execution counts"
    );
    assert_eq!(
        receipt.individual_calls.len(),
        4,
        "failure, denial, cancellation and pending retry remain visible"
    );
}

#[test]
fn v3_conversion_keeps_provider_metadata_and_resolves_reused_clearing_ids() {
    use crate::contract::{ConversationRecord, HistoryCleared, OccurrenceId};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("v3.jsonl");
    let destination = dir.path().join("v4");
    let session = SessionId::new();
    let mut rows = Vec::new();
    for index in 0..2 {
        let request = ToolCallRequest {
            call_id: ToolCallId("reused".into()),
            occurrence_id: OccurrenceId(format!("occ-{index}")),
            tool_id: "fs.read".into(),
            input: json!({"path":format!("file-{index}")}).into(),
        };
        let mut row = old_record(
            rows.len() as u64 + 1,
            session,
            serde_json::to_value(Event::ToolCallRequested(request.clone())).unwrap(),
        );
        row["provider_payload"] = json!({"rig":{"tool_call":{"id":format!("local-{index}"),"call_id":"reused","signature":"signed","additional_params":{"vendor":"preserved"}}}});
        rows.push(row);
        rows.push(old_record(
            rows.len() as u64 + 1,
            session,
            serde_json::to_value(Event::ToolCallFinished(
                request.identity().result(json!({"content":"body"})),
            ))
            .unwrap(),
        ));
        rows.push(old_record(
            rows.len() as u64 + 1,
            session,
            json!({"HistoryCleared":{"cleared_call_ids":["reused"],"recovered_chars":100}}),
        ));
    }
    for row in &mut rows {
        row["version"] = 3.into();
    }
    let original = rows
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    std::fs::write(&source, &original).unwrap();
    let count = convert_conversation_file(&source, &destination).unwrap();
    assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(destination.join("original.jsonl")).unwrap(),
        original
    );
    let report = read(destination.join("events.jsonl")).unwrap();
    assert_eq!(report.records.len(), count);
    let mut cleared = Vec::new();
    let mut announced = 0;
    for record in report.records {
        match record.event {
            Event::HistoryCleared(HistoryCleared {
                cleared_occurrence_ids,
                ..
            }) => cleared.extend(cleared_occurrence_ids),
            Event::ConversationRecorded(ConversationRecord::ToolAnnounced {
                tool_call, ..
            }) => {
                assert_eq!(tool_call.0["signature"], "signed");
                assert_eq!(tool_call.0["additional_params"]["vendor"], "preserved");
                announced += 1;
            }
            _ => {}
        }
    }
    assert_eq!(announced, 2);
    assert_eq!(
        cleared,
        [OccurrenceId("occ-0".into()), OccurrenceId("occ-1".into())]
    );
    let copy = dir.path().join("idempotent");
    assert_eq!(
        convert_conversation_file(&destination.join("events.jsonl"), &copy).unwrap(),
        count
    );
    assert_eq!(
        std::fs::read(destination.join("events.jsonl")).unwrap(),
        std::fs::read(copy.join("events.jsonl")).unwrap()
    );
    assert!(convert_conversation_file(&source, &destination).is_err());
}

#[test]
fn invalid_current_conversation_cannot_create_an_activation_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("invalid.jsonl");
    let destination = dir.path().join("rejected");
    let mut row = old_record(
        1,
        SessionId::new(),
        json!({"ConversationRecorded":{"Response":{"response_id":"invalid","codec":999,"message":{},"calls":[]}}}),
    );
    row["version"] = 4.into();
    let original = format!("{row}\n");
    std::fs::write(&source, &original).unwrap();
    let error = convert_conversation_file(&source, &destination).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Unsupported conversation message codec"),
        "{error}"
    );
    assert!(!destination.exists());
    assert_eq!(std::fs::read_to_string(source).unwrap(), original);
}
