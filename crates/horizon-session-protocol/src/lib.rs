//! Transport-neutral JSONL framing shared by Horizon's session-daemon
//! domains. Agent and terminal commands remain sister vocabularies in their
//! own crates; this crate owns only the envelope, handshake shape, and stream
//! framing they share.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

/// The shared session-daemon envelope and handshake version.
///
/// Version 2 is the existing agent wire version. Extracting its framing into
/// this crate is behavior-preserving.
pub const SESSION_PROTOCOL_VERSION: u32 = 2;

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

/// Sent by either peer during the session-daemon handshake. Capabilities
/// advertise the sister vocabularies the peer can route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub contract_version: u32,
    pub binary_id: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed envelope json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown envelope kind: {0:?}")]
    UnknownKind(String),
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
            payload: serde_json::json!({"ping": null}),
        };
        let (mut client, server) = tokio::io::duplex(4096);
        write_envelope(&mut client, &expected).await.unwrap();

        let mut reader = BufReader::new(server);
        assert_eq!(read_envelope(&mut reader).await.unwrap(), Some(expected));
    }

    #[tokio::test]
    async fn rejects_a_torn_line() {
        let (mut client, server) = tokio::io::duplex(4096);
        client.write_all(b"{\"v\":2").await.unwrap();
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
}
