use super::*;
use crate::session::approval::dispatch_inbound_command;
use crate::session::test_support::{drain_events, test_state};
use crate::session::Connection;
use horizon_agent::contract::{Command, InputResult, SessionInputOutcome};
use horizon_agent::persistence::event_log::{read, WriterHandle, WriterInit};

fn input(id: &str) -> SessionInput {
    SessionInput {
        resume_work: false,
        id: id.into(),
        origin: "test".into(),
        text: "An identified request".into(),
        reply_to: Some("sender".into()),
    }
}

#[test]
fn identified_inputs_publish_after_flush_and_deduplicate_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let (writer, ready) = WriterHandle::open(&path);
    assert!(matches!(ready.recv().unwrap(), WriterInit::Ready(_)));
    let state = test_state();
    state.set_writer(Some(writer.clone()));
    let session_id = SessionId::new();
    let live = LiveState::with_event_log_and_history(session_id, None, None, writer, Vec::new());
    let mut events = Connection::new(state.clone()).subscribe_agent(session_id);
    let (commands_tx, commands) = crossbeam_channel::unbounded();
    let accepted = input("accepted");
    for _ in 0..2 {
        dispatch_inbound_command(
            &state,
            &live,
            &commands_tx,
            session_id,
            Command::SessionInput(accepted.clone()),
        );
    }
    assert_eq!(
        commands.try_iter().collect::<Vec<_>>(),
        vec![Command::SessionInput(accepted.clone())]
    );
    assert_eq!(
        read(&path)
            .unwrap()
            .records
            .iter()
            .map(|r| r.event.clone())
            .collect::<Vec<_>>(),
        vec![Event::InputAccepted(accepted)]
    );
    let outbound = input("outbound");
    let recipient = SessionId::new();
    for _ in 0..2 {
        dispatch_inbound_command(
            &state,
            &live,
            &commands_tx,
            session_id,
            Command::SendSessionInput {
                session_id: recipient,
                input: outbound.clone(),
            },
        );
    }
    let outcome = Event::InputOutcome(SessionInputOutcome {
        input_ids: vec!["accepted".into()],
        delivery_id: "result".into(),
        reply_to: Some("sender".into()),
        outcome: InputResult::Success {
            text: "Done".into(),
        },
    });
    assert!(persist_and_send_session_event(
        &state, &live, session_id, outcome
    ));
    for id in ["unknown", "outbound", "outbound", "result", "result"] {
        dispatch_inbound_command(
            &state,
            &live,
            &commands_tx,
            session_id,
            Command::AcknowledgeDelivery {
                delivery_id: id.into(),
            },
        );
    }
    let published = drain_events(&mut events);
    let persisted = read(&path)
        .unwrap()
        .records
        .into_iter()
        .map(|r| r.event)
        .collect::<Vec<_>>();
    assert_eq!(published, persisted);
    assert_eq!(persisted, live.events());
    assert_eq!(persisted.len(), 5);
    assert_eq!(
        &persisted[3..],
        &[
            Event::DeliveryAcknowledged("outbound".into()),
            Event::DeliveryAcknowledged("result".into())
        ]
    );
    assert!(commands.try_recv().is_err());
}

#[test]
fn unavailable_persistence_never_forwards_or_publishes_an_input() {
    let state = test_state();
    let session_id = SessionId::new();
    let mut events = Connection::new(state.clone()).subscribe_agent(session_id);
    let (commands_tx, commands) = crossbeam_channel::unbounded();
    let dir = tempfile::tempdir().unwrap();
    let (failed_writer, ready) = WriterHandle::open(dir.path());
    assert!(matches!(ready.recv().unwrap(), WriterInit::Failed(_)));
    let failed = LiveState::with_event_log_and_history(
        session_id,
        None,
        None,
        failed_writer.clone(),
        Vec::new(),
    );
    state.set_writer(Some(failed_writer));
    for live in [LiveState::with_disabled_persistence(), failed] {
        dispatch_inbound_command(
            &state,
            &live,
            &commands_tx,
            session_id,
            Command::SessionInput(input("rejected")),
        );
        assert!(live.events().is_empty());
        assert!(commands.try_recv().is_err());
        assert!(drain_events(&mut events).is_empty());
    }
}
