//! The `horizon-logd` socket client: every board write, and the subscribe
//! stream.
//!
//! The write path (exclusive flock + read-fold + id/rank computation +
//! append) lives in `horizon-logd` (`docs/logd-design.md`). A write here is
//! a thin socket call: connect-or-spawn logd, `hello`, send one `ingest` rtc
//! call, return the result. There is no direct-append fallback.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::store::source::{LogSource, Source};
use crate::store::types::StoreError;
use crate::store::Store;
use crate::wire::{
    log_client_hello, IngestReply, IngestRequest, LogError, LogHub, LogHubClient, SubscribeRequest,
};

/// Connects to logd (spawning it if necessary), hellos, and sends one
/// `ingest` call. Each write method on `Store` wraps this with the matching
/// `IngestRequest`/`IngestReply` variant. Pure async — the caller owns the
/// tokio runtime (the CLI creates one in `run_board`, the GUI creates one on
/// its background thread), so the library never builds a runtime inside a
/// sync method (which would panic if the caller was already on a runtime:
/// 'Cannot start a runtime from within a runtime').
pub(crate) async fn ingest(
    log: &LogSource,
    request: IngestRequest,
) -> Result<IngestReply, StoreError> {
    let socket = log.socket.clone();
    let path = log.events.to_string_lossy().to_string();
    let stream = horizon_wire::spawn::connect_or_spawn_logd_retrying(&socket)
        .await
        .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
    let (hub, conn_task) =
        horizon_wire::spawn::connect_hub_client::<LogHubClient<horizon_wire::WireCodec>>(stream)
            .await
            .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;

    hub.hello(log_client_hello(concat!(
        "horizon-board/",
        env!("CARGO_PKG_VERSION")
    )))
    .await
    .map_err(hub_error_to_store)?;

    let reply = hub
        .ingest(path, request)
        .await
        .map_err(log_error_to_store)?;

    conn_task.abort();
    Ok(reply)
}

impl Store {
    // -- subscribe (raw NDJSON, stage B) -------------------------------
    //
    // Unlike `ingest` (which rides the remoc chmux path), `subscribe` is a
    // raw NDJSON line protocol on the same socket — see `docs/logd-design.md`
    // Subscription shape. The caller owns the tokio runtime and the returned
    // [`SubscribeStream`], reading lines until the connection closes.

    /// Connects to logd (spawning it if necessary), sends the subscribe
    /// request as one NDJSON line, and returns a stream the caller reads
    /// NDJSON poke lines from (`{"log":"board","seq":N}`). The first line
    /// is the current seq (the cursor-on-connect reply); subsequent lines
    /// are pokes for each appended event.
    ///
    /// An in-memory store has no daemon and no further events, so it fails
    /// with [`StoreError::ReadOnly`] instead of connecting.
    pub async fn subscribe(&self, since: Option<u64>) -> Result<SubscribeStream, StoreError> {
        let log = match &self.source {
            Source::Log(log) => log,
            Source::Memory(_) => return Err(StoreError::ReadOnly),
        };
        let stream = horizon_wire::spawn::connect_or_spawn_logd_retrying(&log.socket)
            .await
            .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
        let (read_half, mut write_half) = stream.into_split();

        let request = serde_json::to_string(&SubscribeRequest {
            path: Some(log.events.to_string_lossy().to_string()),
            since,
        })
        .map_err(StoreError::Json)?;
        write_half
            .write_all(request.as_bytes())
            .await
            .map_err(StoreError::Io)?;
        write_half.write_all(b"\n").await.map_err(StoreError::Io)?;
        write_half.flush().await.map_err(StoreError::Io)?;

        Ok(SubscribeStream {
            reader: tokio::io::BufReader::new(read_half),
            _write: write_half,
        })
    }
}

/// A raw NDJSON line stream from logd's subscribe path. The caller reads
/// lines (one `{"log":"board","seq":N}` per appended event) until the
/// connection closes. The write half is held to keep the connection alive.
pub struct SubscribeStream {
    reader: tokio::io::BufReader<OwnedReadHalf>,
    _write: OwnedWriteHalf,
}

impl SubscribeStream {
    /// Reads the next NDJSON line from the stream. Returns `None` when logd
    /// closes the connection (drain/shutdown). The line includes the
    /// trailing `\n` stripped.
    pub async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        // Strip the trailing newline.
        if line.ends_with('\n') {
            line.pop();
        }
        Ok(Some(line))
    }
}

/// Maps a `HubError` from the `hello` call to `StoreError`. A `hello` failure
/// is a protocol/transport problem (version mismatch, lost connection), not a
/// board-domain error.
fn hub_error_to_store(err: horizon_wire::HubError) -> StoreError {
    StoreError::Io(std::io::Error::other(err.to_string()))
}

/// Maps a `LogError` from the `ingest` call to `StoreError`, preserving the
/// typed domain errors (`ItemNotFound`, `RankExhausted`).
fn log_error_to_store(err: LogError) -> StoreError {
    match err {
        LogError::InvalidOperation(msg) => StoreError::Io(std::io::Error::other(msg)),
        LogError::ItemNotFound(id) => StoreError::ItemNotFound(id),
        LogError::RankExhausted => StoreError::RankExhausted,
        LogError::Io(msg) => StoreError::Io(std::io::Error::other(msg)),
        LogError::Call(msg) => StoreError::Io(std::io::Error::other(msg)),
    }
}
