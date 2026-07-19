//! The judge's wire-level client: a mockable [`ModelClient`] trait plus the
//! real implementation over rig's OpenAI-completions client, reusing the
//! same per-call `.model()` override pattern
//! `providers::rig::completion::openai_completions_client` already uses for
//! the acting model.
//!
//! `logit_bias`/`logprobs` have no first-class builder methods in rig-core
//!0.39 (confirmed by the research doc against the vendored source) -- both
//! reach the wire through `CompletionRequestBuilder::additional_params`,
//! which is `#[serde(flatten)]`-merged directly into the OpenAI-shaped
//! request JSON by the provider's own request struct. This module never
//! uses `logit_bias` (Plan B throughout -- see `judge::parse`'s doc
//! comment); it only ever sends `reasoning_effort`/`logprobs`/
//! `top_logprobs` this way.

use std::sync::OnceLock;

use async_trait::async_trait;
use rig_core::client::CompletionClient;
use rig_core::completion::{AssistantContent, CompletionModel, Message};
use rig_core::providers::openai;

/// One stage's completion request, already assembled by
/// `judge::prompt`/`judge::run_judge` -- everything a [`ModelClient`] needs
/// to actually place the call. Kept as plain data (not a rig type) so a
/// test can assert its shape without touching rig or the network -- see
/// [`stage1_additional_params`].
#[derive(Clone, Debug)]
pub(super) struct RawCompletionRequest {
    pub(super) system_prompt: String,
    pub(super) user_content: String,
    pub(super) max_tokens: u64,
    pub(super) additional_params: serde_json::Value,
}

/// A stage's parsed-enough response: the assistant's text content, and the
/// raw `logprobs` JSON if the endpoint returned one (opaque -- rig itself
/// doesn't type this, see the module doc on `providers::rig::completion`'s
/// own `Choice.logprobs: Option<serde_json::Value>`).
#[derive(Clone, Debug, Default)]
pub(super) struct RawCompletionResponse {
    pub(super) text: String,
    pub(super) logprobs: Option<serde_json::Value>,
}

/// The mockable seam over "place one completion call" -- real judge calls go
/// through [`RigModelClient`]; tests inject a fake implementation so no test
/// ever makes a real network call (`docs/agent-approval-design.md`'s judge
/// design is only ever exercised against a mock client in this crate's own
/// test suite).
#[async_trait]
pub(super) trait ModelClient: Send + Sync {
    async fn complete(
        &self,
        model: &str,
        request: RawCompletionRequest,
    ) -> anyhow::Result<RawCompletionResponse>;
}

/// Stage 1's `additional_params`: `reasoning_effort: "none"` (keeps the
/// acting-model-class reasoning models like Kimi from spending their whole
/// token budget on `reasoning_content` before ever emitting `Y`/`N` -- the
/// research doc's provider-probe appendix), plus `logprobs`/`top_logprobs`
/// for the confidence signal. Never `logit_bias` -- see the module doc.
pub(super) fn stage1_additional_params() -> serde_json::Value {
    serde_json::json!({
        "reasoning_effort": "none",
        "logprobs": true,
        "top_logprobs": 5,
    })
}

/// Stage 2 leaves reasoning on (the chain-of-thought step itself) and
/// doesn't need a confidence signal, so it sends no extra provider params.
pub(super) fn stage2_additional_params() -> serde_json::Value {
    serde_json::json!({})
}

/// Real judge model client: a pooled rig OpenAI-completions client reused
/// across every judge call in this process (a fresh client would mean a
/// fresh `reqwest::Client`, i.e. a fresh connection pool -- the ~0.5s warm
/// latency the research doc's provider probe measured depends on reusing
/// the same pooled connection, not redialing per call).
pub(super) struct RigModelClient {
    base_url: Option<String>,
}

impl RigModelClient {
    pub(super) fn new(base_url: Option<String>) -> Self {
        Self { base_url }
    }
}

/// Lazily builds (once per process) and hands back clones of the shared
/// judge `CompletionsClient` -- `openai::CompletionsClient` wraps a
/// `reqwest::Client` internally and is cheap to clone (an `Arc`-backed
/// connection pool underneath), so every judge call reuses the same pooled
/// connection regardless of which session fired it. `base_url` is only
/// consulted on the *first* call in a process's lifetime (this crate's
/// config is process-wide already -- see `config::RigAgentConfig::
/// base_url`'s own doc comment -- so this mirrors that, rather than
/// supporting a base URL that changes mid-process).
fn shared_client(base_url: Option<&str>) -> anyhow::Result<openai::CompletionsClient> {
    static CLIENT: OnceLock<Option<openai::CompletionsClient>> = OnceLock::new();
    CLIENT
        .get_or_init(|| build_client(base_url))
        .clone()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "judge model client unavailable ({} unset, or client build failed)",
                crate::config::OPENAI_API_KEY_VAR
            )
        })
}

fn build_client(base_url: Option<&str>) -> Option<openai::CompletionsClient> {
    let api_key = std::env::var(crate::config::OPENAI_API_KEY_VAR).ok()?;
    let mut builder = openai::CompletionsClient::builder().api_key(&api_key);
    if let Some(base_url) = base_url {
        builder = builder.base_url(base_url);
    }
    builder.build().ok()
}

#[async_trait]
impl ModelClient for RigModelClient {
    async fn complete(
        &self,
        model: &str,
        request: RawCompletionRequest,
    ) -> anyhow::Result<RawCompletionResponse> {
        let client = shared_client(self.base_url.as_deref())?;
        let completion_model = client.completion_model(model);
        let response = completion_model
            .completion_request(Message::user(request.user_content))
            .preamble(request.system_prompt)
            .max_tokens(request.max_tokens)
            .additional_params(request.additional_params)
            .send()
            .await?;

        let text = response
            .choice
            .into_iter()
            .find_map(|content| match content {
                AssistantContent::Text(text) => Some(text.text),
                _ => None,
            })
            .unwrap_or_default();
        let logprobs = response
            .raw_response
            .choices
            .first()
            .and_then(|choice| choice.logprobs.clone());

        Ok(RawCompletionResponse { text, logprobs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage1_additional_params_never_uses_logit_bias() {
        let params = stage1_additional_params();
        assert_eq!(params["reasoning_effort"], "none");
        assert_eq!(params["logprobs"], true);
        assert_eq!(params["top_logprobs"], 5);
        assert!(
            params.get("logit_bias").is_none(),
            "the judge must never reach for logit_bias -- Plan B only"
        );
    }

    #[test]
    fn stage2_additional_params_carries_no_reasoning_effort_override() {
        // Stage 2 leaves reasoning on: it must not carry stage 1's
        // "reasoning_effort": "none" override.
        let params = stage2_additional_params();
        assert!(params.get("reasoning_effort").is_none());
        assert!(params.get("logit_bias").is_none());
    }
}
