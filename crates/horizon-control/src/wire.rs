//! Newline-delimited JSON framing for [`crate::contract::Envelope`], over
//! plain synchronous `std::io` -- see this crate's `lib.rs` doc comment for
//! why (no tokio: the future in-process listener runs on a dedicated OS
//! thread, and CLI clients are synchronous processes).
//!
//! Generic over `std::io::{Write, BufRead}` rather than any concrete
//! transport (a Unix socket, a pipe, an in-memory buffer in tests) --
//! mirrors `horizon-agent::wire`'s guardrail of staying generic over
//! `tokio::io::{AsyncBufRead, AsyncWrite}` for the same reason: nothing
//! here should have to change if the future session daemon accepts
//! connections differently than the in-process listener does today.

use std::io::{BufRead, Write};

use serde::Deserialize;

use crate::contract::{
    Envelope, EnvelopeBody, ErrorMessage, Hello, HelloAck, Invoke, ProfileSnapshot, Query,
    Rejected, Sessions, State, CONTROL_VERSION,
};

/// Explicit, non-panicking failure modes for [`read_envelope`]. Unlike
/// `horizon-agent::wire::WireError`, there is no `UnknownKind` variant here:
/// an unrecognized `kind` is not a read failure in this contract, it decodes
/// to [`EnvelopeBody::Unknown`] instead (see that variant's doc comment) --
/// the forward-compatibility guarantee the design doc asks for.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed envelope json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("torn line: connection closed mid-message")]
    TornLine,
    #[error("envelope wire version mismatch: this build speaks v{expected}, received v{found}")]
    VersionMismatch { expected: u32, found: u32 },
}

/// Writes one envelope as a single newline-terminated JSON line and flushes
/// the writer, so a peer reading line-by-line (e.g. [`read_envelope`]) sees
/// it immediately rather than waiting on a fuller buffer.
pub fn write_envelope(writer: &mut impl Write, envelope: &Envelope) -> Result<(), WireError> {
    let mut line = serde_json::to_string(envelope)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()?;
    Ok(())
}

/// Reads one newline-delimited envelope. `Ok(None)` means the peer closed
/// the connection cleanly between messages (0 bytes read, no partial line
/// pending); a partial line with no trailing newline (peer closed
/// mid-message) is [`WireError::TornLine`], never silently treated as a
/// complete (truncated) message.
pub fn read_envelope(reader: &mut impl BufRead) -> Result<Option<Envelope>, WireError> {
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line)?;
    if bytes_read == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') {
        return Err(WireError::TornLine);
    }
    parse_line(line.trim_end_matches(['\n', '\r'])).map(Some)
}

/// Unknown top-level fields are tolerated for free (no `deny_unknown_fields`
/// on `RawEnvelope` or any payload type above); this function's own job is
/// the version check ([`WireError::VersionMismatch`]) that must happen
/// *before* `payload` is decoded into a specific type, plus routing on
/// `kind` -- an unrecognized `kind` becomes [`EnvelopeBody::Unknown`] rather
/// than an error (see that variant's doc comment).
fn parse_line(line: &str) -> Result<Envelope, WireError> {
    #[derive(Deserialize)]
    struct RawEnvelope {
        v: u32,
        id: u64,
        kind: String,
        #[serde(default)]
        payload: serde_json::Value,
    }

    let raw: RawEnvelope = serde_json::from_str(line)?;
    if raw.v != CONTROL_VERSION {
        return Err(WireError::VersionMismatch {
            expected: CONTROL_VERSION,
            found: raw.v,
        });
    }

    let body = match raw.kind.as_str() {
        "hello" => EnvelopeBody::Hello(serde_json::from_value::<Hello>(raw.payload)?),
        "invoke" => EnvelopeBody::Invoke(serde_json::from_value::<Invoke>(raw.payload)?),
        "query" => EnvelopeBody::Query(serde_json::from_value::<Query>(raw.payload)?),
        "hello_ack" => EnvelopeBody::HelloAck(serde_json::from_value::<HelloAck>(raw.payload)?),
        "rejected" => EnvelopeBody::Rejected(serde_json::from_value::<Rejected>(raw.payload)?),
        "ok" => EnvelopeBody::Ok,
        "error" => EnvelopeBody::Error(serde_json::from_value::<ErrorMessage>(raw.payload)?),
        "sessions" => EnvelopeBody::Sessions(serde_json::from_value::<Sessions>(raw.payload)?),
        "state" => EnvelopeBody::State(serde_json::from_value::<State>(raw.payload)?),
        "profile" => EnvelopeBody::Profile(serde_json::from_value::<ProfileSnapshot>(raw.payload)?),
        other => EnvelopeBody::Unknown {
            kind: other.to_string(),
            payload: raw.payload,
        },
    };

    Ok(Envelope {
        v: raw.v,
        id: raw.id,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{ProfileFrameEntry, SessionEntry};
    use std::io::BufReader;

    fn sample_bodies() -> Vec<EnvelopeBody> {
        vec![
            EnvelopeBody::Hello(Hello {
                control_version: CONTROL_VERSION,
                binary_id: "0.1.0".to_string(),
            }),
            EnvelopeBody::Invoke(Invoke {
                command: "new-terminal".to_string(),
                args: serde_json::json!({}),
            }),
            EnvelopeBody::Query(Query {
                what: "sessions".to_string(),
            }),
            EnvelopeBody::HelloAck(HelloAck {
                control_version: CONTROL_VERSION,
                binary_id: "0.1.0".to_string(),
                capabilities: vec!["sessions".to_string(), "state".to_string()],
            }),
            EnvelopeBody::Rejected(Rejected {
                reason: "control version mismatch".to_string(),
            }),
            EnvelopeBody::Ok,
            EnvelopeBody::Error(ErrorMessage {
                message: "no such session".to_string(),
            }),
            EnvelopeBody::Sessions(Sessions {
                sessions: vec![SessionEntry {
                    session_id: "s-1".to_string(),
                    kind: "agent".to_string(),
                    attached: true,
                    title: "agent: fix bug".to_string(),
                }],
            }),
            EnvelopeBody::State(State {
                tab_count: 2,
                visible_pane_count: 3,
                has_active_session: true,
                detached_session_count: 1,
                has_pending_approval: false,
                has_turn_in_flight: true,
                destructive_commands: vec![
                    "terminate-active-session".to_string(),
                    "terminate-all-detached-sessions".to_string(),
                ],
            }),
            EnvelopeBody::Profile(ProfileSnapshot {
                enabled: true,
                log_path: "/tmp/ui-profile.jsonl".to_string(),
                frames: vec![ProfileFrameEntry {
                    trigger: "KeyDown".to_string(),
                    duration_us: 1234,
                    created_at_unix_ms: 1_700_000_000_000,
                }],
            }),
        ]
    }

    #[test]
    fn round_trips_every_known_body_kind_with_id_echoed() {
        let envelopes: Vec<Envelope> = sample_bodies()
            .into_iter()
            .enumerate()
            .map(|(i, body)| Envelope::new(i as u64, body))
            .collect();

        let mut buf: Vec<u8> = Vec::new();
        for envelope in &envelopes {
            write_envelope(&mut buf, envelope).unwrap();
        }

        let mut reader = BufReader::new(buf.as_slice());
        let mut received = Vec::new();
        while let Some(envelope) = read_envelope(&mut reader).unwrap() {
            received.push(envelope);
        }

        assert_eq!(received, envelopes);
        // The id is caller-assigned and simply carried through -- this is
        // the "echo" contract a server implements by reusing the request's
        // id on its response, not something this crate enforces itself.
        for (i, envelope) in received.iter().enumerate() {
            assert_eq!(envelope.id, i as u64);
        }
    }

    #[test]
    fn wire_version_mismatch_is_an_explicit_error() {
        let mut reader = BufReader::new(
            b"{\"v\":99,\"id\":1,\"kind\":\"query\",\"payload\":{\"what\":\"state\"}}\n".as_slice(),
        );

        let result = read_envelope(&mut reader);
        assert!(
            matches!(
                result,
                Err(WireError::VersionMismatch {
                    expected: CONTROL_VERSION,
                    found: 99
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn unknown_kind_decodes_to_the_forward_compat_variant_not_an_error() {
        let mut reader = BufReader::new(
            b"{\"v\":1,\"id\":7,\"kind\":\"subscribe\",\"payload\":{\"topic\":\"events\"}}\n"
                .as_slice(),
        );

        let envelope = read_envelope(&mut reader)
            .unwrap()
            .expect("envelope should parse despite the unrecognized kind");
        assert_eq!(envelope.id, 7);
        match envelope.body {
            EnvelopeBody::Unknown { kind, payload } => {
                assert_eq!(kind, "subscribe");
                assert_eq!(payload, serde_json::json!({"topic": "events"}));
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn clean_disconnect_between_messages_is_not_an_error() {
        let mut reader = BufReader::new(b"".as_slice());
        let result = read_envelope(&mut reader);
        assert!(matches!(result, Ok(None)), "{result:?}");
    }

    #[test]
    fn torn_line_without_trailing_newline_is_an_explicit_error() {
        let mut reader = BufReader::new(b"{\"v\":1,\"id\":1,\"kind\":\"query\"".as_slice());
        let result = read_envelope(&mut reader);
        assert!(matches!(result, Err(WireError::TornLine)), "{result:?}");
    }

    #[test]
    fn malformed_json_is_an_explicit_error() {
        let mut reader = BufReader::new(b"not json at all\n".as_slice());
        let result = read_envelope(&mut reader);
        assert!(matches!(result, Err(WireError::Json(_))), "{result:?}");
    }

    #[test]
    fn unknown_top_level_fields_are_tolerated() {
        let mut reader = BufReader::new(
            b"{\"v\":1,\"id\":3,\"kind\":\"ok\",\"payload\":null,\"future_field\":42}\n".as_slice(),
        );

        let envelope = read_envelope(&mut reader)
            .unwrap()
            .expect("envelope should parse despite the unrecognized field");
        assert_eq!(envelope.id, 3);
        assert_eq!(envelope.body, EnvelopeBody::Ok);
    }
}
