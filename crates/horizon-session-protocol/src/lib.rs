//! Transport-neutral JSONL framing shared by Horizon's session-daemon
//! domains. Agent and terminal commands remain sister vocabularies in their
//! own crates; this crate owns only the envelope, handshake shape, and stream
//! framing they share.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

/// The shared session-daemon envelope and handshake version.
///
/// Version 4 adds correlated terminal discovery and attach controls; attach
/// changed shape, so older peers cannot safely decode the terminal vocabulary.
///
/// Version 5: `TerminalSpan`'s `fg`/`bg` now carry `horizon-terminal-core`'s
/// own `TerminalColor`/`NamedColor` enums instead of a re-exported
/// `alacritty_terminal::vte::ansi::Color`/`NamedColor` — same role, different
/// wire shape (variant names/order changed, `Spec(Rgb)` became `Rgb([u8;
/// 3])`), so a stale daemon/UI pair must fail the handshake rather than
/// misdecode a frame's colors.
///
/// Version 6: `Hello` drops the dead `capabilities` field (owner decision,
/// 2026-07-18) -- every sender hardcoded `["agent", "terminal"]` and the
/// only reader was a test assertion, so it was forward-compat weight with
/// no actual use. Removing a field changes the wire shape, so a stale
/// peer sending the old shape must fail the handshake rather than
/// misdecode.
///
/// Version 7: one frame-vocabulary bump carrying three extensions together
/// (resolving `docs/terminal-protocol-goals.md`'s open question of whether
/// they land as one bump or two):
/// - `TerminalSpan` gains text-style attributes -- `italic`,
///   `strikethrough`, `underline` (single/double/curl/dotted/dashed), and
///   the SGR 58 `underline_color` (backlog #44).
/// - Selection becomes semantic frame metadata: `TerminalFrame::selection`
///   (viewport-space, inclusive endpoints, window-clamped) with the
///   cursor's nested-`Option` diff idiom, replacing the literal RGB
///   highlight previously baked into selected spans' `fg`/`bg` (goal 2).
/// - `TerminalCursor` gains its DECSCUSR `shape`
///   (block/underline/beam/hollow-block); a DECTCEM-hidden cursor is now
///   `cursor: None` on the wire instead of a stale always-visible block.
pub const SESSION_PROTOCOL_VERSION: u32 = 7;

pub const SESSION_CONTROL_KIND: &str = "session_control";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionControl {
    Hello(Hello),
    HandshakeRejected(String),
    Ping,
    Pong,
    Drain,
}

/// A domain-neutral session-daemon envelope. `kind` selects a sister
/// vocabulary; `payload` is decoded by that domain only after the shared
/// version check succeeds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    pub kind: String,
    pub payload: serde_json::Value,
}

impl Envelope {
    pub fn from_typed<T: Serialize>(
        kind: &str,
        session_id: Option<Uuid>,
        payload: &T,
    ) -> Result<Self, WireError> {
        Ok(Self {
            v: SESSION_PROTOCOL_VERSION,
            session_id,
            kind: kind.to_string(),
            payload: serde_json::to_value(payload)?,
        })
    }

    pub fn decode_payload<T>(&self, expected_kind: &str) -> Result<T, WireError>
    where
        T: serde::de::DeserializeOwned,
    {
        if self.kind != expected_kind {
            return Err(WireError::UnexpectedKind {
                expected: expected_kind.to_string(),
                found: self.kind.clone(),
            });
        }
        Ok(serde_json::from_value(self.payload.clone())?)
    }

    pub fn session_control(control: &SessionControl) -> Result<Self, WireError> {
        Self::from_typed(SESSION_CONTROL_KIND, None, control)
    }
}

/// Sent by either peer during the session-daemon handshake.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub contract_version: u32,
    pub binary_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed envelope json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown envelope kind: {0:?}")]
    UnknownKind(String),
    #[error("unexpected envelope kind: expected {expected:?}, found {found:?}")]
    UnexpectedKind { expected: String, found: String },
    #[error("torn line: connection closed mid-message")]
    TornLine,
    #[error("envelope wire version mismatch: this build speaks v{expected}, received v{found}")]
    VersionMismatch { expected: u32, found: u32 },
}

pub async fn write_envelope<W>(writer: &mut W, envelope: &Envelope) -> Result<(), WireError>
where
    W: AsyncWrite + Unpin,
{
    let mut line = serde_json::to_string(envelope)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_envelope<R>(reader: &mut R) -> Result<Option<Envelope>, WireError>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line).await?;
    if bytes_read == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') {
        return Err(WireError::TornLine);
    }

    let envelope: Envelope = serde_json::from_str(line.trim_end_matches(['\n', '\r']))?;
    if envelope.v != SESSION_PROTOCOL_VERSION {
        return Err(WireError::VersionMismatch {
            expected: SESSION_PROTOCOL_VERSION,
            found: envelope.v,
        });
    }
    Ok(Some(envelope))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn round_trips_a_domain_neutral_envelope() {
        let expected = Envelope {
            v: SESSION_PROTOCOL_VERSION,
            session_id: Some(Uuid::nil()),
            kind: "agent_control".to_string(),
            payload: serde_json::json!("ping"),
        };
        let (mut client, server) = tokio::io::duplex(4096);
        write_envelope(&mut client, &expected).await.unwrap();

        let mut reader = BufReader::new(server);
        assert_eq!(read_envelope(&mut reader).await.unwrap(), Some(expected));
    }

    #[tokio::test]
    async fn rejects_a_torn_line() {
        let (mut client, server) = tokio::io::duplex(4096);
        client.write_all(b"{\"v\":3").await.unwrap();
        client.shutdown().await.unwrap();

        let result = read_envelope(&mut BufReader::new(server)).await;
        assert!(matches!(result, Err(WireError::TornLine)));
    }

    #[tokio::test]
    async fn rejects_a_different_version_before_domain_decoding() {
        let (mut client, server) = tokio::io::duplex(4096);
        client
            .write_all(b"{\"v\":99,\"session_id\":null,\"kind\":\"future\",\"payload\":{}}\n")
            .await
            .unwrap();

        let result = read_envelope(&mut BufReader::new(server)).await;
        assert!(matches!(
            result,
            Err(WireError::VersionMismatch {
                expected: SESSION_PROTOCOL_VERSION,
                found: 99
            })
        ));
    }

    #[test]
    fn typed_helpers_validate_kind_before_decoding() {
        let envelope = Envelope::session_control(&SessionControl::Ping).unwrap();
        assert_eq!(
            envelope
                .decode_payload::<SessionControl>(SESSION_CONTROL_KIND)
                .unwrap(),
            SessionControl::Ping
        );
        assert!(matches!(
            envelope.decode_payload::<SessionControl>("agent_control"),
            Err(WireError::UnexpectedKind { .. })
        ));
    }
}
