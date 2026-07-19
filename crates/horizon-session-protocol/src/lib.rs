//! Transport-neutral JSONL framing shared by Horizon's session-daemon
//! domains. Agent and terminal commands remain sister vocabularies in their
//! own crates; this crate owns only the envelope, handshake shape, and stream
//! framing they share.
//!
//! One kind is special: `session_control` is the version-stable shared
//! vocabulary. Domain payloads are only decoded after the version handshake
//! succeeds, but the handshake itself (and contract-mismatch recovery --
//! telling a stale daemon to [`SessionControl::Drain`] so it can be
//! restarted at the right version) must work *across* a version skew, so
//! [`read_envelope`] exempts `session_control` envelopes from the exact
//! `v` check. In exchange, [`SessionControl`]'s existing variants may never
//! change shape or meaning -- extend it additively only.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

pub mod schema_check;

/// The payload of a wire enum's deserialize-only `Unknown` variant — the
/// skew-discipline catch-all every wire enum carries from
/// `docs/remoc-adoption-design.md` §4 onwards: a peer evolved past this
/// build may send variants this build has never heard of, and the receive
/// path must degrade them to "an unknown item, to be skipped" instead of
/// tearing the connection down.
///
/// Mechanics: serde's `#[serde(other)]` only exists for internally/
/// adjacently tagged enums, and every Horizon wire enum is externally
/// tagged — so the catch-all is instead a trailing `#[serde(untagged)]`
/// newtype variant wrapping this zero-sized type, whose `Deserialize`
/// accepts (and discards) any value. Two consequences, both deliberate:
///
/// - **Deserialize-only.** Serializing this type is an error (never a
///   panic): `Unknown` is something a peer *received*, not something it is
///   ever allowed to put back on the wire.
/// - **A malformed known variant also degrades to `Unknown`.** serde tries
///   the tagged variants first and falls back to the untagged catch-all on
///   *any* failure, so a known tag with an undecodable payload decodes as
///   `Unknown` too. That is the same "skip this item, keep the channel"
///   posture the remoc adoption decided for deserialization errors
///   (adoption condition 2), applied one layer earlier.
///
/// New variants on a wire enum must be declared *above* the `Unknown`
/// catch-all: serde requires untagged variants to come last, and the
/// schema checker's appended-variant rule relies on declaration order.
///
/// The schema artifact (`schema/session-wire.json`) deliberately excludes
/// these catch-all branches: the schema documents what a peer may *send*,
/// and `Unknown` never legally crosses the wire.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct UnknownPayload;

impl Serialize for UnknownPayload {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom(
            "wire Unknown variants are deserialize-only and must never be sent",
        ))
    }
}

impl<'de> Deserialize<'de> for UnknownPayload {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(UnknownPayload)
    }
}

impl JsonSchema for UnknownPayload {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "UnknownPayload".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // `{"not": {}}` matches nothing: `Unknown` can never legally be
        // *sent*. The schema-artifact generator strips these branches
        // entirely (see this type's doc comment); the unsatisfiable schema
        // is only what a stray direct `schema_for!` call would see.
        schemars::json_schema!({"not": {}})
    }
}

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
///
/// Version 8: `TerminalCommand` gains `SetColorScheme`, re-pushing the
/// host's live theme-derived color scheme into an already-running
/// session (a live `Reload Config`/theme-settings apply) so OSC 10/11/12
/// query replies stop reflecting a stale spawn-time snapshot. A new
/// command variant on an already-versioned vocabulary, same bump
/// discipline as every other wire-shape addition here.
///
/// Version 9: `TerminalFrame.text` removed -- it was fully derivable from
/// `lines`, and its only production reader was the `HORIZON_GPUI_DUMP`
/// debug dump (copy goes through the daemon's `selected_text`, paint never
/// read it). Dropping it removes a per-snapshot and per-diff-apply String
/// rebuild plus its share of every snapshot's wire weight; the derivation
/// survives as the debug/test helper `TerminalFrame::text()`. Removing a
/// field changes the wire shape, so a stale peer must fail the handshake.
pub const SESSION_PROTOCOL_VERSION: u32 = 9;

pub const SESSION_CONTROL_KIND: &str = "session_control";

/// The version-stable shared vocabulary (see the crate doc): decodable
/// regardless of the envelope's `v`, because the handshake and
/// contract-mismatch recovery are exactly the conversations that happen
/// *between* versions. Existing variants must never change shape or
/// meaning; extend additively only. Peers built before the `Unknown`
/// catch-all existed (v9 and earlier builds predating the skew groundwork)
/// treat an unknown variant as malformed; peers from this build on decode
/// it as [`SessionControl::Unknown`] and ignore it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionControl {
    Hello(Hello),
    HandshakeRejected(String),
    Ping,
    Pong,
    Drain,
    /// Deserialize-only skew catch-all — see [`UnknownPayload`]. Keep last.
    #[serde(untagged)]
    Unknown(UnknownPayload),
}

/// A domain-neutral session-daemon envelope. `kind` selects a sister
/// vocabulary; `payload` is decoded by that domain only after the shared
/// version check succeeds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
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

    /// Same as [`Self::session_control`], but stamped with a *peer's*
    /// envelope version instead of this build's own. Contract-mismatch
    /// recovery needs this: `session_control` is version-stable (see the
    /// crate doc), but a daemon built before the [`read_envelope`]
    /// exemption landed (v8 and earlier) rejects any envelope whose `v`
    /// differs from its own before ever looking at `kind` -- so a `Drain`
    /// aimed at a stale daemon must travel at *that daemon's* version to be
    /// decoded at all.
    pub fn session_control_at(control: &SessionControl, v: u32) -> Result<Self, WireError> {
        Ok(Self {
            v,
            session_id: None,
            kind: SESSION_CONTROL_KIND.to_string(),
            payload: serde_json::to_value(control)?,
        })
    }
}

/// Sent by either peer during the session-daemon handshake.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    // `session_control` is the version-stable shared vocabulary (see the
    // crate doc): a foreign-versioned peer must still be able to say Hello
    // (and be told why it's rejected), and a mismatch-recovering client
    // must still be able to ask a stale daemon to Drain. Every other kind
    // is a domain vocabulary that only decodes safely at an exact version
    // match.
    if envelope.v != SESSION_PROTOCOL_VERSION && envelope.kind != SESSION_CONTROL_KIND {
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

    #[tokio::test]
    async fn session_control_is_readable_at_a_foreign_version() {
        let (mut client, server) = tokio::io::duplex(4096);
        let sent =
            Envelope::session_control_at(&SessionControl::Drain, SESSION_PROTOCOL_VERSION + 1)
                .unwrap();
        write_envelope(&mut client, &sent).await.unwrap();

        let received = read_envelope(&mut BufReader::new(server))
            .await
            .unwrap()
            .expect("a foreign-versioned session_control envelope should be readable");
        assert_eq!(received.v, SESSION_PROTOCOL_VERSION + 1);
        assert_eq!(
            received
                .decode_payload::<SessionControl>(SESSION_CONTROL_KIND)
                .unwrap(),
            SessionControl::Drain
        );
    }

    /// A `session_control` variant from a future build decodes as
    /// [`SessionControl::Unknown`] instead of failing the whole envelope —
    /// the §4 skew catch-all, on the one vocabulary that must already work
    /// across versions today.
    #[test]
    fn unknown_session_control_variant_decodes_to_unknown() {
        for raw in [
            serde_json::json!("future_unit_control"),
            serde_json::json!({"future_payload_control": {"anything": [1, 2, 3]}}),
        ] {
            let control: SessionControl = serde_json::from_value(raw).unwrap();
            assert_eq!(control, SessionControl::Unknown(UnknownPayload));
        }
    }

    /// `Unknown` is deserialize-only: an attempt to put it back on the wire
    /// is a serialization *error*, never a panic and never bytes.
    #[test]
    fn serializing_the_unknown_variant_is_an_error_not_a_panic() {
        let result = serde_json::to_string(&SessionControl::Unknown(UnknownPayload));
        assert!(result.is_err(), "{result:?}");
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
