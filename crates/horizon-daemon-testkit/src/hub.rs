//! Opening a remoc connection to a daemon's socket, and the one call every
//! hub answers the same way: `drain`.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use horizon_wire::WireCodec;
use remoc::RemoteSend;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

/// How long to wait for a `drain` call that is expected never to answer --
/// see [`drain_with_timeout`].
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Connects to `path`, retrying until the daemon's listener is up (a fresh
/// daemon binds shortly after spawn) or panicking after a generous budget.
pub async fn connect_with_retry(path: &Path) -> UnixStream {
    for _ in 0..200 {
        if let Ok(stream) = UnixStream::connect(path).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no daemon ever accepted a connection on {}", path.display());
}

/// Establishes the remoc connection over an already-connected stream and
/// takes the hub client the daemon hands over on the base channel.
///
/// Returns the client and the chmux mux task; the caller owns that task and
/// must abort it (which closes the socket, so a daemon's one-at-a-time
/// accept loop can serve the next connection) when done -- both suites do
/// that from their own test client's `Drop`.
pub async fn connect_hub_client<T: RemoteSend>(stream: UnixStream) -> (T, JoinHandle<()>) {
    let (read_half, write_half) = stream.into_split();
    let (conn, _base_tx, mut base_rx) =
        remoc::Connect::io::<_, _, (), T, WireCodec>(remoc::Cfg::default(), read_half, write_half)
            .await
            .expect("remoc connect to the real daemon");
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let hub = base_rx
        .recv()
        .await
        .expect("base channel recv")
        .expect("the daemon should hand over its hub client");
    (hub, conn_task)
}

/// Awaits a hub's `drain` call, discarding its outcome. The daemon exits
/// inside the call, so the reply never travels -- the call resolves as a
/// transport error, which is expected; the caller confirms the exit via
/// [`crate::wait_for_exit`]. The timeout only bounds the pathological case
/// where neither a reply nor a disconnect ever arrives.
pub async fn drain_with_timeout<F: Future>(drain: F) {
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, drain).await;
}
