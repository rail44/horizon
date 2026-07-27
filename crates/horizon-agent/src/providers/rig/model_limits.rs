//! In-band model-limit discovery: `GET {base_url}/models`, parsed for the
//! per-model `context_length`/`max_output_length` the configured provider
//! declares.
//!
//! `docs/agent-compaction-design.md` Tier 1 derives its trigger from the
//! model's real window, and
//! `docs/research/agent-context-memory-separation-2026-07-20.md`'s
//! provider-path check established that Horizon does not need an external
//! model-metadata catalog (opencode's models.dev, crush's catwalk) for this:
//! synthetic.new's `/models` already returns both numbers per model.
//! Standard OpenAI `/models` does not, so this reads them **when present**
//! and reports nothing at all when absent.
//!
//! "Nothing at all" is the whole failure model. Every failure mode -- no API
//! key, an unreachable endpoint, a non-JSON body, a model that isn't listed,
//! a listing without the two fields -- resolves to `None`, and a `None`
//! window means Tier 1 clearing **never fires** (crush's `cw == 0`
//! protection, `internal/agent/agent.go`). There is deliberately no
//! conservative fallback window: guessing one would mean clearing history on
//! the strength of a number Horizon made up.
//!
//! One lookup per process per `(base_url, model)`, negative results cached
//! too -- a provider that doesn't publish limits must not be re-asked once
//! per session for the life of the daemon.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Bounds the whole `/models` lookup. Session start waits on this once per
/// process, so it is short: an unresponsive endpoint costs a session a few
/// seconds of clearing-disabled startup, never a hang.
const MODELS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Rig's own default when `RigAgentConfig::base_url` is `None` (see
/// `providers::rig::completion::openai_completions_client`). Named here so
/// the cache key and the request URL agree on what "no base URL" resolves
/// to.
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// What a provider declares about one model's context budget. Both numbers
/// are as-reported; the effective window is derived by the caller
/// ([`Self::effective_window_tokens`]) because it depends on what Horizon
/// actually sends as `max_tokens`, not on what the model could accept.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ModelLimits {
    pub(super) context_length: u64,
    /// The model's declared maximum output length. Recorded for
    /// completeness (it is what made the 2026-07-27 `max_tokens` audit
    /// legible) but deliberately *not* used to derive the effective window:
    /// the window has to be reduced by the output budget Horizon really
    /// sends, which is `RigAgentConfig::max_output_tokens`.
    pub(super) max_output_length: Option<u64>,
}

impl ModelLimits {
    /// The window a request's input may actually occupy: the declared
    /// context length minus the output budget Horizon reserves on every
    /// request. `None` when the subtraction leaves nothing (a declared
    /// context smaller than the output budget is nonsense Horizon must not
    /// build a percentage on).
    pub(super) fn effective_window_tokens(self, max_output_tokens: u64) -> Option<u64> {
        self.context_length
            .checked_sub(max_output_tokens)
            .filter(|window| *window > 0)
    }
}

/// Keyed by `(base_url, model)`; the value is the *answer*, so a `None`
/// (this provider declares no limits) is cached exactly like a hit.
type LimitsCache = Mutex<HashMap<(String, String), Option<ModelLimits>>>;

/// Resolves this process's cached limits for `(base_url, model)`, fetching
/// them on the first call and reusing the answer -- including a negative one
/// -- for every later session.
pub(super) async fn model_limits(base_url: Option<&str>, model: &str) -> Option<ModelLimits> {
    let base = base_url.unwrap_or(DEFAULT_OPENAI_BASE_URL).to_string();
    let key = (base.clone(), model.to_string());

    if let Some(cached) = cache().lock().ok().and_then(|map| map.get(&key).copied()) {
        return cached;
    }

    let fetched = fetch_model_limits(&base, model).await;
    if let Ok(mut map) = cache().lock() {
        map.insert(key, fetched);
    }
    fetched
}

fn cache() -> &'static LimitsCache {
    static CACHE: OnceLock<LimitsCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One plain authenticated GET. Every error path returns `None` -- see the
/// module doc: a failed lookup disables clearing, it never degrades into a
/// guessed window.
async fn fetch_model_limits(base_url: &str, model: &str) -> Option<ModelLimits> {
    let api_key = std::env::var(crate::config::OPENAI_API_KEY_VAR).ok()?;
    let client = reqwest::Client::builder()
        .timeout(MODELS_REQUEST_TIMEOUT)
        .build()
        .ok()?;
    let response = client
        .get(models_url(base_url))
        .bearer_auth(api_key)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().await.ok()?;
    let body: serde_json::Value = serde_json::from_str(&body).ok()?;
    parse_model_limits(&body, model)
}

/// `{base_url}/models`, tolerating a trailing slash on the configured base.
pub(super) fn models_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

/// Reads `model`'s entry out of an OpenAI-shaped `/models` listing.
///
/// `context_length` is required (it *is* the window); `max_output_length` is
/// optional. A listing whose entry carries neither -- standard OpenAI --
/// yields `None`, which is the "limits unavailable" signal, not an error.
pub(super) fn parse_model_limits(body: &serde_json::Value, model: &str) -> Option<ModelLimits> {
    let entry = body
        .get("data")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("id").and_then(serde_json::Value::as_str) == Some(model))?;
    Some(ModelLimits {
        context_length: entry.get("context_length")?.as_u64()?,
        max_output_length: entry
            .get("max_output_length")
            .and_then(serde_json::Value::as_u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_listing() -> serde_json::Value {
        serde_json::json!({
            "object": "list",
            "data": [
                {"id": "hf:MiniMaxAI/MiniMax-M3", "context_length": 131072},
                {
                    "id": "hf:moonshotai/Kimi-K2.7-Code",
                    "context_length": 262144,
                    "max_output_length": 65536
                },
            ]
        })
    }

    #[test]
    fn parses_the_named_models_declared_limits() {
        let limits = parse_model_limits(&synthetic_listing(), "hf:moonshotai/Kimi-K2.7-Code")
            .expect("the listed model declares a context length");
        assert_eq!(limits.context_length, 262_144);
        assert_eq!(limits.max_output_length, Some(65_536));
    }

    #[test]
    fn max_output_length_is_optional() {
        let limits = parse_model_limits(&synthetic_listing(), "hf:MiniMaxAI/MiniMax-M3")
            .expect("an entry without max_output_length still declares a window");
        assert_eq!(limits.context_length, 131_072);
        assert_eq!(limits.max_output_length, None);
    }

    #[test]
    fn a_standard_openai_listing_declares_no_limits() {
        // What `GET https://api.openai.com/v1/models` actually returns: no
        // context length anywhere, so clearing stays off for that provider.
        let body = serde_json::json!({
            "object": "list",
            "data": [{"id": "gpt-4o-mini", "object": "model", "owned_by": "openai"}]
        });
        assert_eq!(parse_model_limits(&body, "gpt-4o-mini"), None);
    }

    #[test]
    fn an_unlisted_model_declares_no_limits() {
        assert_eq!(
            parse_model_limits(&synthetic_listing(), "hf:other/Model"),
            None
        );
    }

    #[test]
    fn a_body_without_a_data_array_declares_no_limits() {
        assert_eq!(
            parse_model_limits(&serde_json::json!({"error": "unauthorized"}), "any"),
            None
        );
    }

    #[test]
    fn effective_window_subtracts_the_output_budget_horizon_actually_sends() {
        let limits = ModelLimits {
            context_length: 262_144,
            max_output_length: Some(65_536),
        };
        assert_eq!(
            limits.effective_window_tokens(crate::config::DEFAULT_AGENT_MAX_OUTPUT_TOKENS),
            Some(262_144 - 32_768)
        );
    }

    #[test]
    fn effective_window_is_none_when_the_output_budget_swallows_the_context() {
        let limits = ModelLimits {
            context_length: 8_192,
            max_output_length: None,
        };
        assert_eq!(limits.effective_window_tokens(32_768), None);
        assert_eq!(limits.effective_window_tokens(8_192), None);
    }

    #[test]
    fn models_url_tolerates_a_trailing_slash() {
        assert_eq!(
            models_url("https://api.synthetic.new/openai/v1"),
            "https://api.synthetic.new/openai/v1/models"
        );
        assert_eq!(
            models_url("https://api.synthetic.new/openai/v1/"),
            "https://api.synthetic.new/openai/v1/models"
        );
    }
}
