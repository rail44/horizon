//! Domain-free hub plumbing: the error vocabulary every runtime's hub
//! trait returns (`docs/runtime-crate-alignment-design.md` judgment 3 —
//! "domain-free hub plumbing shared by both belongs in `horizon-wire`, not
//! duplicated").
//!
//! [`HubError`] is one enum for *both* hubs on purpose. Its variants are
//! plain data — the same reasoning that lets this crate host the
//! `FRAME_…`/`TERMINAL_EVENT_…` size caps despite their names — and
//! splitting it per hub would renumber the surviving variants under an
//! index-based codec, i.e. a wire reshape for a pure code move. One enum
//! also keeps a rejected `hello` decodable on whichever socket produced it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use remoc::prelude::*;

use crate::negotiate::VersionRange;

/// The hub's error vocabulary. One enum for every method: domain errors
/// and transport errors share it, per remoc's own rtc pattern (the
/// `From<rtc::CallError>` impl is what lets a lost connection surface as
/// an `Err` from any pending call).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
pub enum HubError {
    /// `hello`: the peers' version ranges do not overlap. Feeds the same
    /// auto-drain recovery as the JSONL era's `HandshakeRejected`.
    #[error("session protocol version ranges do not overlap: client {client}, daemon {daemon}")]
    IncompatibleVersion {
        client: VersionRange,
        daemon: VersionRange,
    },
    /// `attach_terminal`: no live terminal session with that id.
    #[error("no live terminal session with that id")]
    TerminalNotFound,
    /// `create_terminal`: the PTY spawn itself failed (bad shell,
    /// permissions, or the bounded spawn retries were exhausted). What the
    /// JSONL wire reported as a `TerminalUpdate::Error` on the update
    /// stream is now the create call's own result.
    #[error("terminal failed to start: {0}")]
    TerminalSpawnFailed(String),
    /// Transport failure of the rtc call itself, carried as its rendered
    /// message (`rtc::CallError` itself is not `Eq`/`JsonSchema`; nothing
    /// programmatic branches on its inner structure). Constructed
    /// client-side by the `From<rtc::CallError>` impl below — a server
    /// never sends it.
    #[error("hub call failed: {0}")]
    Call(String),
    /// Any method other than `hello`/`drain` was called before a
    /// successful `hello` on this connection. `hello` is contractually the
    /// first call (§3), and the daemon enforces it rather than trusting
    /// the client: a rejected or skipped negotiation must not grant access
    /// to the negotiated-behavior surface. (`drain` stays reachable — it
    /// is the version-stable recovery path a rejected client legitimately
    /// uses.) Appended additively for v10.1 of the artifact's history —
    /// an older client never triggers it (it always hellos first).
    #[error("hello has not completed on this connection")]
    HelloRequired,
    /// Skew catch-all: an error variant from a newer peer. Keep last.
    #[serde(other)]
    #[error("unknown hub error from a newer peer")]
    Unknown,
}

impl From<rtc::CallError> for HubError {
    fn from(err: rtc::CallError) -> Self {
        Self::Call(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::WireCodec;

    /// An unknown `HubError` variant from a newer peer degrades to
    /// `Unknown` under the wire codec (Postbag), instead of failing the
    /// reply — the §4 catch-all, proven on the one enum this crate owns.
    #[test]
    fn unknown_hub_error_variant_degrades_to_unknown_under_postbag() {
        #[derive(Serialize)]
        enum FutureHubError {
            SomethingNew { detail: String },
        }
        let mut bytes = Vec::new();
        <WireCodec as remoc::codec::Codec>::serialize(
            &mut bytes,
            &FutureHubError::SomethingNew {
                detail: "later".into(),
            },
        )
        .unwrap();
        let decoded: HubError =
            <WireCodec as remoc::codec::Codec>::deserialize(&bytes[..]).unwrap();
        assert_eq!(decoded, HubError::Unknown);
    }
}
