//! The raw-NDJSON subscribe handler — the half of the accept loop that a
//! `{` first byte routes to (the other half is the remoc chmux `ingest`
//! path). See `docs/logd-design.md` Subscription shape.
//!
//! Protocol (one NDJSON line per message, `\n`-terminated):
//!
//! 1. subscriber → logd: `{"path":"…","since":N}` (one line; both fields
//!    optional — `{}` subscribes to all paths with no cursor).
//! 2. logd → subscriber: `{"log":"board","seq":M}` (the current seq; same
//!    shape as a streaming poke so the consumer treats every line alike).
//! 3. logd → subscriber: `{"log":"board","seq":K}` per appended line, for
//!    as long as the subscriber stays connected.
//!
//! A shell script can consume this with no SDK: `nc -U /path/to/logd.sock`
//! and type the request line. `horizon board watch` does the same from Rust.

use std::path::PathBuf;
use std::sync::Arc;

use horizon_board::wire::SubscribeRequest;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;

use crate::subscribers::SubscriberRegistry;

/// The log type for board pokes (the only log type in v1).
const LOG_TYPE: &str = "board";

/// Handles one raw-NDJSON subscribe connection to completion. Called from
/// `handle_connection` after the first-byte sniff routes here; runs on its
/// own spawned task so it never blocks the accept loop or any `ingest`.
///
/// Takes the already-buffered read half (the sniff's `fill_buf` peeked from
/// it without consuming) plus the write half — both split from the original
/// `UnixStream` by the caller.
pub async fn handle_subscribe<R>(
    reader: BufReader<R>,
    mut write_half: OwnedWriteHalf,
    registry: Arc<SubscriberRegistry>,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    // 1. Read the subscribe request (one NDJSON line).
    let mut buf_reader = reader;
    let mut line = String::new();
    if buf_reader.read_line(&mut line).await? == 0 {
        return Ok(()); // client closed before sending
    }
    let request: SubscribeRequest = serde_json::from_str(line.trim()).unwrap_or_default();

    // 2. Reply with the current seq (line count of the file).
    let key = request.path.as_ref().map(PathBuf::from);
    let current_seq = match &key {
        Some(path) => horizon_board::read_events(path)
            .map(|r| r.line_count)
            .unwrap_or(0),
        None => 0,
    };
    write_poke(&mut write_half, LOG_TYPE, current_seq).await?;

    // 3. Register for future pokes and stream them until the subscriber
    //    disconnects (write fails) or logd shuts down (channel closes).
    let mut rx = registry.register(key);
    while let Some(poke) = rx.recv().await {
        if write_poke(&mut write_half, &poke.log, poke.seq)
            .await
            .is_err()
        {
            break;
        }
    }

    Ok(())
}

/// Writes one `{"log":"<log>","seq":<seq>}\n` line to `write_half`.
async fn write_poke(write_half: &mut OwnedWriteHalf, log: &str, seq: u64) -> std::io::Result<()> {
    // Hand-serialize to keep the output byte-stable (serde_json's field
    // order is insertion order for structs, which matches our declaration
    // order — but being explicit costs nothing and makes the `nc -U`
    // output predictable for shell scripts that grep on it).
    let line = format!(r#"{{"log":"{log}","seq":{seq}}}"#);
    write_half.write_all(line.as_bytes()).await?;
    write_half.write_all(b"\n").await?;
    write_half.flush().await
}
