//! The allowlist proxy itself: a loopback-only hudsucker `Proxy` wrapping
//! [`crate::handler::AllowlistHandler`]. Bound to `127.0.0.1:0`; the sandbox
//! permits only this exact TCP endpoint.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use hudsucker::hyper_util::client::legacy::connect::HttpConnector;
use hudsucker::Proxy;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::allowlist::Allowlist;
use crate::denial_log::DenialLog;
use crate::error::ProxyError;
use crate::handler::{AllowlistHandler, NeverInterceptCa};

/// A running allowlist proxy. Dropping it aborts the background task and
/// releases the listener. As of leg 4b (`docs/agent-approval-design.md`),
/// ownership is per-session (`horizon-agent`'s `tools::network::
/// SessionNetworkProxy` holds one) rather than
/// one long-lived instance per `horizon-sessiond` process -- but nothing
/// here assumes a particular owner or lifetime; this crate's own tests
/// still create and drop one per test.
pub struct AllowlistProxy {
    addr: SocketAddr,
    join_handle: JoinHandle<()>,
    // Sending on this (or dropping it) resolves the proxy's graceful-
    // shutdown future; kept as `Option` so `Drop` can take it without a
    // second field just for that.
    shutdown: Option<oneshot::Sender<()>>,
    // Shared with `AllowlistHandler` so `Self::allow`/`Self::
    // drain_denied_hosts` can mutate/read the exact same state the handler
    // consults on every request -- see those methods' doc comments.
    allowlist: Arc<Allowlist>,
    denial_log: Arc<DenialLog>,
}

impl AllowlistProxy {
    /// Binds a loopback listener on an OS-assigned port and starts serving
    /// the proxy on a spawned task. Requires a Tokio runtime context
    /// (`tokio::spawn`).
    pub async fn spawn(allowlist: Allowlist) -> Result<Self, ProxyError> {
        Self::spawn_shared(Arc::new(allowlist)).await
    }

    /// Starts a proxy backed by an allowlist shared with its owning agent
    /// session. Host-side network tools and sandboxed clients then consult
    /// one session-scoped domain policy rather than maintaining parallel
    /// grant stores.
    pub async fn spawn_shared(allowlist: Arc<Allowlist>) -> Result<Self, ProxyError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(ProxyError::Bind)?;
        let addr = listener.local_addr().map_err(ProxyError::Bind)?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let denial_log = Arc::new(DenialLog::default());
        let handler = AllowlistHandler::new(Arc::clone(&allowlist), Arc::clone(&denial_log));

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
            allowlist,
            denial_log,
        })
    }

    /// The exact loopback address the sandboxed client may reach.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Adds `domain` to this proxy's own allowlist at runtime -- leg 4b's
    /// per-session domain-approval mutation (`docs/agent-approval-
    /// design.md`): a caller (`horizon-agent`'s `tools::network::
    /// SessionNetworkProxy`) calls this when the user approves a
    /// previously-denied domain for a session, so every later request
    /// through this same proxy (including an immediate retry of the call
    /// that got denied) can reach it. Scoped to this one `AllowlistProxy`
    /// instance only -- a per-session proxy means this never leaks to any
    /// other session's own instance.
    pub fn allow(&self, domain: impl Into<String>) {
        self.allowlist.allow(domain);
    }

    /// Reads the same live policy the request handler consults. This is
    /// primarily useful to verify that two owners were wired to the same
    /// session-scoped store rather than to parallel snapshots.
    pub fn is_allowed(&self, host: &str) -> bool {
        self.allowlist.is_allowed(host)
    }

    /// Drains every host this proxy has refused since the last drain (or
    /// since construction) -- see [`crate::denial_log::DenialLog::drain`].
    /// The mechanism `docs/agent-approval-design.md` leg 4b uses to
    /// attribute a denial to the specific bash call that triggered it,
    /// independent of that call's own exit code.
    pub fn drain_denied_hosts(&self) -> Vec<String> {
        self.denial_log.drain()
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
