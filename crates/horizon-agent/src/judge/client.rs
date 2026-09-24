//! The judge's wire-level client: a mockable [`ModelClient`] trait plus the
//! real implementation over rig's OpenAI-completions client, reusing the
//! same per-call `.model()` override pattern
//! `providers::rig::completion::completion_client` already uses for
//! the acting model.
//!
//! `logit_bias`/`logprobs` have no first-class builder methods in rig-core
//!0.42 (this held for 0.39 too, per the research doc against the vendored
//! source) -- both
//! reach the wire through `CompletionRequestBuilder::additional_params`,
//! which is `#[serde(flatten)]`-merged directly into the OpenAI-shaped
//! request JSON by the provider's own request struct. This module never
//! uses `logit_bias` (Plan B throughout -- see `judge::parse`'s doc
//! comment); it only ever sends `reasoning_effort`/`logprobs`/
//! `top_logprobs` this way.

use async_trait::async_trait;
use rig_core::client::CompletionClient;
use rig_core::completion::{AssistantContent, CompletionModel, Message};

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
/// raw `logprobs` JSON if the endpoint returned one (opaque -- rig 0.42
/// exposes the provider's serialized raw response as the untyped
/// `CompletionResponse::raw: serde_json::Value`, and this reads
/// `choices[0].logprobs` out of it).
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

/// Stage 2 sends the same `reasoning_effort: "none"` as stage 1, and no
/// confidence signal (only stage 1 derives one).
///
/// It used to leave reasoning on, on the theory that provider-side
/// reasoning *is* the chain-of-thought step. Measured against the chosen
/// judge model that was wrong in the worst way: a reasoning-first model
/// spent the whole stage-2 token budget inside its think block and never
/// emitted the `VERDICT:` line, so every single stage-2 call parsed as
/// unparseable and fell back to a human prompt (8/8 escalations in the
/// 2026-07-28 event-log investigation). The chain of thought stage 2
/// actually depends on is the one `prompt::STAGE2_SYSTEM_PROMPT` asks for
/// in the *visible* reply ("think through this in 2-4 sentences, then end
/// with VERDICT: ..."), which arrives with reasoning disabled -- exactly
/// the shape stage 1 already proves parses reliably on this provider.
pub(super) fn stage2_additional_params() -> serde_json::Value {
    serde_json::json!({
        "reasoning_effort": "none",
    })
}

/// A pooled connection retained by this session's judge handle.
pub(super) struct RigModelClient {
    client: crate::auxiliary::AuxiliaryClient,
}

impl RigModelClient {
    pub(super) fn new(config: crate::auxiliary::AuxiliaryConfig) -> Self {
        Self {
            client: crate::auxiliary::AuxiliaryClient::new(config),
        }
    }
}

#[async_trait]
impl ModelClient for RigModelClient {
    async fn complete(
        &self,
        model: &str,
        request: RawCompletionRequest,
    ) -> anyhow::Result<RawCompletionResponse> {
        let client = self.client.completion_client()?;
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
            .raw
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("logprobs"))
            .cloned();

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
    fn stage2_additional_params_disables_reasoning_like_stage1() {
        // Reasoning left on made stage 2 unparseable 100% of the time --
        // see this function's own doc comment.
        let params = stage2_additional_params();
        assert_eq!(params["reasoning_effort"], "none");
        assert!(params.get("logit_bias").is_none());
        assert!(
            params.get("logprobs").is_none(),
            "only stage 1 derives a confidence signal"
        );
    }
    #[test]
    fn auxiliary_clients_keep_their_selected_endpoint_and_credentials() {
        use crate::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        use std::time::Duration;

        fn endpoint(requests: usize) -> (String, mpsc::Receiver<String>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/v1", listener.local_addr().unwrap());
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for incoming in listener.incoming().take(requests) {
                    let mut socket = incoming.unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    loop {
                        let mut buffer = [0u8; 1024];
                        let count = socket.read(&mut buffer).unwrap();
                        assert_ne!(count, 0);
                        bytes.extend_from_slice(&buffer[..count]);
                        if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                            let length = head
                                .lines()
                                .find_map(|line| line.strip_prefix("content-length:"))
                                .unwrap()
                                .trim()
                                .parse::<usize>()
                                .unwrap();
                            if bytes.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    tx.send(String::from_utf8(bytes).unwrap()).unwrap();
                    let body = serde_json::json!({
                        "id": "aux-response", "object": "chat.completion", "created": 0,
                        "model": "helper-model",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "A"},
                            "finish_reason": "stop", "logprobs": null}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    })
                    .to_string();
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                }
            });
            (url, rx)
        }

        let (first_url, first_requests) = endpoint(3);
        let (second_url, second_requests) = endpoint(2);
        std::env::set_var("OPENAI_API_KEY", "wrong-conversation-key");
        std::env::set_var("HORIZON_TEST_AUX_FIRST_KEY", "first-helper-key");
        std::env::set_var("HORIZON_TEST_AUX_SECOND_KEY", "second-helper-key");
        std::env::set_var("OPENAI_BASE_URL", &first_url);
        let first = AuxiliaryConfig::from_env(
            Some("https://unused.invalid".into()),
            "HORIZON_TEST_AUX_FIRST_KEY".into(),
        );
        assert_eq!(first.base_url.as_deref(), Some(first_url.as_str()));
        std::env::remove_var("OPENAI_BASE_URL");
        let second =
            AuxiliaryConfig::from_env(Some(second_url), "HORIZON_TEST_AUX_SECOND_KEY".into());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let first_title = AuxiliaryClient::new(first.clone());
        assert_eq!(
            crate::summarize::summarize_session_title(&first_title, "first"),
            Some("A".into())
        );
        for config in [first, second.clone()] {
            let judge = RigModelClient::new(config);
            let response = runtime
                .block_on(judge.complete(
                    "judge-model",
                    RawCompletionRequest {
                        system_prompt: "Judge".into(),
                        user_content: "Approve?".into(),
                        max_tokens: 16,
                        additional_params: serde_json::json!({}),
                    },
                ))
                .unwrap();
            assert_eq!(response.text, "A");
        }
        assert_eq!(
            crate::summarize::summarize_session_title(&AuxiliaryClient::new(second), "second"),
            Some("A".into())
        );
        // A captured handle retains its first endpoint after the config changes.
        assert_eq!(
            crate::summarize::summarize_session_title(&first_title, "still first"),
            Some("A".into())
        );
        for (requests, key, count) in [
            (first_requests, "first-helper-key", 3),
            (second_requests, "second-helper-key", 2),
        ] {
            for _ in 0..count {
                let request = requests
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .to_ascii_lowercase();
                assert!(request.starts_with("post /v1/chat/completions "));
                assert!(request.contains(&format!("authorization: bearer {key}")));
                assert!(!request.contains("wrong-conversation-key"));
            }
        }
    }
}
