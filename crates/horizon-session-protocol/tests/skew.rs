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
    rch::base::Sender<rch::mpsc::Receiver<CommandV2, WireCodec>, WireCodec>,
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
/// spike's c2 scenario, now a standing test): a newer sender pushes a known
/// variant, then an unknown one, then another known one; the old receiver
/// gets both known items and — because the catch-all decodes the unknown
/// one rather than erroring — the channel survives the middle item intact.
///
/// The skew is real (the two ends are genuinely different Rust types): the
/// channel is transported over an asymmetric base connection whose sender
/// side carries `Receiver<CommandV2>` and whose receiver side reconstructs
/// it as `Receiver<CommandV1>`. Adoption condition 3: both `Connect::io`
/// handshakes are driven concurrently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_unknown_item_does_not_kill_a_live_channel() {
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
                // skipped, never a teardown — the loop keeps going.
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

    let (command_tx, command_rx) = rch::mpsc::channel::<CommandV2, WireCodec>(8);
    tx.send(command_rx).await.unwrap();
    for command in [
        CommandV2::Input(b"before".to_vec()),
        CommandV2::SetTitle("unknown to v1".to_string()),
        CommandV2::Input(b"after".to_vec()),
    ] {
        drop(command_tx.send(command).await.unwrap());
    }
    drop(command_tx);

    let (delivered, _skipped) = old.await.unwrap();
    // Both known items arrive; the unknown one in between degrades to the
    // catch-all rather than poisoning the stream. (Under Postbag's
    // `#[serde(other)]` the middle item decodes to `Unknown` rather than
    // erroring, so it also lands as a delivered item — either way the
    // channel is never torn down and the trailing known item arrives.)
    assert!(
        delivered.contains(&CommandV1::Input(b"before".to_vec())),
        "the first known item must arrive, got: {delivered:?}"
    );
    assert!(
        delivered.contains(&CommandV1::Input(b"after".to_vec())),
        "the trailing known item must survive the unknown one, got: {delivered:?}"
    );
}
