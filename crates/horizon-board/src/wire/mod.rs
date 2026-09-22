//! The log daemon's wire vocabulary — what a board write says and what
//! comes back — plus the version pair it negotiates.
//!
//! This module lives in `horizon-board` (not in `horizon-logd`) to break what
//! would otherwise be a circular package dependency: `horizon-logd` depends on
//! `horizon-board` (it reuses `BoardEvent`/`Envelope` for the JSONL append),
//! and the board library's write path is the logd *client* — so it needs the
//! `LogHubClient` type the `#[rtc::remote]` macro generates in [`hub`].
//! Keeping the trait in `horizon-board` lets both sides name it without a
//! cycle.
//!
//! The daemon crate (`crates/horizon-logd`) supplies the `Hub` implementation
//! and the `main.rs` entry point; this module owns only the wire contract.
//!
//! The operation vocabulary here is plain serde/schemars data, so it
//! compiles wherever the board queries do. The rtc trait, the error type it
//! answers with, and the version handshake need the transport, and live in
//! the native-only [`hub`] submodule.
//!
//! **Stage A** was `ingest` only. **Stage B** adds the subscribe stream — a
//! raw NDJSON line protocol multiplexed onto the same socket via first-byte
//! sniffing (a `{` byte routes to subscribe, anything else to the remoc chmux
//! handshake for `ingest`). The subscribe types below are plain serde structs,
//! not remoc rtc types — the `LogHub` trait itself is unchanged.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::store::Position;

#[cfg(not(target_family = "wasm"))]
mod hub;

#[cfg(not(target_family = "wasm"))]
pub use hub::*;

/// The log-daemon protocol version this build speaks.
///
/// A new independent protocol (logd has no pre-split history with the agent
/// or terminal hubs), so it starts at 1 — not 19. Under the standing lockstep
/// policy (`MIN_SUPPORTED_LOG_PROTOCOL_VERSION == LOG_PROTOCOL_VERSION`),
/// same-machine self-spawned daemons need no cross-version interop, only
/// honest restart.
pub const LOG_PROTOCOL_VERSION: u32 = 6;

/// The oldest log-wire version this build is still willing to negotiate down
/// to in `LogHub::hello`. Equal to [`LOG_PROTOCOL_VERSION`] under the
/// lockstep, no-per-feature-gates policy.
pub const MIN_SUPPORTED_LOG_PROTOCOL_VERSION: u32 = 6;

/// `horizon-logd`'s `hello` reply. Channel-free, like terminald's: logd has
/// no connection-global channels in stage A.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct LogHubHello {
    /// The highest mutually supported version.
    pub negotiated: u32,
    pub binary_id: String,
}

/// One board write operation, sent over the socket to logd. Each variant
/// mirrors a `Store` write method; logd performs the full read-fold-compute-
/// append atomically under its exclusive flock (the same sequence the library
/// used to do in-process) and returns the result.
///
/// `BoardEvent` is reused internally (logd constructs it from these
/// parameters when appending to the JSONL) — the wire type is the operation,
/// not the event, because operations like `add` need sibling rank context.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum IngestRequest {
    SetParent {
        id: u64,
        parent: Option<u64>,
        position: Position,
    },
    SetClosed {
        id: u64,
        is_closed: bool,
        status: Option<String>,
    },
    SetDependencies {
        id: u64,
        depends_on: Vec<u64>,
    },
    BindSession {
        id: u64,
        session_id: String,
        review: bool,
    },
    PostMessage {
        id: u64,
        message: crate::Comment,
    },
    MarkRead {
        id: u64,
        reader: String,
        message_id: String,
    },
    AdvanceCursor {
        consumer: String,
        position: u64,
    },
    /// `Store::add`: create a new item, optionally with a parent.
    Add {
        title: String,
        body: String,
        parent: Option<u64>,
        position: Position,
    },
    /// `Store::comment`: append a comment to an existing item.
    Comment {
        id: u64,
        author: String,
        text: String,
    },
    /// `Store::set_status`: set an item's status string.
    SetStatus {
        id: u64,
        status: String,
    },

    /// `Store::move_item`: re-rank an item to a new position.
    MoveItem {
        id: u64,
        position: Position,
    },

    /// `Store::edit`: update an item's title and/or body. Each `Option` is
    /// `None` for "leave unchanged", matching how `ItemUpdated` models partial
    /// updates.
    Edit {
        id: u64,
        title: Option<String>,
        body: Option<String>,
    },
}

/// The result of an [`IngestRequest`], carrying back exactly what the
/// corresponding `Store` method returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum IngestReply {
    /// The created task or atomically bound task session.
    Item(crate::Item),
    /// Successful mutation with no returned data.
    Done,
    /// `move_item`: the new rank string.
    Rank(String),
}

/// The subscribe request a consumer sends on connect, as one NDJSON line.
/// The daemon replies with a [`SubscribePoke`] carrying the current seq, then
/// streams further [`SubscribePoke`]s as ingests append to the file.
///
/// This is **not** a remoc rtc type — it rides a raw NDJSON line on the same
/// socket, sniffed apart from the chmux path by the first byte (`{` vs
/// binary). See `docs/logd-design.md` Subscription shape.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeRequest {
    /// The events.jsonl path to watch (the same path `ingest` takes). If
    /// absent, the subscriber receives pokes for all paths.
    #[serde(default)]
    pub path: Option<String>,
    /// The last seq the subscriber has seen. logd does not replay missed
    /// pokes — the subscriber catches up on its own (e.g. `tail -n +N`).
    #[serde(default)]
    pub since: Option<u64>,
}

/// One line on the subscribe stream: `{"log":"board","seq":1234}`. Carries the
/// log type and the 1-based line number of the appended event — never the
/// payload. Pokes are lossy; correctness lives in cursors over the log
/// (`docs/logd-design.md` decision 3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribePoke {
    /// The log type (v1: "board" only).
    pub log: String,
    /// The 1-based line number of the appended event in the JSONL file.
    pub seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lockstep_pair_is_equal() {
        assert_eq!(LOG_PROTOCOL_VERSION, 6);
        assert_eq!(MIN_SUPPORTED_LOG_PROTOCOL_VERSION, 6);
    }
}
