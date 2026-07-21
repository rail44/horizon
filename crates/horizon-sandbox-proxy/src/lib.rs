//! Network-proxy leg of the agent approval trust model
//! (`docs/agent-approval-design.md`, "Sandbox architecture" / "Staging" leg
//! 4): a loopback-only domain-allowlist HTTP/HTTPS CONNECT proxy. A
//! `horizon-sandbox` child may reach only this exact TCP endpoint.
//!
//! ```no_run
//! use horizon_sandbox_proxy::{Allowlist, AllowlistProxy};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let allowlist = Allowlist::new(["example.com"]);
//! let proxy = AllowlistProxy::spawn(allowlist).await?;
//! // Pass `proxy.addr()` as `horizon_sandbox::NetworkPolicy::Proxied` and
//! // configure the child with standard HTTP proxy environment variables.
//! # let _ = proxy.addr();
//! # Ok(())
//! # }
//! ```
//!
//! ## Architecture (owner-pinned, 2026-07-19; ownership updated leg 4b)
//!
//! - **One proxy per isolated, sandboxed session**, owned by
//!   `horizon-agent` (`tools::network::SessionNetworkProxy`) on its own
//!   dedicated tokio runtime -- not stood up per command (the cost profile
//!   the sandbox survey rejected for `srt`), and no longer one shared
//!   instance per `horizon-sessiond` process (leg 4a's shape): a per-session
//!   allowlist is what makes an approved domain scoped to the session that
//!   approved it, with zero cross-session leakage. This crate itself is
//!   unaware of "session" as a concept -- it provides the standalone proxy,
//!   an [`Allowlist`] that can grow at runtime (`Allowlist::allow`),
//!   and a denial log a caller can drain (`AllowlistProxy::
//!   drain_denied_hosts`) to attribute a refusal to whichever call
//!   triggered it, independent of that call's own exit code.
//! - **Reachability is one exact loopback TCP endpoint.** Standard proxy
//!   environment variables make common HTTP clients speak the right protocol;
//!   `horizon-sandbox` denies every other destination and protocol.
//! - **No MITM by default.** [`AllowlistProxy`] allows or refuses a CONNECT
//!   purely by looking at the target host in the CONNECT authority (or, for
//!   plain HTTP, the request's own host) -- see `handler`'s module doc for
//!   why declining interception makes hudsucker's own CONNECT path a
//!   transparent, byte-for-byte tunnel rather than a TLS-terminating one.

mod allowlist;
mod denial_log;
mod error;
mod handler;
mod proxy;

pub use allowlist::Allowlist;
pub use error::ProxyError;
pub use handler::{DENIAL_HEADER, DENIAL_REASON_NOT_ALLOWLISTED};
pub use proxy::AllowlistProxy;

#[cfg(test)]
mod tests;
