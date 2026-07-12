use std::collections::HashMap;
use std::time::{Duration, Instant};

use horizon_agent::contract::{Event, ProviderId, SessionId, SessionState};
use horizon_agent::wire::{self as agent_wire, Envelope};
use horizon_session_protocol::{self as session_wire, Hello, SESSION_PROTOCOL_VERSION};
use horizon_terminal_core::{
    decode_terminal_command, decode_terminal_control, encode_terminal_update, TerminalColorScheme,
    TerminalFrame, TerminalSize,
};
use tokio::io::BufReader;

use super::*;

fn spec() -> TerminalSpawnSpec {
    TerminalSpawnSpec {
        shell: "/bin/sh".into(),
        args: Vec::new(),
        term: "xterm-256color".into(),
        scrollback_lines: 1_000,
        color_scheme: TerminalColorScheme::default(),
        control_socket: "/tmp/horizon-control.sock".into(),
        fallback_cwd: "/tmp".into(),
        spawn_source_session_id: None,
        initial_size: TerminalSize::new(80, 24),
    }
}

async fn receive_hello_and_reply<S>(
    reader: &mut BufReader<S>,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
) where
    S: tokio::io::AsyncRead + Unpin,
{
    let hello = session_wire::read_envelope(reader)
        .await
        .unwrap()
        .expect("client hello");
    assert_eq!(hello.kind, horizon_session_protocol::SESSION_CONTROL_KIND);
    let reply = RawEnvelope::session_control(&SessionControl::Hello(Hello {
        contract_version: SESSION_PROTOCOL_VERSION,
        binary_id: "test-sessiond".into(),
        capabilities: vec!["agent".into(), "terminal".into()],
    }))
    .unwrap();
    session_wire::write_envelope(writer, &reply).await.unwrap();
}

#[tokio::test]
async fn start_returns_before_hello_and_queued_create_is_first_after_handshake() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let started = Instant::now();
    let (handle, _host_tools) = SessiondHandle::start_on_stream(client);
    assert!(started.elapsed() < Duration::from_millis(50));

    let terminal_id = Uuid::new_v4();
    let terminal = handle.start_terminal(terminal_id, spec());
    let (read_half, mut writer) = tokio::io::split(server);
    let mut reader = BufReader::new(read_half);
    let hello = session_wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("client hello");
    assert_eq!(hello.kind, horizon_session_protocol::SESSION_CONTROL_KIND);
    assert!(tokio::time::timeout(
        Duration::from_millis(30),
        session_wire::read_envelope(&mut reader)
    )
    .await
    .is_err());

    let reply = RawEnvelope::session_control(&SessionControl::Hello(Hello {
        contract_version: SESSION_PROTOCOL_VERSION,
        binary_id: "delayed-sessiond".into(),
        capabilities: vec!["agent".into(), "terminal".into()],
    }))
    .unwrap();
    session_wire::write_envelope(&mut writer, &reply)
        .await
        .unwrap();
    let create = session_wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("queued terminal create");
    assert_eq!(create.session_id, Some(terminal_id));
    assert!(matches!(
        decode_terminal_control(&create).unwrap(),
        TerminalControl::Create(_)
    ));

    let snapshot = TerminalUpdate::Snapshot(TerminalFrame::from_text("ready".into()));
    session_wire::write_envelope(
        &mut writer,
        &encode_terminal_update(terminal_id, &snapshot).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        terminal
            .updates()
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        snapshot
    );
}

#[tokio::test]
async fn incoming_shared_agent_and_terminal_messages_are_demultiplexed() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools) = SessiondHandle::start_on_stream(client);
    let terminal_id = Uuid::new_v4();
    let terminal = handle.start_terminal(terminal_id, spec());
    let agent_id = SessionId::new();
    let agent = handle.start_session(agent_id, ProviderId("mock".into()), None);

    let (read_half, mut writer) = tokio::io::split(server);
    let mut reader = BufReader::new(read_half);
    receive_hello_and_reply(&mut reader, &mut writer).await;
    for _ in 0..2 {
        session_wire::read_envelope(&mut reader)
            .await
            .unwrap()
            .expect("queued create");
    }

    let snapshot = TerminalUpdate::Snapshot(TerminalFrame::from_text("terminal".into()));
    session_wire::write_envelope(
        &mut writer,
        &encode_terminal_update(terminal_id, &snapshot).unwrap(),
    )
    .await
    .unwrap();
    let event = Event::StateChanged(SessionState::WaitingForUser);
    let raw = agent_wire::encode_envelope(&Envelope::event(agent_id, event.clone())).unwrap();
    session_wire::write_envelope(&mut writer, &raw)
        .await
        .unwrap();
    terminal
        .sender()
        .send(TerminalCommand::Input(b"fifo".to_vec()))
        .unwrap();
    session_wire::write_envelope(
        &mut writer,
        &RawEnvelope::session_control(&SessionControl::Ping).unwrap(),
    )
    .await
    .unwrap();

    assert_eq!(
        terminal
            .updates()
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        snapshot
    );
    assert_eq!(
        agent
            .events()
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .event,
        event
    );
    let mut saw_command = false;
    let mut saw_pong = false;
    for _ in 0..2 {
        let envelope = session_wire::read_envelope(&mut reader)
            .await
            .unwrap()
            .expect("terminal command or pong");
        if envelope.kind == horizon_terminal_core::TERMINAL_COMMAND_KIND {
            assert_eq!(
                decode_terminal_command(&envelope).unwrap(),
                TerminalCommand::Input(b"fifo".to_vec())
            );
            assert!(!saw_command, "duplicate terminal command");
            saw_command = true;
        } else if envelope.kind == horizon_session_protocol::SESSION_CONTROL_KIND {
            assert_eq!(
                envelope
                    .decode_payload::<SessionControl>(
                        horizon_session_protocol::SESSION_CONTROL_KIND
                    )
                    .unwrap(),
                SessionControl::Pong
            );
            assert!(!saw_pong, "duplicate pong");
            saw_pong = true;
        } else {
            panic!("unexpected envelope kind: {}", envelope.kind);
        }
    }
    assert!(saw_command && saw_pong);
}

#[tokio::test]
async fn concurrent_terminal_lists_are_correlated_by_request_id() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools) = SessiondHandle::start_on_stream(client);
    let (read_half, mut writer) = tokio::io::split(server);
    let mut reader = BufReader::new(read_half);
    receive_hello_and_reply(&mut reader, &mut writer).await;

    let first_handle = handle.clone();
    let first = std::thread::spawn(move || first_handle.terminal_list().unwrap());
    let second_handle = handle.clone();
    let second = std::thread::spawn(move || second_handle.terminal_list().unwrap());
    let first_request = session_wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("first terminal list request");
    let second_request = session_wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("second terminal list request");
    let TerminalControl::List {
        request_id: first_request_id,
    } = decode_terminal_control(&first_request).unwrap()
    else {
        panic!("expected terminal list request");
    };
    let TerminalControl::List {
        request_id: second_request_id,
    } = decode_terminal_control(&second_request).unwrap()
    else {
        panic!("expected terminal list request");
    };
    let first_session = Uuid::new_v4();
    let second_session = Uuid::new_v4();

    session_wire::write_envelope(
        &mut writer,
        &encode_terminal_control(
            None,
            &TerminalControl::ListResult {
                request_id: second_request_id,
                sessions: vec![TerminalSummary {
                    session_id: second_session,
                }],
            },
        )
        .unwrap(),
    )
    .await
    .unwrap();
    session_wire::write_envelope(
        &mut writer,
        &encode_terminal_control(
            None,
            &TerminalControl::ListResult {
                request_id: first_request_id,
                sessions: vec![TerminalSummary {
                    session_id: first_session,
                }],
            },
        )
        .unwrap(),
    )
    .await
    .unwrap();

    let mut returned = vec![
        first.join().unwrap()[0].session_id,
        second.join().unwrap()[0].session_id,
    ];
    returned.sort_unstable();
    let mut expected = vec![first_session, second_session];
    expected.sort_unstable();
    assert_eq!(returned, expected);
}

#[tokio::test]
async fn terminal_batch_attach_keeps_attached_sessions_and_drops_not_found() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools) = SessiondHandle::start_on_stream(client);
    let (read_half, mut writer) = tokio::io::split(server);
    let mut reader = BufReader::new(read_half);
    receive_hello_and_reply(&mut reader, &mut writer).await;
    let attached_id = Uuid::new_v4();
    let missing_id = Uuid::new_v4();
    let attach_handle = handle.clone();
    let attached =
        std::thread::spawn(move || attach_handle.attach_terminals(vec![attached_id, missing_id]));

    let mut requests = HashMap::new();
    for _ in 0..2 {
        let envelope = session_wire::read_envelope(&mut reader)
            .await
            .unwrap()
            .expect("terminal attach request");
        let TerminalControl::Attach { request_id } = decode_terminal_control(&envelope).unwrap()
        else {
            panic!("expected terminal attach request");
        };
        requests.insert(envelope.session_id.unwrap(), request_id);
    }

    session_wire::write_envelope(
        &mut writer,
        &encode_terminal_control(
            Some(missing_id),
            &TerminalControl::AttachResult {
                request_id: requests[&missing_id],
                result: TerminalAttachResult::NotFound,
            },
        )
        .unwrap(),
    )
    .await
    .unwrap();
    session_wire::write_envelope(
        &mut writer,
        &encode_terminal_control(
            Some(attached_id),
            &TerminalControl::AttachResult {
                request_id: requests[&attached_id],
                result: TerminalAttachResult::Attached,
            },
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let snapshot = TerminalUpdate::Snapshot(TerminalFrame::from_text("survived".into()));
    session_wire::write_envelope(
        &mut writer,
        &encode_terminal_update(attached_id, &snapshot).unwrap(),
    )
    .await
    .unwrap();

    let mut sessions = attached.join().unwrap();
    assert_eq!(sessions.len(), 1);
    let (session_id, session) = sessions.pop().unwrap();
    assert_eq!(session_id, attached_id);
    assert_eq!(
        session
            .updates()
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        snapshot
    );
}

#[tokio::test]
async fn terminal_attach_rejects_a_result_for_a_different_session() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools) = SessiondHandle::start_on_stream(client);
    let (read_half, mut writer) = tokio::io::split(server);
    let mut reader = BufReader::new(read_half);
    receive_hello_and_reply(&mut reader, &mut writer).await;
    let requested_id = Uuid::new_v4();
    let attach_handle = handle.clone();
    let attached = std::thread::spawn(move || attach_handle.attach_terminals(vec![requested_id]));
    let request = session_wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("terminal attach request");
    let TerminalControl::Attach { request_id } = decode_terminal_control(&request).unwrap() else {
        panic!("expected terminal attach request");
    };

    session_wire::write_envelope(
        &mut writer,
        &encode_terminal_control(
            Some(Uuid::new_v4()),
            &TerminalControl::AttachResult {
                request_id,
                result: TerminalAttachResult::Attached,
            },
        )
        .unwrap(),
    )
    .await
    .unwrap();

    assert!(attached.join().unwrap().is_empty());
}

#[tokio::test]
async fn dropping_the_runtime_does_not_send_drain() {
    let (client, server) = tokio::io::duplex(4096);
    let (handle, _host_tools) = SessiondHandle::start_on_stream(client);
    let responder = handle.responder();
    let (read_half, mut writer) = tokio::io::split(server);
    let mut reader = BufReader::new(read_half);
    receive_hello_and_reply(&mut reader, &mut writer).await;
    drop(handle);

    let next = tokio::time::timeout(
        Duration::from_secs(1),
        session_wire::read_envelope(&mut reader),
    )
    .await
    .expect("runtime should close after its last handle drops")
    .unwrap();
    assert!(next.is_none());
    drop(responder);
}

#[tokio::test]
async fn dropping_before_hello_cancels_the_runtime_without_drain() {
    let (client, server) = tokio::io::duplex(4096);
    let (handle, _host_tools) = SessiondHandle::start_on_stream(client);
    let responder = handle.responder();
    let (read_half, _write_half) = tokio::io::split(server);
    let mut reader = BufReader::new(read_half);
    let hello = session_wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .expect("client hello");
    assert_eq!(hello.kind, horizon_session_protocol::SESSION_CONTROL_KIND);

    drop(handle);
    let next = tokio::time::timeout(
        Duration::from_secs(1),
        session_wire::read_envelope(&mut reader),
    )
    .await
    .expect("pre-hello runtime should stop after its handle drops")
    .unwrap();
    assert!(next.is_none());
    drop(responder);
}

#[tokio::test]
async fn established_disconnect_reports_errors_without_reconnecting() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (handle, _host_tools) = SessiondHandle::start_on_stream(client);
    let terminal_id = Uuid::new_v4();
    let terminal = handle.start_terminal(terminal_id, spec());
    let agent_id = SessionId::new();
    let agent = handle.start_session(agent_id, ProviderId("mock".into()), None);

    let (read_half, mut writer) = tokio::io::split(server);
    let mut reader = BufReader::new(read_half);
    receive_hello_and_reply(&mut reader, &mut writer).await;
    for _ in 0..2 {
        session_wire::read_envelope(&mut reader)
            .await
            .unwrap()
            .expect("queued create");
    }
    drop(reader);
    drop(writer);

    let terminal_error = terminal
        .updates()
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(matches!(terminal_error, TerminalUpdate::Error(_)));
    let agent_error = agent.events().recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(agent_error.event, Event::Error(_)));

    assert!(handle.session_list().unwrap_err().contains("disconnected"));
    let late_terminal = handle.start_terminal(Uuid::new_v4(), spec());
    assert!(matches!(
        late_terminal
            .updates()
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        TerminalUpdate::Error(_)
    ));
}
