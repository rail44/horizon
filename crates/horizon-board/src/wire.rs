//! The log daemon's hub — `horizon-logd`'s whole rtc surface — and the
//! version pair it negotiates.
//!
//! This module lives in `horizon-board` (not in `horizon-logd`) to break what
//! would otherwise be a circular package dependency: `horizon-logd` depends on
//! `horizon-board` (it reuses `BoardEvent`/`Envelope` for the JSONL append),
//! and the board library's write path is the logd *client* — so it needs the
//! `LogHubClient` type the `#[rtc::remote]` macro generates here. Keeping the
//! trait in `horizon-board` lets both sides name it without a cycle.
//!
//! The daemon crate (`crates/horizon-logd`) supplies the `Hub` implementation
//! and the `main.rs` entry point; this module owns only the wire contract.
//!
//! **Stage A** (`docs/logd-design.md` v1 slicing): the API is `ingest` only.
//! Subscribe is stage B and will be added by bumping `LOG_PROTOCOL_VERSION`
//! (lockstep — no wire slots reserved).

use remoc::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use horizon_wire::{ClientHello, HubError, VersionRange};

use crate::model::Item;
use crate::store::Position;

/// The log-daemon protocol version this build speaks.
///
/// A new independent protocol (logd has no pre-split history with the agent
/// or terminal hubs), so it starts at 1 — not 19. Under the standing lockstep
/// policy (`MIN_SUPPORTED_LOG_PROTOCOL_VERSION == LOG_PROTOCOL_VERSION`),
/// same-machine self-spawned daemons need no cross-version interop, only
/// honest restart.
pub const LOG_PROTOCOL_VERSION: u32 = 1;

/// The oldest log-wire version this build is still willing to negotiate down
/// to in [`LogHub::hello`]. Equal to [`LOG_PROTOCOL_VERSION`] under the
/// lockstep, no-per-feature-gates policy.
pub const MIN_SUPPORTED_LOG_PROTOCOL_VERSION: u32 = 1;

/// The version range this build advertises in every `hello` to `horizon-logd`.
pub fn log_version_range() -> VersionRange {
    VersionRange::new(MIN_SUPPORTED_LOG_PROTOCOL_VERSION, LOG_PROTOCOL_VERSION)
}

/// A [`ClientHello`] advertising [`log_version_range`] under `binary_id`.
pub fn log_client_hello(binary_id: impl Into<String>) -> ClientHello {
    ClientHello::new(log_version_range(), binary_id)
}

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
/// not the event, because operations like `add` need context the event does
/// not carry (e.g. `Position` for rank computation) and because `claim` must
/// find-and-append atomically.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum IngestRequest {
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
    SetStatus { id: u64, status: String },
    /// `Store::assign`: set an item's assignee.
    Assign { id: u64, who: String },
    /// `Store::move_item`: re-rank an item to a new position.
    MoveItem { id: u64, position: Position },
    /// `Store::claim`: atomically claim the first ready+unassigned item.
    Claim { who: String },
}

/// The result of an [`IngestRequest`], carrying back exactly what the
/// corresponding `Store` method returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum IngestReply {
    /// `add` / `claim`: the new or claimed item (with assigned id and rank).
    Item(Item),
    /// `comment` / `set_status` / `assign`: success, no data.
    Done,
    /// `move_item`: the new rank string.
    Rank(String),
    /// `claim` when no ready+unassigned item was found.
    MaybeItem(Option<Item>),
}

/// The domain error `ingest` returns. Distinct from [`HubError`] (which is
/// `horizon-wire`'s shared protocol-level vocabulary) because the board
/// domain has its own error shape (`ItemNotFound`, `RankExhausted`) that
/// should survive the wire round-trip as typed data, not be stringified into
/// `HubError::Call`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
pub enum LogError {
    #[error("item {0} not found")]
    ItemNotFound(u64),
    #[error("rank space exhausted (rebalance needed)")]
    RankExhausted,
    #[error("{0}")]
    Io(String),
    /// Transport failure of the rtc call itself, carried as its rendered
    /// message (constructed client-side by the `From<rtc::CallError>` impl —
    /// a server never sends it).
    #[error("hub call failed: {0}")]
    Call(String),
}

impl From<remoc::rtc::CallError> for LogError {
    fn from(err: remoc::rtc::CallError) -> Self {
        Self::Call(err.to_string())
    }
}

/// The log hub — `horizon-logd`'s whole rtc surface (`docs/logd-design.md`).
///
/// `hello` and `drain` return [`HubError`] (the shared protocol vocabulary);
/// `ingest` returns [`LogError`] (the board domain vocabulary). Stage A is
/// `ingest` only; subscribe is stage B.
#[rtc::remote]
pub trait LogHub {
    /// Version negotiation — the first call on every connection.
    async fn hello(&self, client: ClientHello) -> Result<LogHubHello, HubError>;

    /// Performs one board write operation against the project whose
    /// `events.jsonl` lives at `path`. The path is resolved client-side
    /// (via `Store::from_cwd`/`from_dir`, which collapse worktree → main git
    /// root) and sent as a string; logd opens, flocks, reads-folds, computes,
    /// appends, and flushes before replying, so the file is durable when the
    /// reply arrives.
    async fn ingest(&self, path: String, request: IngestRequest) -> Result<IngestReply, LogError>;

    /// Flush-and-exit. Like the other daemons' `drain`, the call itself
    /// typically errors because the process is gone before a reply travels.
    async fn drain(&self) -> Result<(), HubError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_range_negotiates_with_itself_at_the_current_version() {
        assert_eq!(
            log_version_range().negotiate(log_version_range()),
            Some(LOG_PROTOCOL_VERSION)
        );
    }

    #[test]
    fn the_lockstep_pair_is_equal() {
        assert_eq!(LOG_PROTOCOL_VERSION, 1);
        assert_eq!(MIN_SUPPORTED_LOG_PROTOCOL_VERSION, 1);
    }
}
