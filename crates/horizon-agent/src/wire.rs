//! The JSONL wire envelope for `horizon-agentd`'s socket protocol -- see
//! `docs/agent-runtime-split-design.md`'s decision 4 and ACP guardrails 1-2.
//!
//! **Guardrail 1 (contract ≠ wire)**: this module references
//! [`crate::contract`] types (`Command`, `Event`, `SessionId`, ...) to build
//! the envelope shape; nothing in `contract` references this module. An ACP
//! adapter is a second binding beside this one, translating JSON-RPC to the
//! same contract types.
//!
//! **Guardrail 2 (framing over any stream)**: [`read_envelope`]/
//! [`write_envelope`] are generic over `tokio::io::{AsyncBufRead,
//! AsyncWrite}` -- nothing here names `UnixStream` or any other concrete
//! transport. Callers (`horizon-agentd`'s connection handler, Horizon's
//! `agent::agentd_client`) wrap whatever socket/pipe they have (typically
//! `tokio::io::BufReader::new(unix_stream_read_half)` for the read side)
//! and pass it in here.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::contract::{Command, Event, ProviderId, RequestId, SessionId};

/// The contract/wire version this build speaks. Carried both by every
/// envelope's `v` field (checked structurally by [`read_envelope`] -- an
/// envelope from an incompatible wire format can't be assumed to parse into
/// today's `Command`/`Event`/[`Control`] shapes) and, independently, by
/// [`Hello::contract_version`] (the semantic version compared during the
/// hello handshake -- see `horizon-agentd`'s connection handler and
/// Horizon's `agent::agentd_client`). The two checks are deliberately
/// separate: a future transport without an envelope at all (ACP's JSON-RPC
/// over stdio, guardrail 2) has no `v` field to check, so `Hello` carries
/// its own copy that survives independently of this envelope format.
pub const CONTRACT_VERSION: u32 = 1;

/// One JSONL message: `{"v":1,"session_id":..,"kind":"command"|"event"|
/// "control","payload":..}`. `session_id` is `None` for connection-global
/// control messages (`hello`, `ping`, `drain`, `session_list`) and `Some`
/// for anything scoped to one agent session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Envelope {
    pub v: u32,
    pub session_id: Option<SessionId>,
    #[serde(flatten)]
    pub body: EnvelopeBody,
}

impl Envelope {
    pub fn command(session_id: SessionId, command: Command) -> Self {
        Self {
            v: CONTRACT_VERSION,
            session_id: Some(session_id),
            body: EnvelopeBody::Command(command),
        }
    }

    pub fn event(session_id: SessionId, event: Event) -> Self {
        Self {
            v: CONTRACT_VERSION,
            session_id: Some(session_id),
            body: EnvelopeBody::Event(event),
        }
    }

    /// A connection-global control message (`session_id: None`). Construct
    /// [`Envelope`] directly (struct literal -- every field is `pub`) for a
    /// control message scoped to one session, e.g. a future session-bound
    /// host-tool exchange.
    pub fn control(control: Control) -> Self {
        Self {
            v: CONTRACT_VERSION,
            session_id: None,
            body: EnvelopeBody::Control(control),
        }
    }
}

/// The envelope's `kind`/`payload` pair. Serializes adjacently tagged
/// (`{"kind":"command","payload":{..}}`) via the derive below; deserializing
/// needs the version check *before* picking which contract type to decode
/// `payload` as, so reading is hand-rolled in [`parse_line`] rather than
/// derived -- see [`WireError::UnknownKind`]/[`WireError::VersionMismatch`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum EnvelopeBody {
    Command(Command),
    Event(Event),
    Control(Control),
}

/// Control-message payloads (decision 4 in the design doc), plus
/// [`Control::HandshakeRejected`] -- a step-2 addition, not in the
/// original design list; see `docs/agent-runtime-split-design.md`'s "Step 2
/// implementation notes" for why.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    Hello(Hello),
    SessionList,
    SessionListResult(Vec<SessionSummary>),
    SessionNew(SessionNew),
    SessionLoad(SessionLoad),
    HostToolRequest(HostToolRequest),
    HostToolResponse(HostToolResponse),
    Ping,
    Pong,
    Drain,
    /// Sent instead of a normal [`Control::Hello`] reply when the peer's
    /// hello can't be honored (currently: a `contract_version` mismatch) --
    /// carries a human-readable reason so the receiving side can build an
    /// error string directly from the wire without re-deriving it. See
    /// `horizon-agentd`'s connection handler (sender) and
    /// `agent::agentd_client::handshake` in the `horizon` crate (receiver).
    HandshakeRejected(String),
}

/// Sent by whichever side speaks first (Horizon's `agentd_client` connects
/// and sends this; `horizon-agentd` answers with its own, or with
/// [`Control::HandshakeRejected`]). `contract_version` is the semantic
/// contract version -- see [`CONTRACT_VERSION`]'s doc comment; a mismatch is
/// the handshake's job to detect and surface, not this module's.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub contract_version: u32,
    pub binary_id: String,
    pub capabilities: Vec<String>,
}

/// One entry of a [`Control::SessionListResult`] reply.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
}

/// Per `docs/agent-runtime-split-design.md` guardrail 5, `session_new` is
/// distinct from `session_load` and carries per-session config overrides.
/// `config_overrides` is a placeholder shape (arbitrary JSON) until a later
/// step defines the actual override fields -- not yet produced or consumed
/// anywhere in step 2.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionNew {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    pub config_overrides: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionLoad {
    pub session_id: SessionId,
}

/// The agent (child) asking the client to run a host-coupled tool (e.g.
/// `workspace.snapshot`) over this same connection -- guardrail 4. Not yet
/// sent or handled anywhere in step 2 (tool execution stays in Horizon
/// until step 3); the shape exists here so the wire format is settled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostToolRequest {
    pub request_id: RequestId,
    pub tool_id: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostToolResponse {
    pub request_id: RequestId,
    pub output: serde_json::Value,
}

/// Explicit, non-panicking failure modes for [`read_envelope`]. The two
/// cases the design calls out by name (unknown `kind`, version mismatch)
/// get their own variants rather than falling through to a bare
/// [`serde_json::Error`], so callers can match on them instead of
/// string-sniffing a JSON error message.
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

/// Writes one envelope as a single newline-terminated JSON line and flushes
/// the writer, so a peer reading line-by-line (e.g. [`read_envelope`]) sees
/// it immediately rather than waiting on a fuller buffer.
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

/// Reads one newline-delimited envelope. `Ok(None)` means the peer closed
/// the connection cleanly between messages (0 bytes read, no partial line
/// pending); a partial line with no trailing newline (peer closed
/// mid-message) is [`WireError::TornLine`], never silently treated as a
/// complete (truncated) message.
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
    parse_line(line.trim_end_matches(['\n', '\r'])).map(Some)
}

/// Unknown-field tolerance comes for free from `serde`'s default behavior
/// (no `deny_unknown_fields` on `RawEnvelope` or any payload type above);
/// this function's own job is the two checks that *do* need to be explicit
/// -- the wire version ([`WireError::VersionMismatch`]) and the `kind` tag
/// ([`WireError::UnknownKind`]) -- ahead of decoding `payload` into a
/// specific contract type.
fn parse_line(line: &str) -> Result<Envelope, WireError> {
    #[derive(Deserialize)]
    struct RawEnvelope {
        v: u32,
        kind: String,
        #[serde(default)]
        session_id: Option<SessionId>,
        payload: serde_json::Value,
    }

    let raw: RawEnvelope = serde_json::from_str(line)?;
    if raw.v != CONTRACT_VERSION {
        return Err(WireError::VersionMismatch {
            expected: CONTRACT_VERSION,
            found: raw.v,
        });
    }
    let body = match raw.kind.as_str() {
        "command" => EnvelopeBody::Command(serde_json::from_value(raw.payload)?),
        "event" => EnvelopeBody::Event(serde_json::from_value(raw.payload)?),
        "control" => EnvelopeBody::Control(serde_json::from_value(raw.payload)?),
        other => return Err(WireError::UnknownKind(other.to_string())),
    };
    Ok(Envelope {
        v: raw.v,
        session_id: raw.session_id,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ToolCallId;
    use tokio::io::{AsyncWriteExt, BufReader};

    fn sample_command() -> Command {
        Command::UserMessage {
            text: "hi".to_string(),
        }
    }

    fn sample_event() -> Event {
        Event::ToolCallRequested(crate::contract::ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "fs.read".to_string(),
            input: serde_json::json!({"path": "a.txt"}),
        })
    }

    fn sample_controls() -> Vec<Control> {
        vec![
            Control::Hello(Hello {
                contract_version: CONTRACT_VERSION,
                binary_id: "0.1.0".to_string(),
                capabilities: vec!["sessions".to_string()],
            }),
            Control::SessionList,
            Control::SessionListResult(vec![SessionSummary {
                session_id: SessionId::new(),
                provider_id: ProviderId("builtin.agent.rig".to_string()),
            }]),
            Control::SessionNew(SessionNew {
                session_id: SessionId::new(),
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                config_overrides: None,
            }),
            Control::SessionLoad(SessionLoad {
                session_id: SessionId::new(),
            }),
            Control::HostToolRequest(HostToolRequest {
                request_id: RequestId("req-1".to_string()),
                tool_id: "workspace.snapshot".to_string(),
                input: serde_json::json!({}),
            }),
            Control::HostToolResponse(HostToolResponse {
                request_id: RequestId("req-1".to_string()),
                output: serde_json::json!({"ok": true}),
            }),
            Control::Ping,
            Control::Pong,
            Control::Drain,
            Control::HandshakeRejected("contract version mismatch".to_string()),
        ]
    }

    #[tokio::test]
    async fn round_trips_every_envelope_kind_over_a_duplex_stream() {
        let session_id = SessionId::new();
        let mut envelopes = vec![
            Envelope::command(session_id, sample_command()),
            Envelope::event(session_id, sample_event()),
        ];
        envelopes.extend(sample_controls().into_iter().map(Envelope::control));

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut server_reader = BufReader::new(server);

        for envelope in &envelopes {
            write_envelope(&mut client, envelope).await.unwrap();
        }
        drop(client);

        let mut received = Vec::new();
        while let Some(envelope) = read_envelope(&mut server_reader).await.unwrap() {
            received.push(envelope);
        }

        assert_eq!(received, envelopes);
    }

    #[tokio::test]
    async fn torn_line_without_trailing_newline_is_an_explicit_error() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        client
            .write_all(b"{\"v\":1,\"kind\":\"control\"")
            .await
            .unwrap();
        drop(client);

        let result = read_envelope(&mut server_reader).await;
        assert!(matches!(result, Err(WireError::TornLine)), "{result:?}");
    }

    #[tokio::test]
    async fn clean_disconnect_between_messages_is_not_an_error() {
        let (client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);
        drop(client);

        let result = read_envelope(&mut server_reader).await;
        assert!(matches!(result, Ok(None)), "{result:?}");
    }

    #[tokio::test]
    async fn unknown_kind_is_an_explicit_error_not_a_panic() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        client
            .write_all(b"{\"v\":1,\"kind\":\"bogus\",\"session_id\":null,\"payload\":{}}\n")
            .await
            .unwrap();
        drop(client);

        let result = read_envelope(&mut server_reader).await;
        match result {
            Err(WireError::UnknownKind(kind)) => assert_eq!(kind, "bogus"),
            other => panic!("expected UnknownKind, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wire_version_mismatch_is_an_explicit_error() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        client
            .write_all(
                b"{\"v\":99,\"kind\":\"control\",\"session_id\":null,\"payload\":\"ping\"}\n",
            )
            .await
            .unwrap();
        drop(client);

        let result = read_envelope(&mut server_reader).await;
        assert!(
            matches!(
                result,
                Err(WireError::VersionMismatch {
                    expected: CONTRACT_VERSION,
                    found: 99
                })
            ),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn unknown_top_level_fields_are_tolerated() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        client
            .write_all(
                b"{\"v\":1,\"kind\":\"control\",\"session_id\":null,\"payload\":\"ping\",\"future_field\":42}\n",
            )
            .await
            .unwrap();
        drop(client);

        let envelope = read_envelope(&mut server_reader)
            .await
            .unwrap()
            .expect("envelope should parse despite the unrecognized field");
        assert_eq!(envelope.body, EnvelopeBody::Control(Control::Ping));
    }
}
