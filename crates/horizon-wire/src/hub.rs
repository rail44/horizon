//! Domain-free hub plumbing: the error vocabulary every runtime's hub
//! trait returns (`docs/runtime-crate-alignment-design.md` judgment 3 —
//! "domain-free hub plumbing shared by both belongs in `horizon-wire`, not
//! duplicated"), and the `hello`-first invariant two of its variants
//! describe — [`HelloGate`] and [`negotiate_hello`] are the enforcement
//! half, kept next to the definition so the invariant has exactly one.
//!
//! [`HubError`] is one enum for *both* hubs on purpose. Its variants are
//! plain data — the same reasoning that lets this crate host the
//! `FRAME_…`/`TERMINAL_EVENT_…` size caps despite their names — and
//! splitting it per hub would renumber the surviving variants under an
//! index-based codec, i.e. a wire reshape for a pure code move. One enum
//! also keeps a rejected `hello` decodable on whichever socket produced it.

use std::sync::atomic::{AtomicBool, Ordering};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use remoc::prelude::*;

use crate::negotiate::{ClientHello, VersionRange};

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
}

impl From<rtc::CallError> for HubError {
    fn from(err: rtc::CallError) -> Self {
        Self::Call(err.to_string())
    }
}

/// One connection's answer to "has `hello` completed on it yet?" — the
/// enforcement half of [`HubError::HelloRequired`], and the reason that
/// variant's doc is a contract rather than a hope. Every hub owns one per
/// accepted connection; `require` is what its non-`hello`, non-`drain`
/// methods call first.
#[derive(Debug, Default)]
pub struct HelloGate {
    completed: AtomicBool,
}

impl HelloGate {
    pub const fn new() -> Self {
        Self {
            completed: AtomicBool::new(false),
        }
    }

    /// Opens the gate. Called by `hello` once it has produced a reply it is
    /// actually going to return — never merely once the versions matched,
    /// so a `hello` that fails after negotiating leaves the gate closed.
    pub fn mark_completed(&self) {
        self.completed.store(true, Ordering::Release);
    }

    /// Refuses to run before a successful negotiation, rather than trusting
    /// the client to call in order. Every hub method except `hello` itself
    /// and `drain` (the version-stable recovery surface a *rejected* client
    /// legitimately calls) goes through this.
    pub fn require(&self) -> Result<(), HubError> {
        if self.completed.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(HubError::HelloRequired)
        }
    }
}

/// `hello`'s range negotiation (`docs/remoc-adoption-design.md` §3): the
/// highest version both peers can honor, or a logged
/// [`HubError::IncompatibleVersion`] naming the daemon that refused.
///
/// The version *numbers* are deliberately not this crate's business — each
/// hub owns its own pair and passes `ours` in (see [`crate::negotiate`]) —
/// but the rejection shape is, because a client's recovery keys on it and
/// two independently written rejections could drift apart.
pub fn negotiate_hello(
    ours: VersionRange,
    client: &ClientHello,
    daemon_name: &str,
) -> Result<u32, HubError> {
    match ours.negotiate(client.supported) {
        Some(negotiated) => Ok(negotiated),
        None => {
            let reason = HubError::IncompatibleVersion {
                client: client.supported,
                daemon: ours,
            };
            eprintln!(
                "{daemon_name}: rejecting hello from {}: {reason}",
                client.binary_id
            );
            Err(reason)
        }
    }
}
