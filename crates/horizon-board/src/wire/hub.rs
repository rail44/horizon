//! `horizon-logd`'s rtc surface and the hello it negotiates — the half of
//! the log wire that needs the transport.

use remoc::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use horizon_wire::{ClientHello, HubError, VersionRange};

use super::{
    IngestReply, IngestRequest, LogHubHello, LOG_PROTOCOL_VERSION,
    MIN_SUPPORTED_LOG_PROTOCOL_VERSION,
};

/// The version range this build advertises in every `hello` to `horizon-logd`.
pub fn log_version_range() -> VersionRange {
    VersionRange::new(MIN_SUPPORTED_LOG_PROTOCOL_VERSION, LOG_PROTOCOL_VERSION)
}

/// A [`ClientHello`] advertising [`log_version_range`] under `binary_id`.
pub fn log_client_hello(binary_id: impl Into<String>) -> ClientHello {
    ClientHello::new(log_version_range(), binary_id)
}

/// The domain error `ingest` returns. Distinct from [`HubError`] (which is
/// `horizon-wire`'s shared protocol-level vocabulary) because the board
/// domain has its own error shape (`ItemNotFound`, `RankExhausted`) that
/// should survive the wire round-trip as typed data, not be stringified into
/// `HubError::Call`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
pub enum LogError {
    #[error("{0}")]
    InvalidOperation(String),
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

/// The log hub — `horizon-logd`'s remoc rtc surface (`docs/logd-design.md`).
///
/// `hello` and `drain` return [`HubError`] (the shared protocol vocabulary);
/// `ingest` returns [`LogError`] (the board domain vocabulary). The subscribe
/// stream is **not** an rtc method — it rides a raw NDJSON line on the same
/// socket, sniffed apart from chmux by the first byte.
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
}
