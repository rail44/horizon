//! Approval adapter for Horizon's retry-after-completion flow.
//!
//! nono's supervisor asks synchronously while a syscall is blocked. Horizon's
//! current approval contract is asynchronous and retries a fresh sandboxed
//! process, so the initial adapter records the trusted request and immediately
//! denies it. The caller can drain the audit entries after the command exits,
//! present them through Horizon's normal approval flow, add a narrow session
//! grant, and retry without ever removing containment.

use nono::supervisor::AuditEntry;
use nono::{ApprovalBackend, ApprovalDecision, ApprovalRequest};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

const BACKEND_NAME: &str = "horizon-recording-deny";
const DEFAULT_REASON: &str = "deferred to Horizon's sandboxed retry approval flow";

/// A fail-closed approval backend that preserves every structured request.
///
/// Mutex poisoning is recovered rather than propagated: a prior panic must not
/// turn a later request into an implicit grant or discard its audit evidence.
#[derive(Debug)]
pub struct RecordingDenyBackend {
    reason: String,
    entries: Mutex<Vec<AuditEntry>>,
}

impl Default for RecordingDenyBackend {
    fn default() -> Self {
        Self::new(DEFAULT_REASON)
    }
}

impl RecordingDenyBackend {
    /// Creates a backend whose denial records explain the deferred decision.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            entries: Mutex::new(Vec::new()),
        }
    }

    /// Returns a stable snapshot without consuming recorded decisions.
    pub fn snapshot(&self) -> Vec<AuditEntry> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Atomically removes and returns all decisions recorded so far.
    pub fn drain(&self) -> Vec<AuditEntry> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *entries)
    }
}

impl ApprovalBackend for RecordingDenyBackend {
    fn request_approval(&self, request: &ApprovalRequest) -> nono::Result<ApprovalDecision> {
        let started = Instant::now();
        let decision = ApprovalDecision::Denied {
            reason: self.reason.clone(),
        };
        let entry = AuditEntry {
            timestamp: SystemTime::now(),
            request: request.clone(),
            decision: decision.clone(),
            backend: BACKEND_NAME.to_string(),
            duration_ms: started.elapsed().as_millis() as u64,
        };
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry);
        Ok(decision)
    }

    fn backend_name(&self) -> &str {
        BACKEND_NAME
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nono::AccessMode;
    use std::path::PathBuf;

    fn request(id: &str) -> ApprovalRequest {
        ApprovalRequest::Capability {
            request_id: id.to_string(),
            path: PathBuf::from("/outside/worktree/cache.lock"),
            access: AccessMode::Write,
            reason: Some("openat was denied".to_string()),
            child_pid: 42,
            session_id: "session-a".to_string(),
        }
    }

    #[test]
    fn denial_is_immediate_and_request_is_audited() {
        let backend = RecordingDenyBackend::default();
        let decision = backend
            .request_approval(&request("request-1"))
            .expect("recording backend should not fail");

        assert!(decision.is_denied());
        let entries = backend.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].request.request_id(), "request-1");
        assert_eq!(entries[0].request.session_id(), "session-a");
        assert_eq!(entries[0].backend, BACKEND_NAME);
    }

    #[test]
    fn drain_is_atomic_and_preserves_later_requests() {
        let backend = RecordingDenyBackend::default();
        backend
            .request_approval(&request("request-1"))
            .expect("first denial");
        assert_eq!(backend.drain().len(), 1);
        assert!(backend.snapshot().is_empty());

        backend
            .request_approval(&request("request-2"))
            .expect("second denial");
        assert_eq!(backend.snapshot()[0].request.request_id(), "request-2");
    }
}
