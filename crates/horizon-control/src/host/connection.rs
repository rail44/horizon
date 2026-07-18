//! One control-plane connection: the hello handshake, then a strictly
//! synchronous request/response loop -- v1 has no server-initiated pushes
//! (`docs/cli-control-plane-design.md`'s "v1 operation shapes" decision
//! defers subscriptions to v2), so, unlike `horizon-sessiond`'s split reader/
//! writer tasks, one thread reading and writing the same stream in turn is
//! enough.
//!
//! Every protocol decision here routes through [`ControlExecutor`], so this
//! module's own tests exercise the full handshake/request/response logic
//! against a stub executor and a real [`UnixStream`] pair -- no floem, UI
//! thread, or `Workspace` involved at all (the mission's "統合テストはスタ
//! ブ実行側+本物の horizon-control クライアントフレーミングで書く").

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use crate::contract::{Envelope, EnvelopeBody, ErrorMessage, HelloAck, Rejected, CONTROL_VERSION};
use crate::wire::{self, WireError};

use super::executor::{ControlExecutor, ControlRequest};

/// Reported in this build's `hello_ack` reply's `binary_id` -- same
/// convention as `horizon-sessiond`'s own `BINARY_ID` (the crate version, not
/// the semantic `control_version`, which travels separately).
const BINARY_ID: &str = env!("CARGO_PKG_VERSION");

/// Handles one accepted connection end to end: hello handshake, then answers
/// every `Invoke`/`Query` it reads via `executor` until the peer disconnects
/// or a malformed message forces the connection closed. Never panics on a
/// misbehaving peer -- every failure mode is a [`WireError`] the caller
/// ([`super::listener::spawn`]'s per-connection thread) logs and moves on
/// from, exactly like every other connection.
pub fn handle_connection(
    stream: UnixStream,
    executor: &dyn ControlExecutor,
) -> Result<(), WireError> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    if !handshake(&mut reader, &mut writer)? {
        return Ok(());
    }

    loop {
        let Some(envelope) = wire::read_envelope(&mut reader)? else {
            return Ok(());
        };

        let body = match envelope.body {
            EnvelopeBody::Invoke(invoke) => executor.execute(ControlRequest::Invoke(invoke)),
            EnvelopeBody::Query(query) => executor.execute(ControlRequest::Query(query)),
            _ => EnvelopeBody::Error(ErrorMessage {
                message: "expected an invoke or query request".to_string(),
            }),
        };
        wire::write_envelope(&mut writer, &Envelope::new(envelope.id, body))?;
    }
}

/// The first exchange on every connection: `Ok(true)` means it succeeded and
/// [`handle_connection`]'s request loop should start; `Ok(false)` means the
/// peer's hello was rejected (a reply was already sent) and the connection
/// should simply close -- the design doc's "server closes the connection
/// after this is sent".
fn handshake(reader: &mut impl BufRead, writer: &mut impl Write) -> Result<bool, WireError> {
    let Some(envelope) = wire::read_envelope(reader)? else {
        return Ok(false);
    };

    let EnvelopeBody::Hello(hello) = envelope.body else {
        reject(writer, envelope.id, "expected hello as the first message")?;
        return Ok(false);
    };

    if hello.control_version != CONTROL_VERSION {
        reject(
            writer,
            envelope.id,
            &format!(
                "control version mismatch: horizon speaks v{CONTROL_VERSION}, client sent v{}",
                hello.control_version
            ),
        )?;
        return Ok(false);
    }

    wire::write_envelope(
        writer,
        &Envelope::new(
            envelope.id,
            EnvelopeBody::HelloAck(HelloAck {
                control_version: CONTROL_VERSION,
                binary_id: BINARY_ID.to_string(),
                capabilities: vec!["sessions".to_string(), "state".to_string()],
            }),
        ),
    )?;
    Ok(true)
}

fn reject(writer: &mut impl Write, id: u64, reason: &str) -> Result<(), WireError> {
    wire::write_envelope(
        writer,
        &Envelope::new(
            id,
            EnvelopeBody::Rejected(Rejected {
                reason: reason.to_string(),
            }),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Hello, Invoke, Query};
    use std::thread;

    /// A stub [`ControlExecutor`] that always answers with the same
    /// pre-baked response, recording every request it was asked to answer so
    /// a test can assert on what actually reached it.
    struct StubExecutor {
        response: EnvelopeBody,
        seen: std::sync::Mutex<Vec<ControlRequest>>,
    }

    impl ControlExecutor for StubExecutor {
        fn execute(&self, request: ControlRequest) -> EnvelopeBody {
            self.seen.lock().unwrap().push(request);
            self.response.clone()
        }
    }

    /// Runs `handle_connection` against one end of a [`UnixStream::pair`] on
    /// a background thread, handing the test the other end (already wrapped
    /// for line-oriented request/response) plus a join handle to observe the
    /// connection's own `Result` once the test closes its end.
    fn spawn_connection(
        executor: &'static (dyn ControlExecutor + Send + Sync),
    ) -> (UnixStream, thread::JoinHandle<Result<(), WireError>>) {
        let (client, server) = UnixStream::pair().expect("unix socket pair");
        let handle = thread::spawn(move || handle_connection(server, executor));
        (client, handle)
    }

    fn send(client: &mut UnixStream, envelope: &Envelope) {
        wire::write_envelope(client, envelope).expect("write to client stream");
    }

    fn recv(client: &mut UnixStream) -> Envelope {
        let mut reader = BufReader::new(client.try_clone().expect("clone client stream"));
        wire::read_envelope(&mut reader)
            .expect("read from client stream")
            .expect("connection should not have closed")
    }

    fn hello_envelope(id: u64, control_version: u32) -> Envelope {
        Envelope::new(
            id,
            EnvelopeBody::Hello(Hello {
                control_version,
                binary_id: "test-client".to_string(),
            }),
        )
    }

    #[test]
    fn handshake_acks_a_matching_control_version() {
        static EXECUTOR: StubExecutor = StubExecutor {
            response: EnvelopeBody::Ok,
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let (mut client, handle) = spawn_connection(&EXECUTOR);

        send(&mut client, &hello_envelope(1, CONTROL_VERSION));
        let reply = recv(&mut client);

        assert_eq!(reply.id, 1);
        match reply.body {
            EnvelopeBody::HelloAck(ack) => assert_eq!(ack.control_version, CONTROL_VERSION),
            other => panic!("expected HelloAck, got {other:?}"),
        }

        drop(client);
        handle
            .join()
            .unwrap()
            .expect("clean disconnect is not an error");
    }

    #[test]
    fn handshake_rejects_a_version_mismatch_and_closes() {
        static EXECUTOR: StubExecutor = StubExecutor {
            response: EnvelopeBody::Ok,
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let (mut client, handle) = spawn_connection(&EXECUTOR);

        send(&mut client, &hello_envelope(1, CONTROL_VERSION + 1));
        let reply = recv(&mut client);

        assert_eq!(reply.id, 1);
        assert!(
            matches!(reply.body, EnvelopeBody::Rejected(_)),
            "expected Rejected, got {:?}",
            reply.body
        );
        handle
            .join()
            .unwrap()
            .expect("a rejected handshake is not itself a wire error");
    }

    #[test]
    fn handshake_rejects_a_non_hello_first_message() {
        static EXECUTOR: StubExecutor = StubExecutor {
            response: EnvelopeBody::Ok,
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let (mut client, handle) = spawn_connection(&EXECUTOR);

        send(
            &mut client,
            &Envelope::new(
                1,
                EnvelopeBody::Query(Query {
                    what: "state".to_string(),
                }),
            ),
        );
        let reply = recv(&mut client);

        assert!(matches!(reply.body, EnvelopeBody::Rejected(_)));
        handle.join().unwrap().expect("rejection is not an error");
    }

    #[test]
    fn invoke_after_handshake_is_forwarded_to_the_executor_with_the_id_echoed() {
        static EXECUTOR: StubExecutor = StubExecutor {
            response: EnvelopeBody::Ok,
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let (mut client, handle) = spawn_connection(&EXECUTOR);
        send(&mut client, &hello_envelope(1, CONTROL_VERSION));
        recv(&mut client);

        send(
            &mut client,
            &Envelope::new(
                42,
                EnvelopeBody::Invoke(Invoke {
                    command: "new-terminal".to_string(),
                    args: serde_json::json!({}),
                }),
            ),
        );
        let reply = recv(&mut client);

        assert_eq!(reply.id, 42);
        assert!(matches!(reply.body, EnvelopeBody::Ok));
        assert!(matches!(
            EXECUTOR.seen.lock().unwrap().as_slice(),
            [ControlRequest::Invoke(invoke)] if invoke.command == "new-terminal"
        ));

        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn query_after_handshake_is_forwarded_to_the_executor() {
        static EXECUTOR: StubExecutor = StubExecutor {
            response: EnvelopeBody::Ok,
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let (mut client, handle) = spawn_connection(&EXECUTOR);
        send(&mut client, &hello_envelope(1, CONTROL_VERSION));
        recv(&mut client);

        send(
            &mut client,
            &Envelope::new(
                7,
                EnvelopeBody::Query(Query {
                    what: "sessions".to_string(),
                }),
            ),
        );
        let reply = recv(&mut client);

        assert_eq!(reply.id, 7);
        assert!(matches!(
            EXECUTOR.seen.lock().unwrap().as_slice(),
            [ControlRequest::Query(query)] if query.what == "sessions"
        ));

        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn an_unrecognized_request_kind_gets_an_error_reply_without_closing_the_connection() {
        static EXECUTOR: StubExecutor = StubExecutor {
            response: EnvelopeBody::Ok,
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let (mut client, handle) = spawn_connection(&EXECUTOR);
        send(&mut client, &hello_envelope(1, CONTROL_VERSION));
        recv(&mut client);

        // A second `hello` mid-stream is neither `Invoke` nor `Query` --
        // the request loop must answer it with an error, not hang or drop
        // the connection.
        send(&mut client, &hello_envelope(2, CONTROL_VERSION));
        let reply = recv(&mut client);
        assert!(matches!(reply.body, EnvelopeBody::Error(_)));

        // The connection must still be alive: a normal request afterward
        // gets served as usual.
        send(
            &mut client,
            &Envelope::new(
                3,
                EnvelopeBody::Query(Query {
                    what: "state".to_string(),
                }),
            ),
        );
        let reply = recv(&mut client);
        assert_eq!(reply.id, 3);
        assert!(matches!(reply.body, EnvelopeBody::Ok));

        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn multiple_requests_each_get_their_own_id_echoed_in_order() {
        static EXECUTOR: StubExecutor = StubExecutor {
            response: EnvelopeBody::Ok,
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let (mut client, handle) = spawn_connection(&EXECUTOR);
        send(&mut client, &hello_envelope(1, CONTROL_VERSION));
        recv(&mut client);

        for id in [10_u64, 20, 30] {
            send(
                &mut client,
                &Envelope::new(
                    id,
                    EnvelopeBody::Query(Query {
                        what: "state".to_string(),
                    }),
                ),
            );
            let reply = recv(&mut client);
            assert_eq!(reply.id, id);
        }

        drop(client);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn clean_disconnect_after_handshake_ends_the_loop_without_error() {
        static EXECUTOR: StubExecutor = StubExecutor {
            response: EnvelopeBody::Ok,
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let (mut client, handle) = spawn_connection(&EXECUTOR);
        send(&mut client, &hello_envelope(1, CONTROL_VERSION));
        recv(&mut client);

        drop(client);
        handle
            .join()
            .unwrap()
            .expect("a peer closing after handshake is not a wire error");
    }
}
