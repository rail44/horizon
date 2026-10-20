//! [`JudgeHandle`]: the per-session-installed bundle the approval gate fires
//! through -- model id, decision client, ask threshold, rate limiter, and
//! the event-log writer the verdict record rides. Constructed once per session
//! (`horizon-agentd`'s `session::run_session`, mirroring how
//! `tools::SessionNetworkProxy` is constructed there) and threaded onto
//! `ToolSessionState` via `with_judge`, exactly like the network proxy.

use futures_util::FutureExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::contract::SessionId;
use crate::decision::{DecisionClient, DecisionConfig, HttpDecisionClient};
use crate::persistence::event_log::WriterHandle;

use super::ratelimit::RateLimiter;
use super::{
    record, run_judge, ApprovalCandidate, ApprovalJudgment, JudgeApprovalContext,
    JudgeFallbackReason, JudgeInput, JudgeVerdict,
};

const JUDGE_TIMEOUT: Duration = Duration::from_secs(60);

/// The environment variable that moves the ask threshold without a rebuild
/// (`docs/agent-decision-model-design.md`'s "Measurement switches").
const ASK_THRESHOLD_VAR: &str = "HORIZON_AGENT_JUDGE_ASK_THRESHOLD";
const DEFAULT_ASK_THRESHOLD: f64 = 0.2;

pub struct JudgeHandle {
    model: String,
    client: Arc<dyn DecisionClient>,
    ask_threshold: f64,
    limiter: RateLimiter,
    writer: WriterHandle,
    timeout: Duration,
}

impl JudgeHandle {
    /// A session retains the `decision_model` connection accepted when it
    /// started, and judges with that pair's model. No connection (the pair
    /// is absent or invalid), missing credentials, or missing persistence
    /// leave the human approval path active.
    pub fn new(
        decision: Option<&DecisionConfig>,
        writer: Option<WriterHandle>,
    ) -> Option<Arc<Self>> {
        let writer = writer?;
        let decision = decision?;
        std::env::var_os(&decision.api_key_env)?;
        Some(Arc::new(Self {
            model: decision.model.clone(),
            client: Arc::new(HttpDecisionClient::new(decision.clone())),
            ask_threshold: ask_threshold_from(std::env::var(ASK_THRESHOLD_VAR).ok().as_deref()),
            limiter: RateLimiter::judge_default(),
            writer,
            timeout: JUDGE_TIMEOUT,
        }))
    }

    /// The resolved ask threshold: a call auto-approves when the decision
    /// model's `ask_human` probability is at or below it.
    pub fn ask_threshold(&self) -> f64 {
        self.ask_threshold
    }

    /// Test-only constructor: injects a fake [`DecisionClient`] so tests
    /// never place a real network call. Uses the default ask threshold and
    /// the production rate limit and timeout.
    #[cfg(test)]
    pub(super) fn for_test(
        model: impl Into<String>,
        client: Arc<dyn DecisionClient>,
        writer: WriterHandle,
    ) -> Arc<Self> {
        Arc::new(Self {
            model: model.into(),
            client,
            ask_threshold: DEFAULT_ASK_THRESHOLD,
            limiter: RateLimiter::judge_default(),
            writer,
            timeout: JUDGE_TIMEOUT,
        })
    }

    #[cfg(test)]
    pub(super) fn for_test_with_limits(
        model: impl Into<String>,
        client: Arc<dyn DecisionClient>,
        writer: WriterHandle,
        timeout: Duration,
        rate_per_sec: f64,
        burst: f64,
    ) -> Arc<Self> {
        Arc::new(Self {
            model: model.into(),
            client,
            ask_threshold: DEFAULT_ASK_THRESHOLD,
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
                std::panic::AssertUnwindSafe(run_judge(
                    &handle.model,
                    handle.client.as_ref(),
                    &input,
                    handle.ask_threshold,
                )).catch_unwind(),
            )
                => result,
            };
            let verdict = match result {
                Ok(Ok(verdict)) => verdict,
                Ok(Err(panic)) => JudgeVerdict::fallback(
                    JudgeFallbackReason::ClientError,
                    Some(super::bounded_fallback_error(&format!(
                        "judge panicked: {}",
                        panic
                            .downcast_ref::<&str>()
                            .copied()
                            .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                            .unwrap_or("non-string panic payload")
                    ))),
                ),
                Err(_) => JudgeVerdict::fallback(JudgeFallbackReason::Timeout, None),
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
                handle.ask_threshold,
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

/// Parses the ask threshold's environment value: unset means the default;
/// a number is clamped to `0.0..=1.0`; anything else (including a
/// non-finite number) falls back to the default with one warning.
fn ask_threshold_from(value: Option<&str>) -> f64 {
    let Some(value) = value else {
        return DEFAULT_ASK_THRESHOLD;
    };
    match value.trim().parse::<f64>() {
        Ok(threshold) if threshold.is_finite() => threshold.clamp(0.0, 1.0),
        _ => {
            eprintln!(
                "horizon-agent: {ASK_THRESHOLD_VAR}={value:?} is not a number; using the \
                 default {DEFAULT_ASK_THRESHOLD}"
            );
            DEFAULT_ASK_THRESHOLD
        }
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

    fn connection(api_key_env: &str) -> DecisionConfig {
        DecisionConfig {
            base_url: Some("https://ollama.com/v1".into()),
            api_key_env: api_key_env.into(),
            model: "clef:cloud".into(),
        }
    }

    #[test]
    fn no_decision_model_connection_leaves_human_approval_active() {
        let (writer, path) = writer();
        assert!(JudgeHandle::new(None, Some(writer)).is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_unset_key_variable_leaves_human_approval_active() {
        let (writer, path) = writer();
        let connection = connection("HORIZON_JUDGE_TEST_UNSET_KEY");
        assert!(JudgeHandle::new(Some(&connection), Some(writer)).is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_judge_runs_the_decision_model_pairs_model() {
        let (writer, path) = writer();
        // `PATH` is always set, standing in for a present key variable.
        let judge = JudgeHandle::new(Some(&connection("PATH")), Some(writer)).expect("judge");
        assert_eq!(judge.model, "clef:cloud");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_ask_threshold_parses_clamps_and_falls_back() {
        assert_eq!(ask_threshold_from(None), 0.2);
        assert_eq!(ask_threshold_from(Some("0.35")), 0.35);
        assert_eq!(ask_threshold_from(Some("7")), 1.0);
        assert_eq!(ask_threshold_from(Some("-1")), 0.0);
        assert_eq!(ask_threshold_from(Some("abc")), 0.2);
        assert_eq!(ask_threshold_from(Some("NaN")), 0.2);
    }

    #[test]
    fn the_handle_reads_the_threshold_from_the_environment() {
        // nextest runs each test in its own process, so this variable
        // cannot leak into another test.
        std::env::set_var(ASK_THRESHOLD_VAR, "0.35");
        let (writer, path) = writer();
        let judge = JudgeHandle::new(Some(&connection("PATH")), Some(writer)).expect("judge");
        assert_eq!(judge.ask_threshold(), 0.35);
        let _ = std::fs::remove_file(path);
    }
}
