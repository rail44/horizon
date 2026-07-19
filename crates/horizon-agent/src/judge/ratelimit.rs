//! A minimal, dependency-free token-bucket rate limiter guarding judge model
//! calls -- `docs/agent-approval-design.md`'s "Why the denial-counting
//! backstop is dropped" names the reference figure: nono's own sandboxed-
//! command rate limit, 10 req/s with a burst of 5. Kept simple and
//! proxy-agnostic (no assumption about what's on the other end of the
//! call, just a call-count budget), and non-blocking: [`RateLimiter::
//! try_acquire`] never waits, it only ever says yes or no, so a caller can
//! skip a judge call outright rather than queuing or stalling anything.

use std::sync::Mutex;
use std::time::Instant;

pub(super) struct RateLimiter {
    state: Mutex<State>,
    rate_per_sec: f64,
    burst: f64,
}

struct State {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    pub(super) fn new(rate_per_sec: f64, burst: f64) -> Self {
        Self {
            state: Mutex::new(State {
                tokens: burst,
                last_refill: Instant::now(),
            }),
            rate_per_sec,
            burst,
        }
    }

    /// nono's own reference figures for a sandboxed session: 10 req/s,
    /// burst 5.
    pub(super) fn judge_default() -> Self {
        Self::new(10.0, 5.0)
    }

    /// `true` and consumes one token if the budget allows this call right
    /// now; `false` (consuming nothing) if the caller should skip this
    /// call's judging entirely -- never blocks.
    pub(super) fn try_acquire(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_refill).as_secs_f64();
        state.tokens = (state.tokens + elapsed * self.rate_per_sec).min(self.burst);
        state.last_refill = now;

        if state.tokens >= 1.0 {
            state.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn allows_up_to_the_burst_immediately() {
        let limiter = RateLimiter::new(10.0, 3.0);
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire(), "burst of 3 must be exhausted");
    }

    #[test]
    fn refills_over_time_up_to_the_burst_cap() {
        let limiter = RateLimiter::new(1000.0, 1.0);
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire(), "single-token burst must be empty");

        sleep(Duration::from_millis(5));
        assert!(
            limiter.try_acquire(),
            "1000 req/s must refill within a few milliseconds"
        );
    }

    #[test]
    fn judge_default_matches_the_documented_nono_reference_figures() {
        let limiter = RateLimiter::judge_default();
        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.try_acquire(), "burst of 5 must be exhausted");
    }
}
