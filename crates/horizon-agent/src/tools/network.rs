//! Owns the network-proxy leg of the agent approval trust model
//! (`docs/agent-approval-design.md`'s "Staging" leg 4b): a per-session
//! `horizon_sandbox_proxy::AllowlistProxy` + `UdsBridge` pair, so a tier-1
//! sandboxed `bash` call's `NetworkPolicy::Proxied { bridge_socket }`
//! reaches a proxy this crate itself owns, rather than one threaded in as a
//! start-session argument from `horizon-sessiond` (leg 4a's shape --
//! `horizon-sessiond`'s own `network.rs` is gone).
//!
//! **Ownership moved from the daemon to here** (owner decision, leg 4b):
//! the proxy's responsibility sits with the agent implementation, which
//! already owns every other piece of per-session tool state (`tools::state::
//! ToolSessionState`) -- `horizon-sessiond` becomes a pure consumer, handing
//! this crate an `isolated`/sandbox-availability fact and getting back a
//! handle it threads into `ToolSessionState` exactly like `with_skills`/
//! `with_config_path` already do.
//!
//! **One instance per session, not one per process.** Leg 4a's single
//! per-daemon proxy had one shared, empty allowlist -- fine for "refuse
//! everything" (the only posture that existed then), but leg 4b needs a
//! *distinct* allowlist per isolated session (approving a domain for one
//! session must never leak to another's), and nono's per-session bridge
//! socket path is trivially available (no bind-mount constraint to route
//! around, unlike the old bwrap backend) -- see [`SessionNetworkProxy::
//! start`]. A dedicated `AllowlistProxy`/`UdsBridge` pair per session is the
//! natural per-session attribution unit: mutating one session's allowlist
//! (`SessionNetworkProxy::allow_domain`) touches only that instance's own
//! state, with zero shared mutable state across sessions to accidentally
//! leak through.
//!
//! **A shared runtime, not a thread per session.** Every session's own
//! `AllowlistProxy`/`UdsBridge` pair is still cheap (a couple of tokio
//! tasks each), so rather than spin up a dedicated OS thread + tokio runtime
//! per session (wasteful under many concurrent isolated sessions), this
//! module lazily starts *one* dedicated multi-thread runtime for the whole
//! process the first time any session needs it, and hosts every session's
//! proxy/bridge tasks on that one runtime -- mirroring leg 4a's "own
//! runtime, never the per-session `rig` runtime" rule (`providers::rig::
//! session`'s own current-thread runtime is busy running that session's
//! turn loop; a nested `tokio::spawn` from inside it would compete with
//! that instead of running independently), just shared across sessions
//! rather than duplicated per session. The runtime is process-lifetime
//! (never explicitly shut down): as a `'static`, it's simply reclaimed by
//! the OS at process exit like every other thread, the same posture
//! `horizon-sessiond`'s now-deleted `network.rs` already accepted for the
//! abrupt `Control::Drain`/`std::process::exit(0)` paths. A session's own
//! `SessionNetworkProxy`, by contrast, *is* torn down on that session's own
//! `Drop` (via `AllowlistProxy`/`UdsBridge`'s own `Drop` impls) -- both are
//! safe to drop from any thread (`JoinHandle::abort`/`oneshot::Sender::send`
//! need no "current runtime" context), so no explicit shutdown dance is
//! needed here the way leg 4a's process-lifetime `Runtime` required.

use std::path::Path;

use anyhow::Context;
use horizon_sandbox_proxy::{Allowlist, AllowlistProxy, UdsBridge};

/// The dedicated runtime every session's `AllowlistProxy`/`UdsBridge` pair
/// is spawned onto -- see the module doc's "A shared runtime" section.
/// Built lazily on first use rather than at process startup: most sessions
/// are never isolated+sandboxed at all (non-isolated sessions never reach
/// tier 1), so a process that never starts one pays nothing.
fn network_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("horizon-agent-network-proxy")
            .enable_all()
            .build()
            .expect("failed to build the shared network-proxy runtime")
    })
}

/// One session's own allowlist proxy + UNIX-socket bridge pair (`docs/
/// agent-approval-design.md` leg 4b). `Send + Sync`: crossed onto the bash
/// background thread (`tools::bash::exec::run_sandboxed`) the same way
/// `ToolSessionState`'s other `Send`-able handles are (see `bash_cwd_handle`).
pub struct SessionNetworkProxy {
    proxy: AllowlistProxy,
    bridge: UdsBridge,
}

impl SessionNetworkProxy {
    /// Binds a fresh `AllowlistProxy` (empty allowlist -- nothing is
    /// approved yet) and its `UdsBridge` at a fresh, process-unique socket
    /// path under the host's temp directory, both on the shared [`network_runtime`].
    /// Fallible: a bind failure (e.g. the temp directory isn't writable) is
    /// reported to the caller rather than panicking -- the caller
    /// (`horizon-sessiond`'s `session::run_session`) degrades to no network
    /// proxy for the session, the same `NetworkPolicy::Disabled` fallback
    /// tier-1 sandboxed `bash` already had before leg 4a.
    ///
    /// Spawns a throwaway OS thread to do the actual `block_on` (mirroring
    /// leg 4a's own `NetworkProxy::start`): this may be called from a
    /// thread already inside some *other* tokio runtime's context (e.g. a
    /// session's own dedicated thread, or an async task on `horizon-
    /// sessiond`'s accept-loop runtime), and tokio hard-panics
    /// ("cannot start a runtime from within a runtime") on any attempt to
    /// `block_on` a *different* runtime from such a thread. A bare OS
    /// thread has no such context, so it can safely drive `network_runtime()`
    /// via `block_on` regardless of what context the caller is running in.
    /// Blocks the caller (briefly -- binding two loopback/UNIX listeners)
    /// on that thread's result via a plain channel.
    pub fn start() -> anyhow::Result<Self> {
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("horizon-agent-network-proxy-init".to_string())
            .spawn(move || {
                let result = Self::build();
                let _ = result_tx.send(result);
            })
            .context("failed to spawn the session network-proxy init thread")?;
        result_rx
            .recv()
            .context("session network-proxy init thread exited without reporting a result")?
    }

    fn build() -> anyhow::Result<Self> {
        let bridge_socket = std::env::temp_dir().join(format!(
            "horizon-agent-sandbox-proxy-{}.sock",
            uuid::Uuid::new_v4()
        ));
        let (proxy, bridge) = network_runtime().block_on(async {
            let proxy = AllowlistProxy::spawn(Allowlist::new(Vec::<String>::new())).await?;
            let bridge = UdsBridge::spawn(bridge_socket, proxy.addr()).await?;
            Ok::<_, horizon_sandbox_proxy::ProxyError>((proxy, bridge))
        })?;
        Ok(Self { proxy, bridge })
    }

    /// The path a sandboxed `bash` call's `NetworkPolicy::Proxied` should
    /// carry -- see `tools::bash::exec::run_sandboxed`.
    pub fn bridge_socket(&self) -> &Path {
        self.bridge.socket_path()
    }

    /// Adds `domain` to this session's own allowlist -- called once the
    /// user approves a domain a prior sandboxed call was denied for
    /// (`tools::approval`'s domain-denial-retry path). Scoped to this
    /// session's own `AllowlistProxy` instance only: no other session's
    /// `SessionNetworkProxy` is ever touched.
    pub fn allow_domain(&self, domain: impl Into<String>) {
        self.proxy.allow(domain);
    }

    /// Drains every host this session's proxy has refused since the last
    /// drain -- `tools::bash::exec::run_sandboxed` calls this right after a
    /// sandboxed child exits, so a call that hit the allowlist can be
    /// attributed to the exact domain(s) it was denied, independent of the
    /// child's own exit code (backlog 59: a piped command can exit `0`
    /// even though the network call itself was refused).
    pub fn drain_denied_hosts(&self) -> Vec<String> {
        self.proxy.drain_denied_hosts()
    }
}
