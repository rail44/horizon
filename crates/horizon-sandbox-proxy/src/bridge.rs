//! The UNIX-domain-socket bridge: the one thing a `horizon-sandbox`
//! container ever bind-mounts in for network access
//! (`docs/agent-approval-design.md`'s "Reachability" decision --
//! `NetworkPolicy::Proxied { bridge_socket }` in `horizon-sandbox`).
//!
//! hudsucker's `Proxy` can only ever be handed a `TcpListener` (see its
//! `ProxyBuilder::with_listener`), not a `UnixListener` -- there is no
//! public hook to feed it an arbitrary `AsyncRead + AsyncWrite` connection.
//! So this bridge sits in front of it instead: a `UnixListener` at
//! `bridge_socket` accepts a client's raw bytes and relays them
//! byte-for-byte (`copy_bidirectional`) into a fresh loopback TCP
//! connection to `AllowlistProxy::addr()`. The bytes that cross the bridge
//! are ordinary HTTP/1.1 (a CONNECT request line, or an absolute-form
//! request) -- the bridge itself does no parsing, so it composes with
//! `AllowlistHandler`'s own allow/deny logic rather than duplicating it.

use std::path::{Path, PathBuf};

use tokio::net::{TcpStream, UnixListener};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::error::ProxyError;

/// A running UDS-to-loopback-TCP relay.
pub struct UdsBridge {
    socket_path: PathBuf,
    join_handle: JoinHandle<()>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl UdsBridge {
    /// Binds a UNIX socket at `socket_path` (removing any stale file left
    /// there first -- `bind` fails outright otherwise) and relays every
    /// accepted connection to `upstream` until dropped.
    pub async fn spawn(
        socket_path: PathBuf,
        upstream: std::net::SocketAddr,
    ) -> Result<Self, ProxyError> {
        let _ = std::fs::remove_file(&socket_path);
        let listener =
            UnixListener::bind(&socket_path).map_err(|source| ProxyError::BridgeBind {
                path: socket_path.clone(),
                source,
            })?;

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((client, _)) = accepted else { continue };
                        tokio::spawn(relay(client, upstream));
                    }
                    _ = &mut shutdown_rx => break,
                }
            }
        });

        Ok(Self {
            socket_path,
            join_handle,
            shutdown: Some(shutdown_tx),
        })
    }

    /// The path a sandboxed process connects to -- also the
    /// `NetworkPolicy::Proxied { bridge_socket }` value the caller passes
    /// into `horizon_sandbox::spawn` (same absolute path both sides, per
    /// that crate's bind convention).
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

async fn relay(mut client: tokio::net::UnixStream, upstream: std::net::SocketAddr) {
    let Ok(mut server) = TcpStream::connect(upstream).await else {
        return;
    };
    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
}

impl Drop for UdsBridge {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.join_handle.abort();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}
