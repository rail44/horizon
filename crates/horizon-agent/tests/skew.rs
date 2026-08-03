//! Standing wire-decode robustness tests. This project carries no
//! cross-version decode compatibility (owner decision 2026-08-03): every
//! wire enum decodes against exactly the shape the running build produces,
//! and an unrecognized identifier is a decode error like any other
//! malformed item -- there is no `Unknown` catch-all to degrade into.
//!
//! What still matters, and what this file proves, is that a structurally
//! broken item never panics and never tears down a live channel: Postbag
//! deserialization failures are always non-final (`remoc::rch::base::
//! RecvError::is_final`), so a receive loop's non-final branch skips the
//! one bad item and the channel keeps delivering everything after it --
//! the same posture every receive pump in the daemon and UI takes.
//!
//! Also covers the Postbag-specific `JsonValue` round-trip: a raw
//! `serde_json::Value` cannot cross this non-self-describing wire at all
//! (its `Deserialize` needs `deserialize_any`, which Postbag rejects), which
//! is why free-form tool I/O payloads travel as `contract::JsonValue` (JSON
//! text in a string) instead.

use horizon_agent::contract::{JsonValue, ToolCallId, ToolCallRequest};
use horizon_wire::WireCodec;
use remoc::codec::Codec;
use remoc::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;

// The `Connect::io` (conn, base-sender, base-receiver) triples, named to
// keep clippy's `type_complexity` lint quiet in the live-channel test.
type Conn = remoc::Connect<'static, std::io::Error, std::io::Error>;
type ReceiverSide = (
    Conn,
    rch::base::Sender<(), WireCodec>,
    rch::base::Receiver<rch::mpsc::Receiver<Command, WireCodec>, WireCodec>,
);
type SenderSide = (
    Conn,
    rch::base::Sender<rch::mpsc::Receiver<CommandMixedSender, WireCodec>, WireCodec>,
    rch::base::Receiver<(), WireCodec>,
);

/// A minimal wire enum, shaped like any of this crate's real ones now that
/// none of them carry an `Unknown` catch-all.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Command {
    Input(Vec<u8>),
    Resize { rows: u16, cols: u16 },
}

/// A peer sending a *known* identifier with a structurally broken payload
/// -- omits `Resize`'s required `cols` field. Proves that decode failure is
/// a per-item error, not a panic.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandBrokenPayload {
    Input(Vec<u8>),
    Resize { rows: u16 },
}

/// The live-channel test's sender: a known-good item (`Input`), a known
/// identifier with a broken payload (`Resize`, matching
/// [`CommandBrokenPayload`]), and an identifier [`Command`] doesn't know at
/// all (`SetTitle`) -- all three now fail the same way (a per-item decode
/// error), since there is no catch-all to degrade an unrecognized
/// identifier into.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandMixedSender {
    Input(Vec<u8>),
    Resize { rows: u16 },
    SetTitle(String),
}

/// Encode with `value`'s type, decode as `D` — through the exact codec the
/// wire uses.
fn wire_roundtrip<S: Serialize, D: serde::de::DeserializeOwned>(value: &S) -> D {
    let mut bytes = Vec::new();
    <WireCodec as Codec>::serialize(&mut bytes, value).expect("sender must serialize");
    <WireCodec as Codec>::deserialize(&bytes[..]).expect("receiver must decode")
}

/// A peer sending a *known* identifier with a structurally broken payload
/// produces a per-item decode **error** -- not a panic. Receive loops turn
/// this into "skip the item" (the non-final branch of every pump in the
/// daemon/UI); the live-channel test below proves the channel itself
/// survives it.
#[test]
fn a_broken_payload_on_a_known_variant_is_a_per_item_error_not_a_panic() {
    let mut bytes = Vec::new();
    <WireCodec as Codec>::serialize(&mut bytes, &CommandBrokenPayload::Resize { rows: 24 })
        .unwrap();
    let result: Result<Command, _> = <WireCodec as Codec>::deserialize(&bytes[..]);
    assert!(result.is_err(), "{result:?}");
}

/// Corruption robustness over a *live* `rch::mpsc` channel: a known-good
/// item, then a **known identifier with a broken payload**, then an
/// **identifier the receiver doesn't recognize at all**, then another
/// known-good item. Both middle items are per-item decode errors (Postbag
/// deserialization failures are always non-final), so the channel survives
/// both and the trailing item arrives.
///
/// The two ends are genuinely different Rust types: the channel is
/// transported over an asymmetric base connection whose sender side carries
/// `Receiver<CommandMixedSender>` and whose receiver side reconstructs it as
/// `Receiver<Command>`. Both `Connect::io` handshakes are driven
/// concurrently (sequentially awaiting one side deadlocks).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poisoned_items_do_not_kill_a_live_channel() {
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    let receiver = tokio::spawn(async move {
        let (conn, _tx, mut rx): ReceiverSide = remoc::Connect::io(remoc::Cfg::default(), b_r, b_w)
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
                // skipped, never a teardown -- the loop keeps going. This is
                // the same posture every receive pump in the daemon and UI
                // takes.
                Err(err) if !err.is_final() => skipped += 1,
                Err(_) => break,
            }
        }
        (delivered, skipped)
    });

    let (conn, mut tx, _rx): SenderSide = remoc::Connect::io(remoc::Cfg::default(), a_r, a_w)
        .await
        .unwrap();
    tokio::spawn(conn);

    let (command_tx, command_rx) = rch::mpsc::channel::<CommandMixedSender, WireCodec>(8);
    tx.send(command_rx).await.unwrap();
    for command in [
        CommandMixedSender::Input(b"before".to_vec()),
        CommandMixedSender::Resize { rows: 24 },
        CommandMixedSender::SetTitle("unrecognized by the receiver".to_string()),
        CommandMixedSender::Input(b"after".to_vec()),
    ] {
        drop(command_tx.send(command).await.unwrap());
    }
    drop(command_tx);

    let (delivered, skipped) = receiver.await.unwrap();
    assert_eq!(
        delivered,
        vec![
            Command::Input(b"before".to_vec()),
            Command::Input(b"after".to_vec()),
        ],
        "only the two known-good items should be delivered"
    );
    assert_eq!(
        skipped, 2,
        "the broken-payload item and the unrecognized-identifier item must \
         each surface as one skipped (non-final) recv error, got delivered: {delivered:?}"
    );
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
        occurrence_id: None,
    };
    let received: ToolCallRequest = wire_roundtrip(&request);
    assert_eq!(received, request);

    let value: JsonValue = wire_roundtrip(&JsonValue::from(serde_json::json!([1, "two", null])));
    assert_eq!(value, serde_json::json!([1, "two", null]));
}
