//! Linux seccomp-notify policy primitives.
//!
//! Derived from `nono-cli/src/exec_strategy.rs` and
//! `nono-cli/src/exec_strategy/supervisor_linux.rs` at nono v0.68.0 commit
//! `00692e8c7846c6ee00ad6239d1be3b9e9b8d5dea`; modified for Horizon by
//! removing CLI-specific policy, PTY, trust, rollback, and session concerns.

use std::time::Instant;

mod open;

pub(crate) use open::{handle_open_notification, InitialCapability};

/// Describes which seccomp-notify mechanisms a helper must install.
///
/// This mirrors nono-cli v0.68.0's policy shape. The future helper must fail
/// closed when a requested listener cannot be installed; it may not silently
/// fall back to opaque stderr heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeccompPolicy {
    /// Route unrecognised `openat`/`openat2` paths to the approval backend.
    pub capability_elevation: bool,
    /// Mediate connect/bind when Landlock alone cannot enforce the proxy edge.
    pub proxy_fallback: bool,
    /// Mediate pathname Unix-domain socket operations.
    pub af_unix_mediation: bool,
    /// Intercept the narrow `/proc/.../comm` compatibility write.
    pub proc_comm_notify: bool,
}

impl SeccompPolicy {
    /// Whether an `openat`/`openat2` notification listener is required.
    #[must_use]
    pub const fn needs_openat_notify(self) -> bool {
        self.capability_elevation || self.proc_comm_notify
    }

    /// Whether a connect/bind notification listener is required.
    #[must_use]
    pub const fn needs_network_notify(self) -> bool {
        self.proxy_fallback || self.af_unix_mediation
    }

    /// Whether the child must remain inspectable by its supervisor.
    #[must_use]
    pub const fn child_requires_dumpable(self) -> bool {
        self.needs_openat_notify() || self.needs_network_notify()
    }
}

/// Token-bucket limiter copied from nono-cli's Linux supervisor.
///
/// It is crate-local until the helper event loop lands; keeping it here pins
/// the upstream request-flood behavior without making it Horizon policy API.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    capacity: u32,
    tokens: u32,
    rate: u32,
    last_refill: Instant,
}

impl RateLimiter {
    pub(crate) fn new(rate: u32, burst: u32) -> Self {
        Self {
            capacity: burst,
            tokens: burst,
            rate,
            last_refill: Instant::now(),
        }
    }

    pub(crate) fn try_acquire(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill);
        let new_tokens = (elapsed.as_millis() as u64)
            .saturating_mul(self.rate as u64)
            .saturating_div(1000);
        if new_tokens > 0 {
            self.tokens = self.capacity.min(
                self.tokens
                    .saturating_add(u32::try_from(new_tokens).unwrap_or(u32::MAX)),
            );
            self.last_refill = now;
        }

        if self.tokens == 0 {
            false
        } else {
            self.tokens -= 1;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_predicates_match_the_upstream_shape() {
        let policy = SeccompPolicy {
            capability_elevation: true,
            proxy_fallback: false,
            af_unix_mediation: true,
            proc_comm_notify: false,
        };
        assert!(policy.needs_openat_notify());
        assert!(policy.needs_network_notify());
        assert!(policy.child_requires_dumpable());
    }

    #[test]
    fn limiter_enforces_its_burst() {
        let mut limiter = RateLimiter::new(0, 2);
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire());
    }
}
