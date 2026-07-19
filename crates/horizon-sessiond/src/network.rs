//! Owns this process's single long-lived network-proxy pair
//! (`docs/agent-approval-design.md`, "Staging" leg 4a): one
//! `horizon_sandbox_proxy::AllowlistProxy` + `UdsBridge`, stood up once at
//! daemon startup and kept alive for the whole process, per the owner's
//! pinned lifecycle decision ("one long-lived proxy per horizon-sessiond
//! process" -- see that crate's root doc). Every isolated, sandboxed `bash`
//! call gets `horizon_sandbox::NetworkPolicy::Proxied` against the same
//! bridge socket (see `session::run_session`'s `ToolSessionState::
//! with_bridge_socket` and `horizon-agent`'s `tools::execution::
//! execute_tier1_bash`).
//!
//! Runs on its own dedicated tokio runtime -- neither `main`'s own
//! accept-loop runtime (this binary's `#[tokio::main]`) nor a session's
//! per-session `rig` runtime (`horizon_agent::providers::rig::session`,
//! itself a separate current-thread runtime per session). The allowlist
//! starts empty (leg 4b adds a config surface and approval-driven
//! mutation), so right now this only ever *refuses* egress -- see
//! `horizon-agent`'s `tools::bash::exec::run_sandboxed` doc comment for
//! what that means for a network-using command today.

use std::path::PathBuf;

use anyhow::Context;
use horizon_sandbox_proxy::{Allowlist, AllowlistProxy, UdsBridge};

/// The running proxy + bridge pair, plus the dedicated runtime driving
/// them. `main` holds this for the process's whole lifetime; dropping it
/// (reached on the graceful SIGTERM shutdown path -- see that module's doc)
/// tears both down -- the runtime via the explicit [`Drop`] impl below
/// (never its own default `Drop`, which blocks; see that impl's doc
/// comment), `proxy`/`bridge` via their own ordinary `Drop` impls. The
/// `Control::Drain` / `std::process::exit(0)` paths skip this the same way
/// they already skip removing the main socket file -- the OS reclaims the
/// listeners/threads at process exit either way.
pub(crate) struct NetworkProxy {
    // `Option` so the custom `Drop` impl below can `.take()` it and call
    // `shutdown_background()` explicitly -- see that impl's doc comment.
    runtime: Option<tokio::runtime::Runtime>,
    // Never read again after construction (its background task is what
    // matters), but must stay alive for the same reason `runtime` does --
    // underscore-prefixed so `dead_code` doesn't flag it.
    _proxy: AllowlistProxy,
    bridge: UdsBridge,
}

/// `tokio::runtime::Runtime`'s own `Drop` blocks the calling thread until
/// every worker/blocking-pool thread has shut down -- illegal from inside
/// `main`'s own `#[tokio::main]` async body (the same "already inside a
/// runtime" restriction [`NetworkProxy::start`]'s doc comment describes for
/// *starting* a nested runtime; tokio panics identically, "cannot drop a
/// runtime in a context where blocking is not allowed", if a plain `drop`
/// runs there instead -- confirmed by manually running this binary and
/// sending it SIGTERM). `Runtime::shutdown_background` is the documented
/// non-blocking escape hatch: it detaches the actual shutdown work instead
/// of waiting for it inline, so `main` can keep dropping `NetworkProxy`
/// (alongside every other local) as an ordinary synchronous scope-exit.
impl Drop for NetworkProxy {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl NetworkProxy {
    /// Binds a fresh `AllowlistProxy` (empty allowlist -- leg 4b's config
    /// surface and approval-driven mutation are out of scope here) and its
    /// `UdsBridge` at `bridge_socket`, both on a dedicated multi-thread
    /// tokio runtime. Fallible: a bind failure (e.g. the runtime dir isn't
    /// writable) is reported to the caller rather than panicking --
    /// `main` degrades to `bridge_socket: None` for every session, the same
    /// `NetworkPolicy::Disabled` behavior tier-1 sandboxed `bash` had before
    /// this leg.
    ///
    /// Called from `main`'s own `#[tokio::main]` async body, which is
    /// already running *inside* a tokio runtime -- so the dedicated runtime
    /// this constructs is built and driven (`block_on`) on a plain,
    /// tokio-unaware `std::thread::spawn` rather than inline: tokio hard-
    /// panics ("cannot start a runtime from within a runtime") on any
    /// attempt to `block_on` a *new* runtime from a thread the current one
    /// already considers its own (a scheduler worker, or even a
    /// `spawn_blocking` thread -- both carry a "current runtime" context).
    /// A bare OS thread has no such context, mirroring the same reasoning
    /// `horizon-agent`'s `tools::bash::exec::run_inner` already relies on
    /// for its own per-call nested runtime. This function blocks its caller
    /// (briefly -- binding two loopback/UNIX listeners) on that thread's
    /// result via a plain channel; safe here since it runs once, at
    /// startup, before `main`'s accept loop or any session thread exists to
    /// contend for the runtime's worker pool.
    pub(crate) fn start(bridge_socket: PathBuf) -> anyhow::Result<Self> {
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("horizon-sandbox-proxy-init".to_string())
            .spawn(move || {
                let result = Self::build(bridge_socket);
                let _ = result_tx.send(result);
            })
            .context("failed to spawn the network-proxy init thread")?;
        result_rx
            .recv()
            .context("network-proxy init thread exited without reporting a result")?
    }

    /// The actual construction, run on the dedicated init thread spawned by
    /// [`Self::start`] -- see that function's doc comment for why it can't
    /// run inline.
    fn build(bridge_socket: PathBuf) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("horizon-sandbox-proxy")
            .enable_all()
            .build()?;
        let (proxy, bridge) = runtime.block_on(async {
            let proxy = AllowlistProxy::spawn(Allowlist::new(Vec::<String>::new())).await?;
            let bridge = UdsBridge::spawn(bridge_socket, proxy.addr()).await?;
            Ok::<_, horizon_sandbox_proxy::ProxyError>((proxy, bridge))
        })?;
        Ok(Self {
            runtime: Some(runtime),
            _proxy: proxy,
            bridge,
        })
    }

    /// The path a sandboxed `bash` call's `NetworkPolicy::Proxied` should
    /// carry -- see `session::run_session`'s `ToolSessionState::
    /// with_bridge_socket`.
    pub(crate) fn bridge_socket(&self) -> PathBuf {
        self.bridge.socket_path().to_path_buf()
    }
}
