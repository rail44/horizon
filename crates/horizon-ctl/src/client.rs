//! The wire client: one connection, a `hello` handshake, then exactly one
//! more request/response round trip per the design doc's v1 shape ("connect
//! -> handshake -> send one request -> wait for the reply -> exit"). Generic
//! over `std::io::{BufRead, Write}` like [`horizon_control::wire`] itself,
//! so tests can drive it against any std transport a stub server exposes
//! (in practice, a real `UnixStream` connected to a real
//! `std::os::unix::net::UnixListener` in `tests/integration.rs` -- there is
//! no need to fake the transport itself, only the process-level concerns in
//! [`crate::run`]).

use std::io::{BufRead, Write};

use horizon_control::contract::{Envelope, EnvelopeBody, Hello, Query, State, CONTROL_VERSION};
use horizon_control::wire::{self, WireError};

use crate::commands::Request;

/// Reported as this build's `Hello::binary_id`, per the task spec
/// ("horizon-ctl <version>").
pub fn binary_id() -> String {
    format!("horizon-ctl {}", env!("CARGO_PKG_VERSION"))
}

/// Everything that can go wrong on the client side of a round trip. Every
/// variant is a runtime/server-interaction failure in [`crate::run`]'s exit
/// code scheme (never a usage error) -- by the time a [`Connection`] exists,
/// argv has already been accepted.
#[derive(Debug)]
pub enum ClientError {
    Wire(WireError),
    ConnectionClosed,
    Rejected(String),
    UnexpectedHandshakeReply(EnvelopeBody),
    /// The server replied with a different `id` than the request carried --
    /// a protocol violation, not something a well-behaved server should
    /// ever produce (see `horizon_control::contract::Envelope`'s doc
    /// comment on request/response correlation).
    IdMismatch {
        expected: u64,
        found: u64,
    },
    ServerError(String),
    UnexpectedReply(EnvelopeBody),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Wire(err) => write!(f, "wire error: {err}"),
            ClientError::ConnectionClosed => write!(f, "connection closed by server"),
            ClientError::Rejected(reason) => write!(f, "rejected by server: {reason}"),
            ClientError::UnexpectedHandshakeReply(body) => {
                write!(f, "unexpected reply to hello handshake: {body:?}")
            }
            ClientError::IdMismatch { expected, found } => write!(
                f,
                "response id mismatch: sent {expected}, received {found} (protocol violation)"
            ),
            ClientError::ServerError(message) => write!(f, "server error: {message}"),
            ClientError::UnexpectedReply(body) => write!(f, "unexpected reply: {body:?}"),
        }
    }
}

impl From<WireError> for ClientError {
    fn from(err: WireError) -> Self {
        ClientError::Wire(err)
    }
}

/// A control-plane connection mid-lifecycle: not yet handshaken, or
/// handshaken and ready for the single follow-up request `horizon-ctl`
/// sends per invocation.
pub struct Connection<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
}

impl<R: BufRead, W: Write> Connection<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
        }
    }

    /// Writes `body` as a freshly-numbered envelope and reads the matching
    /// reply, verifying the `id` echo along the way.
    fn send(&mut self, body: EnvelopeBody) -> Result<Envelope, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        wire::write_envelope(&mut self.writer, &Envelope::new(id, body))?;
        let reply = wire::read_envelope(&mut self.reader)?.ok_or(ClientError::ConnectionClosed)?;
        if reply.id != id {
            return Err(ClientError::IdMismatch {
                expected: id,
                found: reply.id,
            });
        }
        Ok(reply)
    }

    /// Must be called exactly once, before any other request, per the
    /// contract's "Must be the first message sent on a new connection".
    pub fn handshake(&mut self) -> Result<(), ClientError> {
        let reply = self.send(EnvelopeBody::Hello(Hello {
            control_version: CONTROL_VERSION,
            binary_id: binary_id(),
        }))?;
        match reply.body {
            EnvelopeBody::HelloAck(_) => Ok(()),
            EnvelopeBody::Rejected(rejected) => Err(ClientError::Rejected(rejected.reason)),
            other => Err(ClientError::UnexpectedHandshakeReply(other)),
        }
    }

    /// `Query { what: "state" }`, used both for the `state` subcommand and
    /// internally by [`crate::run`] to check `destructive_commands` before a
    /// destructive subcommand runs.
    pub fn query_state(&mut self) -> Result<State, ClientError> {
        match self.send_request(Request::Query(Query {
            what: "state".to_string(),
        }))? {
            EnvelopeBody::State(state) => Ok(state),
            other => Err(ClientError::UnexpectedReply(other)),
        }
    }

    /// Sends `request` and returns the reply body, or a [`ClientError`] --
    /// an [`EnvelopeBody::Error`] reply is folded into
    /// [`ClientError::ServerError`] here so every caller handles it
    /// uniformly rather than re-checking for it themselves.
    pub fn send_request(&mut self, request: Request) -> Result<EnvelopeBody, ClientError> {
        let body = match request {
            Request::Invoke(invoke) => EnvelopeBody::Invoke(invoke),
            Request::Query(query) => EnvelopeBody::Query(query),
        };
        match self.send(body)?.body {
            EnvelopeBody::Error(err) => Err(ClientError::ServerError(err.message)),
            other => Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_control::contract::{ErrorMessage, HelloAck, Rejected, Sessions};
    use std::io::Cursor;

    /// Builds a `Connection` whose "server" is a pre-recorded byte buffer of
    /// envelopes and whose writes go into a throwaway `Vec` -- enough to
    /// exercise `Connection`'s own request/reply/id-echo bookkeeping without
    /// a real socket (the stub-server tests in `tests/integration.rs` cover
    /// the real-transport, real-`horizon-ctl`-binary path).
    fn connection_reading(lines: &[Envelope]) -> Connection<Cursor<Vec<u8>>, Vec<u8>> {
        let mut buf = Vec::new();
        for envelope in lines {
            wire::write_envelope(&mut buf, envelope).unwrap();
        }
        Connection::new(Cursor::new(buf), Vec::new())
    }

    #[test]
    fn handshake_succeeds_on_hello_ack() {
        let mut conn = connection_reading(&[Envelope::new(
            1,
            EnvelopeBody::HelloAck(HelloAck {
                control_version: CONTROL_VERSION,
                binary_id: "horizon 0.1.0".to_string(),
                capabilities: vec![],
            }),
        )]);
        assert!(conn.handshake().is_ok());
    }

    #[test]
    fn handshake_surfaces_rejection_reason() {
        let mut conn = connection_reading(&[Envelope::new(
            1,
            EnvelopeBody::Rejected(Rejected {
                reason: "control version mismatch".to_string(),
            }),
        )]);
        let err = conn.handshake().unwrap_err();
        assert!(
            matches!(err, ClientError::Rejected(reason) if reason == "control version mismatch")
        );
    }

    #[test]
    fn id_mismatch_is_detected() {
        let mut conn = connection_reading(&[Envelope::new(
            99,
            EnvelopeBody::HelloAck(HelloAck {
                control_version: CONTROL_VERSION,
                binary_id: "horizon 0.1.0".to_string(),
                capabilities: vec![],
            }),
        )]);
        let err = conn.handshake().unwrap_err();
        assert!(matches!(
            err,
            ClientError::IdMismatch {
                expected: 1,
                found: 99
            }
        ));
    }

    #[test]
    fn send_request_folds_error_reply_into_server_error() {
        let mut conn = connection_reading(&[Envelope::new(
            1,
            EnvelopeBody::Error(ErrorMessage {
                message: "no such session".to_string(),
            }),
        )]);
        let err = conn
            .send_request(Request::Query(Query {
                what: "sessions".to_string(),
            }))
            .unwrap_err();
        assert!(matches!(err, ClientError::ServerError(m) if m == "no such session"));
    }

    #[test]
    fn query_state_extracts_the_state_payload() {
        let state = State {
            tab_count: 1,
            visible_pane_count: 1,
            has_active_session: false,
            detached_session_count: 0,
            has_pending_approval: false,
            has_turn_in_flight: false,
            destructive_commands: vec!["terminate-session".to_string()],
        };
        let mut conn = connection_reading(&[Envelope::new(1, EnvelopeBody::State(state.clone()))]);
        assert_eq!(conn.query_state().unwrap(), state);
    }

    #[test]
    fn send_request_returns_the_reply_body_on_success() {
        let mut conn = connection_reading(&[Envelope::new(
            1,
            EnvelopeBody::Sessions(Sessions { sessions: vec![] }),
        )]);
        let body = conn
            .send_request(Request::Query(Query {
                what: "sessions".to_string(),
            }))
            .unwrap();
        assert_eq!(body, EnvelopeBody::Sessions(Sessions { sessions: vec![] }));
    }

    #[test]
    fn connection_closed_before_a_reply_is_an_explicit_error() {
        let mut conn = Connection::new(Cursor::new(Vec::new()), Vec::new());
        let err = conn.handshake().unwrap_err();
        assert!(matches!(err, ClientError::ConnectionClosed));
    }
}
