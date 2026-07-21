//! Standing cross-version skew tests — `docs/remoc-adoption-design.md` §7's
//! V1/V2 type-pair method, promoted from the remoc spike
//! (`spike/remoc/tests/skew.rs`, branch `remoc-spike`) to a permanent
//! resident. A frozen copy of a vocabulary shape (`*V1`) sits beside its
//! evolved twin (`*V2`), and each §4 rule is proven in both directions over
//! the *actual v10 wire codec* (Postbag, [`WireCodec`]) — at the codec
//! level and, for the poisoned-item rule, through a live `rch::mpsc`
//! channel. The frozen types are the *executable* form of the schema
//! artifact (`schema/session-wire.json`); the artifact's own drift/additive
//! checks live in `crates/horizon-sessiond/tests/wire_schema.rs` and
//! `scripts/check-wire-schema.sh`.
//!
//! v10 note: the catch-all is `#[serde(other)] Unknown` (a plain unit
//! variant), not the JSONL era's `#[serde(untagged)] Unknown(UnknownPayload)`.
//! Postbag rejects serde's `deserialize_any` (which the untagged pattern
//! relied on), so the `#[serde(other)]` unit form is both what the adoption
//! conditions specify and what the spike validated under Postbag. One
//! consequence the tests pin: a *payload-carrying* unknown variant degrades
//! to `Unknown` too (its bytes are read and discarded), and `Unknown` is a
//! legal — if never intentionally sent — encoding of the literal tag.
//!
//! Coverage runs over both encodings a vocabulary lives in: the Postbag
//! wire (every test's default) and, where the rule is meaningful there,
//! serde_json — the event log's on-disk format, which shares these types.
//! The free-form JSON payloads (`contract::JsonValue`) get their own
//! Postbag proof: they cross this non-self-describing wire as JSON text,
//! because a raw `serde_json::Value` (whose `Deserialize` requires
//! `deserialize_any`) cannot.

use horizon_agent::contract::{JsonValue, ToolCallId, ToolCallRequest};
use horizon_session_protocol::WireCodec;
use remoc::codec::Codec;
use remoc::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;

// The `Connect::io` (conn, base-sender, base-receiver) triples, named to
// keep clippy's `type_complexity` lint quiet in the live-channel test.
type Conn = remoc::Connect<'static, std::io::Error, std::io::Error>;
type OldSide = (
    Conn,
    rch::base::Sender<(), WireCodec>,
    rch::base::Receiver<rch::mpsc::Receiver<CommandV1, WireCodec>, WireCodec>,
);
type NewSide = (
    Conn,
    rch::base::Sender<rch::mpsc::Receiver<CommandMixedSender, WireCodec>, WireCodec>,
    rch::base::Receiver<(), WireCodec>,
);

/// Frozen: a wire struct as an older build shipped it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct SpawnV1 {
    shell: String,
    rows: u16,
}

/// Evolved: the same struct after an additive change — a new
/// `#[serde(default)]` field, per §4 rule 1.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct SpawnV2 {
    shell: String,
    rows: u16,
    #[serde(default)]
    isolate: bool,
    #[serde(default)]
    workspace_root: Option<String>,
}

/// Frozen: a wire enum as an older build shipped it — mixed unit, newtype,
/// and struct variants, externally tagged like every Horizon wire enum,
/// with the `#[serde(other)]` catch-all last.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandV1 {
    Ping,
    Input(Vec<u8>),
    Resize {
        rows: u16,
        cols: u16,
    },
    #[serde(other)]
    Unknown,
}

/// Evolved: the same enum after appending variants (§4 rule 1: "new enum
/// variants are appended" — above the trailing catch-all). One unit, one
/// payload-carrying, to exercise both shapes of skew.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandV2 {
    Ping,
    Input(Vec<u8>),
    Resize {
        rows: u16,
        cols: u16,
    },
    Bell,
    SetTitle(String),
    #[serde(other)]
    Unknown,
}

/// A *misbehaving* peer's enum: the `resize` identifier is a known V1
/// variant, but its payload is structurally broken — it omits the
/// `cols` field V1 requires (no `#[serde(default)]`). This is the "known
/// variant, broken payload" case, which must fail that one item's decode
/// (never panic, never kill the channel) rather than degrade to
/// `Unknown` (the catch-all only covers unknown *identifiers*).
///
/// Why a *missing field* and not a wrong-typed one: Postbag is not
/// self-describing, so a type-level mismatch does not reliably error —
/// it can silently misdecode (measured while writing this test: `rows:
/// "broken"` as a String decoded into V1's `rows: u16` as `6`, the
/// string's length varint read as the integer). Structural breaks
/// (missing required fields) are what Postbag detects, matching the
/// spike's (b') finding; the silent-misdecode hazard is bounded by the
/// §4 additive-only discipline, which forbids retyping a field without a
/// version bump in the first place.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandBrokenPayload {
    Input(Vec<u8>),
    Resize { rows: u16 },
}

/// The live-channel test's sender: one enum that can produce all three
/// skew shapes against a `CommandV1` receiver on a single channel — a
/// known-good item (`input`), a known identifier with a broken payload
/// (`resize` with string fields), and an unknown identifier
/// (`set_title`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandMixedSender {
    Input(Vec<u8>),
    /// Missing V1's required `cols` — see [`CommandBrokenPayload`].
    Resize {
        rows: u16,
    },
    SetTitle(String),
}

/// Encode with `value`'s type, decode as `D` — through the exact codec the
/// v10 wire uses.
fn wire_roundtrip<S: Serialize, D: serde::de::DeserializeOwned>(value: &S) -> D {
    let mut bytes = Vec::new();
    <WireCodec as Codec>::serialize(&mut bytes, value).expect("sender must serialize");
    <WireCodec as Codec>::deserialize(&bytes[..]).expect("receiver must decode")
}

/// §4 rule 1, new-to-old direction: an old receiver ignores the fields a
/// newer sender added.
#[test]
fn an_old_reader_ignores_fields_a_new_sender_added() {
    let sent = SpawnV2 {
        shell: "fish".to_string(),
        rows: 24,
        isolate: true,
        workspace_root: Some("/tmp/w".to_string()),
    };
    let received: SpawnV1 = wire_roundtrip(&sent);
    assert_eq!(
        received,
        SpawnV1 {
            shell: "fish".to_string(),
            rows: 24,
        }
    );
}

/// §4 rule 1, old-to-new direction: a new receiver completes the fields an
/// older sender never knew about from their `#[serde(default)]`s.
#[test]
fn a_new_reader_defaults_fields_an_old_sender_never_sent() {
    let sent = SpawnV1 {
        shell: "fish".to_string(),
        rows: 24,
    };
    let received: SpawnV2 = wire_roundtrip(&sent);
    assert_eq!(
        received,
        SpawnV2 {
            shell: "fish".to_string(),
            rows: 24,
            isolate: false,
            workspace_root: None,
        }
    );
}

/// §4 rule 2, unit variant: a unit variant appended by a newer sender
/// decodes to the old receiver's `Unknown` catch-all — never an error.
#[test]
fn an_old_reader_decodes_an_appended_unit_variant_to_unknown() {
    let received: CommandV1 = wire_roundtrip(&CommandV2::Bell);
    assert_eq!(received, CommandV1::Unknown);
}

/// §4 rule 2, payload-carrying variant: the Postbag win the spike
/// validated — an unknown variant that *carries data* also degrades to the
/// unit `Unknown` (its payload bytes are read and discarded), where
/// serde_json's `#[serde(other)]` would have errored. This is the whole
/// reason v10 uses Postbag's `#[serde(other)]` rather than the JSONL
/// untagged form.
#[test]
fn an_old_reader_decodes_an_appended_payload_variant_to_unknown() {
    let received: CommandV1 = wire_roundtrip(&CommandV2::SetTitle("hi".to_string()));
    assert_eq!(received, CommandV1::Unknown);
}

/// The shared variants stay bit-for-bit compatible around the appended
/// ones, in both directions.
#[test]
fn shared_variants_survive_the_skew_in_both_directions() {
    let received: CommandV1 = wire_roundtrip(&CommandV2::Resize { rows: 3, cols: 7 });
    assert_eq!(received, CommandV1::Resize { rows: 3, cols: 7 });
    let received: CommandV2 = wire_roundtrip(&CommandV1::Input(vec![1, 2, 3]));
    assert_eq!(received, CommandV2::Input(vec![1, 2, 3]));
}

/// The §7 poisoned-item rule, over a *live* `rch::mpsc` channel (the
/// spike's c2 scenario, now a standing test), with all three skew shapes
/// in one stream: a known-good item, then a **known identifier with a
/// broken payload** (a genuine per-item decode failure — surfaced as a
/// non-final recv error the loop skips), then an **unknown identifier**
/// (degraded to `Unknown` by the catch-all), then another known-good
/// item. The channel survives both middle items; the trailing item
/// arrives.
///
/// The skew is real (the two ends are genuinely different Rust types): the
/// channel is transported over an asymmetric base connection whose sender
/// side carries `Receiver<CommandMixedSender>` and whose receiver side
/// reconstructs it as `Receiver<CommandV1>`. Adoption condition 3: both
/// `Connect::io` handshakes are driven concurrently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poisoned_items_do_not_kill_a_live_channel() {
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    // Old side (V1 receiver).
    let old = tokio::spawn(async move {
        let (conn, _tx, mut rx): OldSide = remoc::Connect::io(remoc::Cfg::default(), b_r, b_w)
            .await
            .unwrap();
        tokio::spawn(conn);

        let mut commands = rx.recv().await.unwrap().unwrap();
        let mut delivered = Vec::new();
        let mut skipped = 0;
        loop {
            match commands.recv().await {
                Ok(Some(command)) => delivered.push(command),
                Ok(None) => break,
                // A per-item decode failure (a non-final recv error) is
                // skipped, never a teardown — the loop keeps going. This is
                // the same posture every receive pump in the daemon and UI
                // takes (adoption condition 2).
                Err(err) if !err.is_final() => skipped += 1,
                Err(_) => break,
            }
        }
        (delivered, skipped)
    });

    // New side (V2 sender).
    let (conn, mut tx, _rx): NewSide = remoc::Connect::io(remoc::Cfg::default(), a_r, a_w)
        .await
        .unwrap();
    tokio::spawn(conn);

    let (command_tx, command_rx) = rch::mpsc::channel::<CommandMixedSender, WireCodec>(8);
    tx.send(command_rx).await.unwrap();
    for command in [
        CommandMixedSender::Input(b"before".to_vec()),
        CommandMixedSender::Resize { rows: 24 },
        CommandMixedSender::SetTitle("unknown to v1".to_string()),
        CommandMixedSender::Input(b"after".to_vec()),
    ] {
        drop(command_tx.send(command).await.unwrap());
    }
    drop(command_tx);

    let (delivered, skipped) = old.await.unwrap();
    assert!(
        delivered.contains(&CommandV1::Input(b"before".to_vec())),
        "the first known item must arrive, got: {delivered:?}"
    );
    assert_eq!(
        skipped, 1,
        "the broken-payload item must surface as exactly one skipped \
         (non-final) recv error, got delivered: {delivered:?}"
    );
    assert!(
        delivered.contains(&CommandV1::Unknown),
        "the unknown-identifier item must degrade to Unknown, got: {delivered:?}"
    );
    assert!(
        delivered.contains(&CommandV1::Input(b"after".to_vec())),
        "the trailing known item must survive both poisoned items, got: {delivered:?}"
    );
}

/// "Known variant, broken payload": a peer that sends a *known* identifier
/// with an undecodable payload produces a per-item decode **error** — not
/// a panic, and not a silent `Unknown` (the `#[serde(other)]` catch-all
/// only covers unknown identifiers). Receive loops turn this into "skip
/// the item" (the non-final branch of every pump in the daemon/UI); the
/// live-channel test below proves the channel itself survives.
#[test]
fn a_broken_payload_on_a_known_variant_is_a_per_item_error_not_a_panic() {
    let mut bytes = Vec::new();
    <WireCodec as Codec>::serialize(&mut bytes, &CommandBrokenPayload::Resize { rows: 24 })
        .unwrap();
    let result: Result<CommandV1, _> = <WireCodec as Codec>::deserialize(&bytes[..]);
    assert!(result.is_err(), "{result:?}");
}

/// The free-form JSON payloads (`contract::JsonValue`) cross the Postbag
/// wire as JSON text — a raw `serde_json::Value` cannot cross it at all
/// (its `Deserialize` needs `deserialize_any`, which Postbag rejects).
/// Round-trips a whole `ToolCallRequest` — the shape that actually rides
/// the agent event channel inside `Event::ToolCallRequested`.
#[test]
fn json_payloads_round_trip_the_postbag_wire_as_json_text() {
    // The control case first: a bare serde_json::Value genuinely cannot
    // cross this wire — the whole reason JsonValue exists.
    let mut bytes = Vec::new();
    <WireCodec as Codec>::serialize(&mut bytes, &serde_json::json!({"path": "a.txt"})).unwrap();
    let bare: Result<serde_json::Value, _> = <WireCodec as Codec>::deserialize(&bytes[..]);
    assert!(
        bare.is_err(),
        "a bare Value decoding under Postbag would make JsonValue unnecessary: {bare:?}"
    );

    let request = ToolCallRequest {
        call_id: ToolCallId("call-1".to_string()),
        tool_id: "fs.read".to_string(),
        input: serde_json::json!({"path": "a.txt", "nested": [1, 2, {"k": true}]}).into(),
    };
    let received: ToolCallRequest = wire_roundtrip(&request);
    assert_eq!(received, request);

    let value: JsonValue = wire_roundtrip(&JsonValue::from(serde_json::json!([1, "two", null])));
    assert_eq!(value, serde_json::json!([1, "two", null]));
}

/// The disk-side (serde_json) halves of the rules, on the same V1/V2
/// pairs: these types' other life is the event log's on-disk JSONL, where
/// the same additive-evolution guarantees must hold. (Payload-carrying
/// unknown variants are the one asymmetry: serde_json's `#[serde(other)]`
/// insists on unit content, so only the unit-variant degradation is
/// provable here — the payload-carrying case is Postbag-only, above.)
#[test]
fn the_field_rules_and_unit_unknown_degradation_also_hold_under_serde_json() {
    let sent = SpawnV2 {
        shell: "fish".to_string(),
        rows: 24,
        isolate: true,
        workspace_root: Some("/tmp/w".to_string()),
    };
    let received: SpawnV1 = serde_json::from_str(&serde_json::to_string(&sent).unwrap()).unwrap();
    assert_eq!(
        received,
        SpawnV1 {
            shell: "fish".to_string(),
            rows: 24,
        }
    );

    let sent = SpawnV1 {
        shell: "fish".to_string(),
        rows: 24,
    };
    let received: SpawnV2 = serde_json::from_str(&serde_json::to_string(&sent).unwrap()).unwrap();
    assert!(!received.isolate);
    assert_eq!(received.workspace_root, None);

    let received: CommandV1 =
        serde_json::from_str(&serde_json::to_string(&CommandV2::Bell).unwrap()).unwrap();
    assert_eq!(received, CommandV1::Unknown);
}
