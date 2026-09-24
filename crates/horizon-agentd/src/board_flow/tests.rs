use super::log::{Index, Pending, Tail};
use horizon_agent::contract::{Event, InputResult, SessionId, SessionInput, SessionInputOutcome};
use horizon_agent::persistence::event_log::Record;
use std::io::Write;

pub(super) fn record(session_id: SessionId, event: Event) -> Record {
    Record {
        schema: "horizon.agent.event_log".into(),
        version: 2,
        event_id: uuid::Uuid::new_v4().to_string(),
        sequence: 1,
        session_id,
        turn_id: None,
        provider_id: None,
        role_id: None,
        session_context: None,
        event_kind: horizon_agent::contract::event_kind(&event).into(),
        event,
        provider_payload: None,
        created_at_unix_ms: 1,
    }
}
#[test]
fn only_explicit_terminal_outcomes_enter_the_delivery_queue() {
    let session = SessionId::new();
    let mut index = Index::default();
    index.fold(record(
        session,
        Event::MessageCommitted(horizon_agent::contract::Message {
            role: horizon_agent::contract::MessageRole::Assistant,
            text: "partial working text".into(),
        }),
    ));
    assert!(index.pending.is_empty());
    let outcome = SessionInputOutcome {
        input_ids: vec!["request".into()],
        delivery_id: "result".into(),
        reply_to: None,
        outcome: InputResult::Success {
            text: "final answer".into(),
        },
    };
    index.fold(record(session, Event::InputOutcome(outcome.clone())));
    assert!(
        matches!(index.pending.get(&(session,"result".into())),Some(Pending::Answer{outcome:value,..}) if value==&outcome)
    );
    index.fold(record(
        session,
        Event::DeliveryAcknowledged("result".into()),
    ));
    index.fold(record(session, Event::InputOutcome(outcome)));
    assert!(index.pending.is_empty());
}
#[test]
fn receipts_are_scoped_to_recipient_and_stable_source() {
    let source = SessionId::new();
    let target = SessionId::new();
    let input = SessionInput {
        id: "send".into(),
        origin: "requester".into(),
        text: "Review this".into(),
        reply_to: None,
        resume_work: false,
    };
    let mut index = Index::default();
    index.fold(record(
        source,
        Event::SessionInputSent {
            session_id: target,
            input: input.clone(),
        },
    ));
    assert!(index.pending.contains_key(&(source, "send".into())));
    assert!(!index.accepted.contains(&(target, "send".into())));
    index.fold(record(target, Event::InputAccepted(input)));
    assert!(index.accepted.contains(&(target, "send".into())));
    assert!(!index.accepted.contains(&(source, "send".into())));
}
#[test]
fn tail_retries_a_partial_last_record_without_replaying_prior_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let mut file = std::fs::File::create(&path).unwrap();
    let session = SessionId::new();
    let text =
        serde_json::to_string(&record(session, Event::DeliveryAcknowledged("one".into()))).unwrap();
    writeln!(file, "{text}").unwrap();
    let split = text.len() / 2;
    write!(file, "{}", &text[..split]).unwrap();
    file.flush().unwrap();
    let mut tail = Tail::new(path);
    assert_eq!(tail.read().unwrap().len(), 1);
    assert!(tail.read().unwrap().is_empty());
    writeln!(file, "{}", &text[split..]).unwrap();
    file.flush().unwrap();
    assert_eq!(tail.read().unwrap().len(), 1);
    assert!(tail.read().unwrap().is_empty());
}

#[test]
fn queueing_an_input_is_not_a_durable_receipt() {
    let state = crate::session::test_support::state_with_rig_config(false, "test");
    let target = SessionId::new();
    let receiver = state.install_test_session(target);
    let input = SessionInput {
        id: "request".into(),
        origin: "owner".into(),
        text: "Explain the approach".into(),
        reply_to: None,
        resume_work: false,
    };
    let mut index = Index::default();
    assert!(!super::dispatch::receipt(
        &state,
        &index,
        target,
        input.clone()
    ));
    assert!(
        matches!(receiver.try_recv().unwrap(),horizon_agent::contract::Command::SessionInput(value) if value==input)
    );
    index.fold(record(target, Event::InputAccepted(input.clone())));
    assert!(super::dispatch::receipt(&state, &index, target, input));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn pending_deliveries_keep_log_order_across_replay_and_duplicate_records() {
    let first = SessionId::new();
    let second = SessionId::new();
    let target = SessionId::new();
    let mut events = Vec::new();
    for (source, sequence, id) in [
        (first, 10, "request"),
        (second, 11, "result"),
        (first, 12, "correction"),
    ] {
        let event = if id == "result" {
            Event::InputOutcome(SessionInputOutcome {
                input_ids: vec!["input".into()],
                delivery_id: id.into(),
                reply_to: Some(super::ReplyAddress::session(target)),
                outcome: InputResult::Success {
                    text: "result".into(),
                },
            })
        } else {
            Event::SessionInputSent {
                session_id: target,
                input: SessionInput {
                    id: id.into(),
                    origin: "source".into(),
                    text: id.into(),
                    reply_to: None,
                    resume_work: false,
                },
            }
        };
        let mut value = record(source, event);
        value.sequence = sequence;
        events.push(value);
    }
    let expected = vec!["request", "result", "correction"];
    for order in [[0, 1, 2], [2, 0, 1]] {
        let mut index = Index::default();
        for offset in order {
            index.fold(events[offset].clone());
        }
        let mut duplicate = events[0].clone();
        duplicate.sequence = 99;
        index.fold(duplicate);
        assert_eq!(
            index
                .ordered_pending()
                .iter()
                .map(|((_, id), _)| id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        index.fold(record(first, Event::DeliveryAcknowledged("request".into())));
        assert_eq!(
            index
                .ordered_pending()
                .iter()
                .map(|((_, id), _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["result", "correction"]
        );
    }
}

#[test]
fn tail_read_failure_does_not_discard_earlier_records_in_the_same_batch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let text = serde_json::to_string(&record(
        SessionId::new(),
        Event::DeliveryAcknowledged("retained".into()),
    ))
    .unwrap()
        + "\n";
    let mut broken = text.as_bytes().to_vec();
    broken.extend_from_slice(&[0xff, b'\n']);
    std::fs::write(&path, broken).unwrap();
    let mut tail = Tail::new(path.clone());
    assert!(tail.read().is_err());
    std::fs::write(&path, text).unwrap();
    let records = tail.read().unwrap();
    assert_eq!(records.len(), 1);
    assert!(matches!(&records[0].event,Event::DeliveryAcknowledged(id) if id=="retained"));
    assert!(tail.read().unwrap().is_empty());
}
