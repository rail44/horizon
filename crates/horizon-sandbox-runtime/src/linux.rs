//! Linux seccomp-notify policy primitives.
//!
//! Derived from `nono-cli/src/exec_strategy.rs` and
//! `nono-cli/src/exec_strategy/supervisor_linux.rs` at nono v0.68.0 commit
//! `00692e8c7846c6ee00ad6239d1be3b9e9b8d5dea`; modified for Horizon by
//! removing CLI-specific policy, PTY, trust, rollback, and session concerns.

use std::path::PathBuf;
use std::time::Instant;

mod filter;
mod mutation;
mod network;
mod open;

pub(crate) use filter::install as install_combined_filter;
pub(crate) use mutation::{
    handle_path_mutation_notification, is_legacy_open, is_path_mutation, path_mutation_syscalls,
};
pub(crate) use network::{handle_network_notification, NetworkEnforcement};
pub(crate) use open::{handle_open_notification, InitialCapability, OpenNotificationContext};

/// Which syscall families the helper's combined filter routes to the
/// supervisor. `openat`/`openat2` are always notified.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SupervisionPolicy {
    /// Mediate socket creation, connect/bind, and destination-bearing sends.
    pub(crate) network_mediation: bool,
    /// Mediate the path-mutating syscalls (`renameat2`, `linkat`, `unlinkat`,
    /// `mkdirat`, `truncate`, legacy `open`, ...). Landlock allows a writable
    /// root as a whole, so the supervisor is the only enforcement of a write
    /// exclusion beneath it (board #68).
    pub(crate) path_mutation_mediation: bool,
}

impl SupervisionPolicy {
    /// Path mutation mediation is on exactly when some capability carries a
    /// write exclusion (a root's `writable_root_exclusions` or a grant's
    /// `excluded_subpaths`); a sandbox without exclusions keeps the
    /// open/network-only filter.
    pub(crate) fn from_exclusions(
        network_mediation: bool,
        excluded_by_path: &[(PathBuf, Vec<PathBuf>)],
    ) -> Self {
        Self {
            network_mediation,
            path_mutation_mediation: excluded_by_path
                .iter()
                .any(|(_, excluded)| !excluded.is_empty()),
        }
    }

    /// io_uring can submit opens, renames, unlinks, and connects that seccomp
    /// never sees, so it is refused whenever anything beyond the always-on
    /// open handler is mediated.
    pub(crate) const fn notifies_io_uring_setup(self) -> bool {
        self.network_mediation || self.path_mutation_mediation
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
    fn limiter_enforces_its_burst() {
        let mut limiter = RateLimiter::new(0, 2);
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire());
    }
}
