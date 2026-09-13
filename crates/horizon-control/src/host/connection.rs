//! One control-plane connection: the hello handshake, then a strictly
//! synchronous request/response loop -- v1 has no server-initiated pushes
//! (`docs/cli-control-plane-design.md`'s "v1 operation shapes" decision
//! defers subscriptions to v2), so, unlike `horizon-agentd`'s split reader/
//! writer tasks, one thread reading and writing the same stream in turn is
//! enough.
//!
//! Every protocol decision here routes through [`ControlExecutor`], so this
//! module's own tests exercise the full handshake/request/response logic
//! against a stub executor and in-memory client framing -- no socket, UI
//! thread, or `Workspace` involved. The public [`UnixStream`] adapter remains
//! covered by the listener and CLI integration tests.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use crate::contract::{Envelope, EnvelopeBody, ErrorMessage, HelloAck, Rejected, CONTROL_VERSION};
use crate::wire::{self, WireError};

use super::executor::{ControlExecutor, ControlRequest};

/// Reported in this build's `hello_ack` reply's `binary_id` -- same
/// convention as `horizon-agentd`'s own `BINARY_ID` (the crate version, not
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
    let writer = stream.try_clone()?;
    handle_connection_io(BufReader::new(stream), writer, executor)
}

fn handle_connection_io(
    mut reader: impl BufRead,
    mut writer: impl Write,
    executor: &dyn ControlExecutor,
) -> Result<(), WireError> {
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

    fn stub_executor() -> StubExecutor {
        StubExecutor {
            response: EnvelopeBody::Ok { session_id: None },
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Feeds a complete client transcript through the same connection loop
    /// production uses, then decodes every response it emitted. Reaching EOF
    /// after the final request models a clean peer disconnect without needing
    /// an AF_UNIX socket, which the outer Horizon sandbox deliberately denies.
    fn run_connection(
        executor: &dyn ControlExecutor,
        requests: &[Envelope],
    ) -> (Vec<Envelope>, Result<(), WireError>) {
        let mut input = Vec::new();
        for request in requests {
            wire::write_envelope(&mut input, request).expect("encode client request");
        }

        let mut output = Vec::new();
        let result = handle_connection_io(BufReader::new(input.as_slice()), &mut output, executor);

        let mut responses = Vec::new();
        let mut reader = BufReader::new(output.as_slice());
        while let Some(response) = wire::read_envelope(&mut reader).expect("decode server response")
        {
            responses.push(response);
        }
        (responses, result)
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
        let executor = stub_executor();
        let (responses, result) = run_connection(&executor, &[hello_envelope(1, CONTROL_VERSION)]);

        result.expect("clean disconnect is not an error");
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].id, 1);
        match &responses[0].body {
            EnvelopeBody::HelloAck(ack) => assert_eq!(ack.control_version, CONTROL_VERSION),
            other => panic!("expected HelloAck, got {other:?}"),
        }
    }

    #[test]
    fn handshake_rejects_a_version_mismatch_and_closes() {
        let executor = stub_executor();
        let requests = [
            hello_envelope(1, CONTROL_VERSION + 1),
            Envelope::new(
                2,
                EnvelopeBody::Query(Query {
                    what: "state".to_string(),
                }),
            ),
        ];
        let (responses, result) = run_connection(&executor, &requests);

        result.expect("a rejected handshake is not itself a wire error");
        assert_eq!(
            responses.len(),
            1,
            "the request after rejection must be ignored"
        );
        assert_eq!(responses[0].id, 1);
        assert!(
            matches!(responses[0].body, EnvelopeBody::Rejected(_)),
            "expected Rejected, got {:?}",
            responses[0].body
        );
        assert!(executor.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn handshake_rejects_a_non_hello_first_message() {
        let executor = stub_executor();
        let (responses, result) = run_connection(
            &executor,
            &[Envelope::new(
                1,
                EnvelopeBody::Query(Query {
                    what: "state".to_string(),
                }),
            )],
        );

        result.expect("rejection is not an error");
        assert_eq!(responses.len(), 1);
        assert!(matches!(responses[0].body, EnvelopeBody::Rejected(_)));
    }

    #[test]
    fn invoke_after_handshake_is_forwarded_to_the_executor_with_the_id_echoed() {
        let executor = stub_executor();
        let requests = [
            hello_envelope(1, CONTROL_VERSION),
            Envelope::new(
                42,
                EnvelopeBody::Invoke(Invoke {
                    command: "new-terminal".to_string(),
                    args: serde_json::json!({}),
                }),
            ),
        ];
        let (responses, result) = run_connection(&executor, &requests);

        result.unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[1].id, 42);
        assert!(matches!(
            responses[1].body,
            EnvelopeBody::Ok { session_id: None }
        ));
        assert!(matches!(
            executor.seen.lock().unwrap().as_slice(),
            [ControlRequest::Invoke(invoke)] if invoke.command == "new-terminal"
        ));
    }

    #[test]
    fn query_after_handshake_is_forwarded_to_the_executor() {
        let executor = stub_executor();
        let requests = [
            hello_envelope(1, CONTROL_VERSION),
            Envelope::new(
                7,
                EnvelopeBody::Query(Query {
                    what: "sessions".to_string(),
                }),
            ),
        ];
        let (responses, result) = run_connection(&executor, &requests);

        result.unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[1].id, 7);
        assert!(matches!(
            executor.seen.lock().unwrap().as_slice(),
            [ControlRequest::Query(query)] if query.what == "sessions"
        ));
    }

    #[test]
    fn an_unrecognized_request_kind_gets_an_error_reply_without_closing_the_connection() {
        let executor = stub_executor();
        // A second `hello` mid-stream is neither `Invoke` nor `Query` --
        // the request loop must answer it with an error, not hang or drop
        // the connection.
        let requests = [
            hello_envelope(1, CONTROL_VERSION),
            hello_envelope(2, CONTROL_VERSION),
            Envelope::new(
                3,
                EnvelopeBody::Query(Query {
                    what: "state".to_string(),
                }),
            ),
        ];
        let (responses, result) = run_connection(&executor, &requests);

        result.unwrap();
        assert_eq!(responses.len(), 3);
        assert!(matches!(responses[1].body, EnvelopeBody::Error(_)));
        assert_eq!(responses[2].id, 3);
        assert!(matches!(
            responses[2].body,
            EnvelopeBody::Ok { session_id: None }
        ));
    }

    #[test]
    fn multiple_requests_each_get_their_own_id_echoed_in_order() {
        let executor = stub_executor();
        let mut requests = vec![hello_envelope(1, CONTROL_VERSION)];
        for id in [10_u64, 20, 30] {
            requests.push(Envelope::new(
                id,
                EnvelopeBody::Query(Query {
                    what: "state".to_string(),
                }),
            ));
        }
        let (responses, result) = run_connection(&executor, &requests);

        result.unwrap();
        assert_eq!(responses.len(), 4);
        let ids: Vec<_> = responses
            .iter()
            .skip(1)
            .map(|response| response.id)
            .collect();
        assert_eq!(ids, [10, 20, 30]);
    }

    #[test]
    fn clean_disconnect_after_handshake_ends_the_loop_without_error() {
        let executor = stub_executor();
        let (responses, result) = run_connection(&executor, &[hello_envelope(1, CONTROL_VERSION)]);

        result.expect("a peer closing after handshake is not a wire error");
        assert_eq!(responses.len(), 1);
    }
}
