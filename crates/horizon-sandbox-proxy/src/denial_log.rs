//! Records which hosts [`crate::handler::AllowlistHandler`] has refused, so
//! a caller can attribute a denial to the specific bash call that triggered
//! it without relying on the sandboxed command's own exit code (`docs/
//! agent-approval-design.md` leg 4b -- backlog 59: `curl ... | head` exits
//! `0` even though the `curl` itself never reached the network). The proxy
//! is the only thing that ever sees the refused CONNECT/request, so this is
//! the authoritative record; a caller like `horizon-agent`'s `tools::bash::
//! exec::run_sandboxed` drains it right after a sandboxed child exits,
//! before that child's own exit code or stdout is ever consulted.

use std::sync::Mutex;

#[derive(Debug, Default)]
pub(crate) struct DenialLog {
    denied: Mutex<Vec<String>>,
}

impl DenialLog {
    /// Appends a denied host. Called from [`crate::handler::AllowlistHandler`]
    /// every time it refuses a request for a named host (never for the
    /// "no host in the request at all" malformed case -- there is no domain
    /// name worth recording there).
    pub(crate) fn record(&self, host: &str) {
        self.denied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(host.to_string());
    }

    /// Drains every host recorded since the last drain (or since
    /// construction), in denial order, possibly with duplicates if the same
    /// host was denied more than once (e.g. a script retrying the same
    /// call). Draining rather than merely reading means each caller sees
    /// only the denials that happened since its own last check -- exactly
    /// what attributing a denial to one specific bash call needs.
    pub(crate) fn drain(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .denied
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}
