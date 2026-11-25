use std::io::{BufReader, Write};
use std::os::unix::net::UnixListener;

use super::*;
use crate::contract::{HelloAck, Workspace};

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("horizon-control-client-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Answers one connection: the handshake, then `reply` to the query.
fn serve_once(listener: UnixListener, reply: EnvelopeBody) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let hello = wire::read_envelope(&mut reader).unwrap().unwrap();
        let ack = EnvelopeBody::HelloAck(HelloAck {
            control_version: CONTROL_VERSION,
            binary_id: "test".into(),
            capabilities: Vec::new(),
        });
        wire::write_envelope(&mut writer, &Envelope::new(hello.id, ack)).unwrap();
        let query = wire::read_envelope(&mut reader).unwrap().unwrap();
        let EnvelopeBody::Query(Query { what }) = query.body else {
            panic!("a query, got {query:?}");
        };
        wire::write_envelope(&mut writer, &Envelope::new(query.id, reply)).unwrap();
        writer.flush().unwrap();
        what
    })
}

#[test]
fn a_query_is_answered_after_the_handshake() {
    let dir = scratch();
    let socket = dir.join("control.sock");
    let snapshot = serde_json::json!({"sessions": []});
    let server = serve_once(
        UnixListener::bind(&socket).unwrap(),
        EnvelopeBody::Workspace(Workspace {
            snapshot: snapshot.clone(),
        }),
    );
    let reply = query(&socket, "workspace", "test", Duration::from_secs(5)).unwrap();
    assert_eq!(reply, EnvelopeBody::Workspace(Workspace { snapshot }));
    assert_eq!(server.join().unwrap(), "workspace");
}

#[test]
fn an_error_reply_and_a_missing_socket_are_errors() {
    let dir = scratch();
    let socket = dir.join("control.sock");
    let server = serve_once(
        UnixListener::bind(&socket).unwrap(),
        EnvelopeBody::Error(crate::contract::ErrorMessage {
            message: "unknown query `nothing`".into(),
        }),
    );
    assert_eq!(
        query(&socket, "nothing", "test", Duration::from_secs(5)),
        Err("unknown query `nothing`".to_string())
    );
    server.join().unwrap();
    assert!(
        query(
            &dir.join("absent.sock"),
            "workspace",
            "test",
            Duration::from_secs(5)
        )
        .is_err()
    );
}
