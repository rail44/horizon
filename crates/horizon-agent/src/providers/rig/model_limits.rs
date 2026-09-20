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
//! window means Tier 1 clearing **never fires**. There is no fallback
//! window: an unknown window never clears history.
//!
//! The bearer token is read from the environment variable **named** by
//! [`RigAgentConfig::api_key_env`], the same variable
//! `providers::rig::completion` reads when it builds the turn's client.
//! The config carries that name, never a value; an unset variable resolves
//! to `None` like any other failure.
//!
//! Only [`ProviderKind::OpenAiCompatible`] entries are looked up: the
//! `{base_url}/models` path, the bearer header, and the
//! `context_length`/`max_output_length` fields are the openai-compatible
//! listing's. [`ProviderKind::Anthropic`] returns `None` without a request
//! -- rig's anthropic client authenticates with `x-api-key` plus
//! `anthropic-version`, and an anthropic entry with no `base_url` resolves
//! to [`DEFAULT_OPENAI_BASE_URL`] below, so a request here would carry that
//! entry's key to another vendor's endpoint.
//!
//! One lookup per process per `(base_url, api_key_env, model)`, negative
//! results cached too -- a provider that doesn't publish limits must not be
//! re-asked once per session for the life of the daemon. The key variable's
//! name is part of the cache key because two entries can share a base URL
//! and a model while authenticating differently: one entry's `401` is not
//! the other's answer.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::config::{ProviderKind, RigAgentConfig};

/// Bounds the whole `/models` lookup. Session start waits on this once per
/// process, so it is short: an unresponsive endpoint costs a session a few
/// seconds of clearing-disabled startup, never a hang.
const MODELS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Rig's own default when `RigAgentConfig::base_url` is `None` (see
/// `providers::rig::completion::completion_client`). Named here so
/// the cache key and the request URL agree on what "no base URL" resolves
/// to. The picker's discovery path passes a concrete URL (the kind's own
/// default is resolved by the caller), so only [`model_limits`] reads this.
const DEFAULT_OPENAI_BASE_URL: &str = crate::config::DEFAULT_OPENAI_BASE_URL;

/// What a provider declares about one model's context budget. Both numbers
/// are as-reported; the effective window is derived by the caller
/// ([`Self::effective_window_tokens`]) because it depends on what Horizon
/// actually sends as `max_tokens`, not on what the model could accept.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ModelLimits {
    pub(super) context_length: u64,
    /// The model's declared maximum output length. Recorded for
    /// completeness; not used to derive the effective window, which is
    /// reduced by the output budget Horizon sends
    /// (`RigAgentConfig::max_output_tokens`).
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

/// Keyed by `(base_url, api_key_env, model)`; the value is the *answer*, so
/// a `None` (this provider declares no limits) is cached exactly like a hit.
type LimitsCache = Mutex<HashMap<(String, String, String), Option<ModelLimits>>>;

/// Keyed by base URL; the value is the ids a *successful* listing returned.
/// Failures (transport, non-2xx, no ids) are deliberately not cached — a
/// provider that was briefly unreachable must still answer a later picker
/// open.
type ListingCache = Mutex<HashMap<String, Vec<String>>>;

/// Resolves this process's cached limits for this session's provider entry,
/// fetching them on the first call and reusing the answer -- including a
/// negative one -- for every later session on the same
/// `(base_url, api_key_env, model)`.
pub(super) async fn model_limits(config: &RigAgentConfig) -> Option<ModelLimits> {
    match config.kind {
        ProviderKind::OpenAiCompatible => {}
        // No OpenAI-shaped lookup for another wire's entry -- module doc.
        ProviderKind::Anthropic => return None,
    }

    let base = config
        .base_url
        .as_deref()
        .unwrap_or(DEFAULT_OPENAI_BASE_URL)
        .to_string();
    let key = (
        base.clone(),
        config.api_key_env.clone(),
        config.model.clone(),
    );

    if let Some(cached) = cache().lock().ok().and_then(|map| map.get(&key).copied()) {
        return cached;
    }

    let fetched = fetch_model_limits(&base, &config.api_key_env, &config.model).await;
    if let Ok(mut map) = cache().lock() {
        map.insert(key, fetched);
    }
    fetched
}

fn cache() -> &'static LimitsCache {
    static CACHE: OnceLock<LimitsCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One plain authenticated GET, the bearer token read from the variable
/// `api_key_env` names. Every error path returns `None`, an unset key
/// variable included.
async fn fetch_model_limits(base_url: &str, api_key_env: &str, model: &str) -> Option<ModelLimits> {
    let api_key = std::env::var(api_key_env).ok()?;
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

/// The provider's live model-id listing, for the picker's discovery: the
/// same `GET {base_url}/models` request as [`model_limits`], but it keeps
/// every `data[].id` rather than looking up one model's limits.
///
/// `base_url` is already resolved to a concrete endpoint by the caller (the
/// kind's env var > the entry's `base_url` > the kind's own default), so an
/// Anthropic entry reaches `api.anthropic.com` rather than rig's OpenAI
/// default. A successful listing is cached process-lifetime, keyed by base
/// URL (discovery rarely changes mid-run, and a picker reopen must not
/// re-ask); a failure is not cached. An empty key is treated as "no key" and
/// returns nothing without a request.
pub(crate) async fn list_model_ids(base_url: &str, api_key: &str) -> Vec<String> {
    if api_key.is_empty() {
        return Vec::new();
    }
    let base = base_url.to_string();
    if let Some(cached) = cache_listings()
        .lock()
        .ok()
        .and_then(|map| map.get(&base).cloned())
    {
        return cached;
    }
    let ids = fetch_model_ids(&base, api_key).await;
    if !ids.is_empty() {
        if let Ok(mut map) = cache_listings().lock() {
            map.insert(base, ids.clone());
        }
    }
    ids
}

fn cache_listings() -> &'static ListingCache {
    static CACHE: OnceLock<ListingCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One plain authenticated GET, keeping only the ids. Every error path
/// returns an empty list.
async fn fetch_model_ids(base_url: &str, api_key: &str) -> Vec<String> {
    let client = match reqwest::Client::builder()
        .timeout(MODELS_REQUEST_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => return Vec::new(),
    };
    let response = match client
        .get(models_url(base_url))
        .bearer_auth(api_key)
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return Vec::new(),
    };
    if !response.status().is_success() {
        return Vec::new();
    }
    let body = match response.text().await {
        Ok(body) => body,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(body) => parse_model_ids(&body),
        Err(_) => Vec::new(),
    }
}

/// Every `data[].id` of a `/models` listing, in listing order. Tolerant by
/// design: both the OpenAI-compatible and the Anthropic listing put the ids
/// under `data`, and anything malformed simply contributes no ids.
pub(super) fn parse_model_ids(body: &serde_json::Value) -> Vec<String> {
    body.get("data")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// One recorded request: its first line and the `authorization` header
    /// value, which is what the lookup's auth source is visible as.
    type Recorded = (String, Option<String>);

    /// A loopback `/models` endpoint. It answers [`synthetic_listing`] to a
    /// request bearing `expected_key` and `401` to anything else -- the
    /// shape a provider takes when handed another entry's key -- and records
    /// every request it accepts.
    struct ModelsMock {
        base_url: String,
        requests: Arc<Mutex<Vec<Recorded>>>,
    }

    impl ModelsMock {
        async fn start(expected_key: &'static str) -> Self {
            // The lookup builds its own client, so a proxy configured in the
            // environment would intercept the loopback request and make the
            // test depend on the machine it runs on.
            for var in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
                std::env::remove_var(var);
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorder = Arc::clone(&requests);
            tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let head = read_request_head(&mut socket).await;
                    // `lines` leaves CRLF's `\r` on every line.
                    let mut lines = head.lines().map(str::trim_end);
                    let request_line = lines.next().unwrap_or_default().to_string();
                    let authorization = lines
                        .find_map(|line| line.strip_prefix("authorization: "))
                        .map(str::to_string);
                    // The head is read lowercased, so is the expectation.
                    let authorized =
                        authorization.as_deref() == Some(&format!("bearer {expected_key}"));
                    recorder.lock().unwrap().push((request_line, authorization));
                    let body = if authorized {
                        serde_json::to_vec(&synthetic_listing()).unwrap()
                    } else {
                        br#"{"error":"unauthorized"}"#.to_vec()
                    };
                    let status = if authorized {
                        "200 OK"
                    } else {
                        "401 Unauthorized"
                    };
                    let response_head = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: \
                         {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(response_head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                }
            });
            Self { base_url, requests }
        }

        fn recorded(&self) -> Vec<Recorded> {
            self.requests.lock().unwrap().clone()
        }
    }

    /// Reads a request up to the blank line, lowercased so header lookups
    /// don't depend on the client's casing.
    async fn read_request_head(socket: &mut tokio::net::TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            match socket.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            }
        }
        String::from_utf8_lossy(&buffer).to_ascii_lowercase()
    }

    /// A session config on `base_url`, listing [`synthetic_listing`]'s
    /// second model, whose key lives in `api_key_env`.
    fn config_on(base_url: &str, api_key_env: &str) -> RigAgentConfig {
        RigAgentConfig {
            api_key_present: true,
            api_key_env: api_key_env.to_string(),
            model: "hf:moonshotai/Kimi-K2.7-Code".to_string(),
            base_url: Some(base_url.to_string()),
            ..RigAgentConfig::default()
        }
    }

    #[tokio::test]
    async fn the_entrys_own_key_variable_authenticates_the_lookup() {
        // OPENAI_API_KEY holds a key the mock rejects, so a lookup reading
        // it instead of the entry's own variable comes back `None`.
        std::env::set_var(crate::config::OPENAI_API_KEY_VAR, "not-this-entrys-key");
        std::env::set_var("HORIZON_TEST_MODEL_LIMITS_KEY_A", "entry-key");
        let mock = ModelsMock::start("entry-key").await;

        let limits = model_limits(&config_on(
            &mock.base_url,
            "HORIZON_TEST_MODEL_LIMITS_KEY_A",
        ))
        .await
        .expect("the entry's own key authenticates the listing");

        assert_eq!(limits.context_length, 262_144);
        assert_eq!(
            mock.recorded(),
            vec![(
                "get /models http/1.1".to_string(),
                Some("bearer entry-key".to_string())
            )]
        );
    }

    #[tokio::test]
    async fn entries_differing_only_by_key_variable_do_not_share_an_answer() {
        // Same base URL and model, different key variables: the rejected
        // entry's `None` must not become the other entry's cached answer.
        std::env::set_var("HORIZON_TEST_MODEL_LIMITS_KEY_A", "stale-key");
        std::env::set_var("HORIZON_TEST_MODEL_LIMITS_KEY_B", "live-key");
        let mock = ModelsMock::start("live-key").await;

        assert_eq!(
            model_limits(&config_on(
                &mock.base_url,
                "HORIZON_TEST_MODEL_LIMITS_KEY_A"
            ))
            .await,
            None,
            "the rejected key declares no limits"
        );
        let limits = model_limits(&config_on(
            &mock.base_url,
            "HORIZON_TEST_MODEL_LIMITS_KEY_B",
        ))
        .await
        .expect("the second entry is asked with its own key, not served the first's None");

        assert_eq!(limits.context_length, 262_144);
        assert_eq!(
            mock.recorded().len(),
            2,
            "both entries reached the provider"
        );
    }

    #[tokio::test]
    async fn the_cached_answer_is_reused_for_the_same_entry() {
        std::env::set_var("HORIZON_TEST_MODEL_LIMITS_KEY_A", "entry-key");
        let mock = ModelsMock::start("entry-key").await;
        let config = config_on(&mock.base_url, "HORIZON_TEST_MODEL_LIMITS_KEY_A");

        let first = model_limits(&config)
            .await
            .expect("the entry's key authenticates the first lookup");
        assert_eq!(model_limits(&config).await, Some(first));
        assert_eq!(
            mock.recorded().len(),
            1,
            "one lookup per process per (base_url, api_key_env, model)"
        );
    }

    #[tokio::test]
    async fn the_default_single_provider_setup_still_reads_openai_api_key() {
        assert_eq!(
            RigAgentConfig::default().api_key_env,
            crate::config::OPENAI_API_KEY_VAR
        );
        std::env::set_var(crate::config::OPENAI_API_KEY_VAR, "default-key");
        let mock = ModelsMock::start("default-key").await;

        let limits = model_limits(&config_on(
            &mock.base_url,
            crate::config::OPENAI_API_KEY_VAR,
        ))
        .await
        .expect("the default entry is discovered exactly as before");

        assert_eq!(limits.context_length, 262_144);
        assert_eq!(
            mock.recorded(),
            vec![(
                "get /models http/1.1".to_string(),
                Some("bearer default-key".to_string())
            )]
        );
    }

    #[tokio::test]
    async fn an_anthropic_entry_is_never_asked_for_an_openai_listing() {
        // The entry's key must not reach an OpenAI-shaped endpoint.
        std::env::set_var("HORIZON_TEST_MODEL_LIMITS_KEY_B", "live-key");
        let mock = ModelsMock::start("live-key").await;
        let anthropic = RigAgentConfig {
            kind: ProviderKind::Anthropic,
            ..config_on(&mock.base_url, "HORIZON_TEST_MODEL_LIMITS_KEY_B")
        };

        assert_eq!(model_limits(&anthropic).await, None);
        // An openai-compatible entry on the same endpoint proves the mock
        // was reachable all along, and counts the requests it really got.
        assert!(model_limits(&config_on(
            &mock.base_url,
            "HORIZON_TEST_MODEL_LIMITS_KEY_B"
        ))
        .await
        .is_some());
        assert_eq!(
            mock.recorded().len(),
            1,
            "only the openai-compatible entry issued a request"
        );
    }

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

    #[test]
    fn parses_every_id_of_a_listing_in_order() {
        assert_eq!(
            parse_model_ids(&synthetic_listing()),
            vec![
                "hf:MiniMaxAI/MiniMax-M3".to_string(),
                "hf:moonshotai/Kimi-K2.7-Code".to_string(),
            ]
        );
    }

    #[test]
    fn parses_a_standard_openai_listing_too() {
        // No limits, but the ids are exactly what the picker wants.
        let body = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "gpt-4o-mini", "object": "model", "owned_by": "openai"},
                {"id": "gpt-4o", "object": "model", "owned_by": "openai"}
            ]
        });
        assert_eq!(
            parse_model_ids(&body),
            vec!["gpt-4o-mini".to_string(), "gpt-4o".to_string()]
        );
    }

    #[test]
    fn a_malformed_listing_yields_no_ids() {
        assert!(parse_model_ids(&serde_json::json!({"error": "unauthorized"})).is_empty());
        assert!(parse_model_ids(&serde_json::json!({"data": "nope"})).is_empty());
        assert!(parse_model_ids(&serde_json::json!({"data": [{"object": "model"}]})).is_empty());
    }
}
