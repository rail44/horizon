//! [`JudgeHandle`]: the per-session-installed bundle the approval gate fires
//! through -- model id, pooled client, rate limiter, and the event-log
//! writer the verdict record rides. Constructed once per session
//! (`horizon-agentd`'s `session::run_session`, mirroring how
//! `tools::SessionNetworkProxy` is constructed there) and threaded onto
//! `ToolSessionState` via `with_judge`, exactly like the network proxy.

use futures_util::FutureExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::contract::SessionId;
use crate::persistence::event_log::WriterHandle;

use super::client::{ModelClient, RigModelClient};
use super::ratelimit::RateLimiter;
use super::{
    record, run_judge, ApprovalCandidate, ApprovalJudgment, JudgeApprovalContext, JudgeDecision,
    JudgeFallbackReason, JudgeInput,
};

const JUDGE_TIMEOUT: Duration = Duration::from_secs(60);

pub struct JudgeHandle {
    model: String,
    client: Arc<dyn ModelClient>,
    limiter: RateLimiter,
    writer: WriterHandle,
    timeout: Duration,
}

impl JudgeHandle {
    /// A session retains the `small_model` connection accepted when it
    /// started, and judges with that pair's model. No connection (the pair
    /// is absent or invalid), missing credentials, or missing persistence
    /// leave the human approval path active.
    pub fn new(
        connection: Option<&crate::auxiliary::AuxiliaryConfig>,
        writer: Option<WriterHandle>,
    ) -> Option<Arc<Self>> {
        let writer = writer?;
        let connection = connection?;
        std::env::var_os(&connection.api_key_env)?;
        Some(Arc::new(Self {
            model: connection.model.clone(),
            client: Arc::new(RigModelClient::new(connection.clone())),
            limiter: RateLimiter::judge_default(),
            writer,
            timeout: JUDGE_TIMEOUT,
        }))
    }

    /// Test-only constructor: injects a fake [`ModelClient`] so tests never
    /// place a real network call. `pub(super)` (not `pub(crate)`): the
    /// integration tests that exercise it live in `judge`'s own `tests`
    /// module (a sibling of this module, both descendants of `judge`) --
    /// see `judge::mod`'s own test module for why they aren't in `policy`'s
    /// or `tools`'s tests instead (keeping [`ModelClient`] itself scoped to
    /// `pub(in crate::judge)` rather than widening it crate-wide).
    #[cfg(test)]
    pub(super) fn for_test(
        model: impl Into<String>,
        client: Arc<dyn ModelClient>,
        writer: WriterHandle,
    ) -> Arc<Self> {
        Arc::new(Self {
            model: model.into(),
            client,
            limiter: RateLimiter::judge_default(),
            writer,
            timeout: JUDGE_TIMEOUT,
        })
    }

    #[cfg(test)]
    pub(super) fn for_test_with_limits(
        model: impl Into<String>,
        client: Arc<dyn ModelClient>,
        writer: WriterHandle,
        timeout: Duration,
        rate_per_sec: f64,
        burst: f64,
    ) -> Arc<Self> {
        Arc::new(Self {
            model: model.into(),
            client,
            limiter: RateLimiter::new(rate_per_sec, burst),
            writer,
            timeout,
        })
    }

    /// Starts one judgment on the dedicated runtime and returns immediately.
    /// `false` means the rate limiter declined the work and the caller must
    /// preserve the human approval path.
    pub(crate) fn start(
        self: &Arc<Self>,
        session_id: SessionId,
        candidate: ApprovalCandidate,
        prior_user_messages: Vec<String>,
        context: JudgeApprovalContext,
        result_tx: crossbeam_channel::Sender<crate::tools::ToolCompletion>,
    ) -> bool {
        let request = &candidate.request;
        let input = JudgeInput {
            identity: request.identity(),
            tool_id: request.tool_id.clone(),
            args: request.input.0.clone(),
            tool_description: super::builtin_tool_description(&request.tool_id),
            prior_user_messages,
            requested_filesystem_grants: context.requested_filesystem_grants,
            requested_domains: context.requested_domains,
            host_execution_requested: context.host_execution_requested,
            git_metadata_operation: context.git_metadata_operation,
        };
        if !self.limiter.try_acquire() {
            record::write_skipped(
                &self.writer,
                session_id,
                &input,
                &self.model,
                "rate_limited",
            );
            return false;
        }

        let registration = crate::tools::background::Registration::new(
            session_id,
            crate::tools::background::Lifetime::Call(request.identity()),
            crate::tools::background::WorkKind::Judgment,
        );
        let token = registration.token();
        let handle = Arc::clone(self);
        super::runtime::runtime().spawn(async move {
            let started_at = Instant::now();
            let result = tokio::select! {
                biased;
                _ = token.cancelled() => return,
                result = tokio::time::timeout(
                handle.timeout,
                std::panic::AssertUnwindSafe(run_judge(&handle.model, handle.client.as_ref(), &input)).catch_unwind(),
            )
                => result,
            };
            let verdict = match result {
                Ok(Ok(verdict)) => verdict,
                Ok(Err(panic)) => super::JudgeVerdict {
                    decision: JudgeDecision::Escalate,
                    stage: 0,
                    confidence: None,
                    fallback_reason: Some(JudgeFallbackReason::ClientError),
                    fallback_error: Some(super::bounded_fallback_error(&format!(
                        "judge panicked: {}",
                        panic
                            .downcast_ref::<&str>()
                            .copied()
                            .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                            .unwrap_or("non-string panic payload")
                    ))),
                },
                Err(_) => super::JudgeVerdict {
                    decision: JudgeDecision::Escalate,
                    stage: 0,
                    confidence: None,
                    fallback_reason: Some(JudgeFallbackReason::Timeout),
                    fallback_error: None,
                },
            };
            if !registration.finish() {
                return;
            }
            let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
            record::write_verdict(
                &handle.writer,
                session_id,
                &input,
                &handle.model,
                &verdict,
                latency_ms,
            );
            let _ = result_tx.send(crate::tools::ToolCompletion::ApprovalJudged(
                ApprovalJudgment {
                    candidate,
                    decision: verdict.decision,
                },
            ));
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer() -> (WriterHandle, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "horizon-agent-judge-handle-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        (WriterHandle::open(&path).0, path)
    }

    #[test]
    fn no_small_model_connection_leaves_human_approval_active() {
        let (writer, path) = writer();
        assert!(JudgeHandle::new(None, Some(writer)).is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_judge_runs_the_small_model_pairs_model() {
        let (writer, path) = writer();
        // `PATH` is always set, standing in for a present key variable.
        let connection = crate::auxiliary::AuxiliaryConfig {
            base_url: None,
            api_key_env: "PATH".into(),
            model: "small-test".into(),
        };
        let judge = JudgeHandle::new(Some(&connection), Some(writer)).expect("judge");
        assert_eq!(judge.model, "small-test");
        let _ = std::fs::remove_file(path);
    }
}
