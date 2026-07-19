//! Standing cross-version skew tests — `docs/remoc-adoption-design.md` §7's
//! V1/V2 type-pair method, promoted from the remoc spike
//! (`spike/remoc/tests/skew.rs`, branch `remoc-spike`) to a permanent
//! resident. A frozen copy of a vocabulary shape (`*V1`) sits beside its
//! evolved twin (`*V2`), and each §4 rule is proven in both directions over
//! the wire encoding the live JSONL protocol actually uses (`serde_json`).
//! The frozen types are the *executable* form of the schema artifact
//! (`schema/session-wire.json`); the artifact's own drift/additive checks
//! live in `crates/horizon-sessiond/tests/wire_schema.rs` and
//! `scripts/check-wire-schema.sh`.
//!
//! Only the rules that are meaningful on JSONL are here: unknown fields
//! ignored, missing `#[serde(default)]` fields completed, unknown variants
//! decoding to the `Unknown` catch-all (and that catch-all being
//! deserialize-only). The remaining spike scenario — one poisoned item not
//! killing a channel — is a channel-semantics property of the remoc
//! transport and joins these tests with the v10 cutover.

use horizon_session_protocol::UnknownPayload;
use serde::{Deserialize, Serialize};

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
/// with the deserialize-only `Unknown` catch-all.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandV1 {
    Ping,
    Input(Vec<u8>),
    Resize {
        rows: u16,
        cols: u16,
    },
    #[serde(untagged)]
    Unknown(UnknownPayload),
}

/// Evolved: the same enum after appending a variant (§4 rule 1: "new enum
/// variants are appended" — above the trailing catch-all).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandV2 {
    Ping,
    Input(Vec<u8>),
    Resize {
        rows: u16,
        cols: u16,
    },
    SetTitle(String),
    #[serde(untagged)]
    Unknown(UnknownPayload),
}

fn wire_roundtrip<S: Serialize, D: serde::de::DeserializeOwned>(value: &S) -> D {
    let line = serde_json::to_string(value).expect("sender must serialize");
    serde_json::from_str(&line).expect("receiver must decode")
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

/// §4 rule 2: a variant appended by a newer sender decodes to the old
/// receiver's `Unknown` catch-all — never an error, never a teardown.
#[test]
fn an_old_reader_decodes_an_appended_variant_to_unknown() {
    let received: CommandV1 = wire_roundtrip(&CommandV2::SetTitle("hi".to_string()));
    assert_eq!(received, CommandV1::Unknown(UnknownPayload));
}

/// The shared variants stay bit-for-bit compatible around the appended one,
/// in both directions.
#[test]
fn shared_variants_survive_the_skew_in_both_directions() {
    let received: CommandV1 = wire_roundtrip(&CommandV2::Resize { rows: 3, cols: 7 });
    assert_eq!(received, CommandV1::Resize { rows: 3, cols: 7 });
    let received: CommandV2 = wire_roundtrip(&CommandV1::Input(vec![1, 2, 3]));
    assert_eq!(received, CommandV2::Input(vec![1, 2, 3]));
}

/// The catch-all is deserialize-only: a receiver that decoded `Unknown`
/// gets an *error* (never a panic, never bytes) if it tries to forward it
/// back onto the wire.
#[test]
fn forwarding_a_received_unknown_is_a_serialize_error_not_a_panic() {
    let received: CommandV1 = wire_roundtrip(&CommandV2::SetTitle("hi".to_string()));
    let forwarded = serde_json::to_string(&received);
    assert!(forwarded.is_err(), "{forwarded:?}");
}
