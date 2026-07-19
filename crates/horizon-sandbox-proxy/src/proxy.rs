//! The allowlist proxy itself: a loopback-only hudsucker `Proxy` wrapping
//! [`crate::handler::AllowlistHandler`]. Bound to `127.0.0.1:0` (an
//! ephemeral port, loopback-only) -- a sandboxed process never talks to
//! this directly (seccomp denies it AF_INET entirely, see `horizon-sandbox`
//! `linux::seccomp`); the only way in is [`crate::bridge::UdsBridge`].

use std::net::{Ipv4Addr, SocketAddr};

use hudsucker::hyper_util::client::legacy::connect::HttpConnector;
use hudsucker::Proxy;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::allowlist::Allowlist;
use crate::error::ProxyError;
use crate::handler::{AllowlistHandler, NeverInterceptCa};

/// A running allowlist proxy. Dropping it aborts the background task and
/// releases the listener; `docs/agent-approval-design.md`'s "one long-lived
/// proxy per `horizon-sessiond` process" shape means the real caller holds
/// this for the process's whole lifetime instead of dropping it quickly,
/// but nothing here assumes that -- this spike's own tests create and drop
/// one per test.
pub struct AllowlistProxy {
    addr: SocketAddr,
    join_handle: JoinHandle<()>,
    // Sending on this (or dropping it) resolves the proxy's graceful-
    // shutdown future; kept as `Option` so `Drop` can take it without a
    // second field just for that.
    shutdown: Option<oneshot::Sender<()>>,
}

impl AllowlistProxy {
    /// Binds a loopback listener on an OS-assigned port and starts serving
    /// the proxy on a spawned task. Requires a Tokio runtime context
    /// (`tokio::spawn`).
    pub async fn spawn(allowlist: Allowlist) -> Result<Self, ProxyError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(ProxyError::Bind)?;
        let addr = listener.local_addr().map_err(ProxyError::Bind)?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handler = AllowlistHandler::new(allowlist);

        let proxy = Proxy::builder()
            .with_listener(listener)
            .with_ca(NeverInterceptCa)
            .with_http_connector(HttpConnector::new())
            .with_http_handler(handler)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .build()?;

        // Errors here (after the listener is already accepted) only
        // happen on genuine I/O failure; `abort()` in `Drop` below is the
        // normal exit path via `graceful_shutdown` above, which this
        // crate's minimal dependency footprint (no tracing, see the
        // Cargo.toml doc comment) has no logger to report through anyway.
        let join_handle = tokio::spawn(async move {
            let _ = proxy.start().await;
        });

        Ok(Self {
            addr,
            join_handle,
            shutdown: Some(shutdown_tx),
        })
    }

    /// The loopback address the proxy is actually listening on --
    /// [`crate::bridge::UdsBridge`]'s relay target.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for AllowlistProxy {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.join_handle.abort();
    }
}
