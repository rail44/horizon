//! Network-proxy leg of the agent approval trust model
//! (`docs/agent-approval-design.md`, "Sandbox architecture" / "Staging" leg
//! 4): a domain-allowlist HTTP/HTTPS CONNECT proxy, plus the
//! UNIX-domain-socket bridge that is the *only* way a `horizon-sandbox`
//! container reaches it.
//!
//! ```no_run
//! use horizon_sandbox_proxy::{Allowlist, AllowlistProxy, UdsBridge};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let allowlist = Allowlist::new(["example.com"]);
//! let proxy = AllowlistProxy::spawn(allowlist).await?;
//! let bridge = UdsBridge::spawn("/tmp/horizon-sandbox-proxy.sock".into(), proxy.addr()).await?;
//!
//! // Pass `bridge.socket_path()` as `horizon_sandbox::NetworkPolicy::Proxied
//! // { bridge_socket }` for every sandboxed spawn that should reach the
//! // network through this allowlist.
//! # let _ = bridge.socket_path();
//! # Ok(())
//! # }
//! ```
//!
//! ## Architecture (owner-pinned, 2026-07-19)
//!
//! - **One long-lived proxy per `horizon-sessiond` process**, shared across
//!   every sandboxed session -- not stood up per command (the cost profile
//!   the sandbox survey rejected for `srt`). This crate provides the
//!   standalone proxy + bridge and their tests; wiring a single instance
//!   into `horizon-sessiond`'s own lifecycle, a `[network]` config surface,
//!   and the one-time new-domain-approval UX are the follow-up legs.
//! - **Reachability is a UNIX-socket bridge, not a loosened seccomp.** A
//!   sandboxed process never talks AF_INET at all (`horizon-sandbox`'s
//!   seccomp cut stays exactly as strict under `NetworkPolicy::Proxied` as
//!   under `Disabled` -- see that crate's `linux::spawn`); the *only*
//!   network-shaped hole is an AF_UNIX socket file bind-mounted in
//!   (AF_UNIX was already outside the seccomp denylist). [`UdsBridge`]
//!   relays whatever bytes arrive on that socket into a loopback TCP
//!   connection to [`AllowlistProxy`], which never itself runs inside any
//!   sandbox.
//! - **No MITM by default.** [`AllowlistProxy`] allows or refuses a CONNECT
//!   purely by looking at the target host in the CONNECT authority (or, for
//!   plain HTTP, the request's own host) -- see `handler`'s module doc for
//!   why declining interception makes hudsucker's own CONNECT path a
//!   transparent, byte-for-byte tunnel rather than a TLS-terminating one.

mod allowlist;
mod bridge;
mod error;
mod handler;
mod proxy;

pub use allowlist::Allowlist;
pub use bridge::UdsBridge;
pub use error::ProxyError;
pub use handler::{DENIAL_HEADER, DENIAL_REASON_NOT_ALLOWLISTED};
pub use proxy::AllowlistProxy;

#[cfg(test)]
mod tests;
