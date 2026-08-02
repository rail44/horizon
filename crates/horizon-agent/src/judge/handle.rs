//! [`JudgeHandle`]: the per-session-installed bundle the approval gate fires
//! through -- model id, pooled client, rate limiter, and the event-log
//! writer the verdict record rides. Constructed once per session
//! (`horizon-agentd`'s `session::run_session`, mirroring how
//! `tools::SessionNetworkProxy` is constructed there) and threaded onto
//! `ToolSessionState` via `with_judge`, exactly like the network proxy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config;
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
    /// Builds the judge handle for a session, or `None` if the judge can't
    /// actually run: no `OPENAI_API_KEY` (mirrors `RigAgentConfig::
    /// openai_enabled` -- the judge is a second model on the *same*
    /// provider, never a separate endpoint/credential), or no event-log
    /// writer configured (a verdict nobody could ever record is pointless
    /// to compute). `base_url` is the session's already-resolved provider
    /// base URL (`config::RigAgentConfig::base_url`) -- reused as-is, never
    /// re-resolved, since the judge is a second model id on the current
    /// provider, not a new endpoint.
    pub fn new(base_url: Option<String>, writer: Option<WriterHandle>) -> Option<Arc<Self>> {
        let writer = writer?;
        std::env::var_os(config::OPENAI_API_KEY_VAR)?;
        Some(Arc::new(Self {
            model: config::resolve_judge_model(std::env::var(config::JUDGE_MODEL_VAR).ok()),
            client: Arc::new(RigModelClient::new(base_url)),
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
            call_id: request.call_id.0.clone(),
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

        let handle = Arc::clone(self);
        super::runtime::runtime().spawn(async move {
            let started_at = Instant::now();
            let verdict = match tokio::time::timeout(
                handle.timeout,
                run_judge(&handle.model, handle.client.as_ref(), &input),
            )
            .await
            {
                Ok(verdict) => verdict,
                Err(_) => super::JudgeVerdict {
                    decision: JudgeDecision::Escalate,
                    stage: 0,
                    confidence: None,
                    fallback_reason: Some(JudgeFallbackReason::Timeout),
                },
            };
            let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
            record::write_verdict(
                &handle.writer,
                session_id,
                &input,
                &handle.model,
                verdict,
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
