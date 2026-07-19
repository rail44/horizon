//! Provider `/models` catalog query for axis A (model-derived history
//! budget) -- `docs/research/agent-context-memory-separation-2026-07-20.md`'s
//! "Decision (2026-07-20)". synthetic.new's `/models` response carries a
//! `context_length` and `max_output_length` per model entry; plain OpenAI's
//! `/models` does not, so a missing field degrades to the caller's
//! conservative built-in default (`config::DEFAULT_HISTORY_TOKEN_BUDGET`)
//! rather than erroring -- this module never fails outward, only returns
//! `None` when it can't resolve a window.

use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, OnceLock};

use crate::config::RigAgentConfig;

/// A model's served context/output window, as reported by an
/// OpenAI-compatible provider's `/models` endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ModelWindow {
    pub(super) context_length: usize,
    pub(super) max_output_length: usize,
}

/// Used when `config.base_url` is `None` -- mirrors rig's own default
/// (`providers::rig::completion::openai_completions_client`'s doc comment).
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Process-wide `(base_url, model) -> resolved window` cache: queried at
/// most once per distinct pair for this cache's lifetime, per the design
/// doc ("query `{base_url}/models` once, cached per process per
/// `(base_url, model)`"). `None` caches a *negative* result too (query
/// failed, model unlisted, or fields absent) so a provider that never
/// exposes these fields isn't re-queried on every session either.
pub(super) struct ModelWindowCache {
    entries: StdMutex<HashMap<(String, String), Option<ModelWindow>>>,
}

impl ModelWindowCache {
    pub(super) fn new() -> Self {
        Self {
            entries: StdMutex::new(HashMap::new()),
        }
    }

    /// Resolves `model`'s window at `base_url`, calling `fetch` at most once
    /// per distinct `(base_url, model)` pair for this cache's lifetime.
    /// Every other call -- including one for a different session that
    /// happens to share the same provider/model -- returns the already
    /// cached outcome without invoking `fetch` again.
    pub(super) async fn resolve<F, Fut>(
        &self,
        base_url: &str,
        model: &str,
        fetch: F,
    ) -> Option<ModelWindow>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Option<ModelWindow>>,
    {
        let key = (base_url.to_string(), model.to_string());
        if let Some(cached) = self.lock().get(&key).copied() {
            return cached;
        }
        let resolved = fetch().await;
        self.lock().insert(key, resolved);
        resolved
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Option<ModelWindow>>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn global_cache() -> &'static ModelWindowCache {
    static CACHE: OnceLock<ModelWindowCache> = OnceLock::new();
    CACHE.get_or_init(ModelWindowCache::new)
}

/// Resolves `config`'s served model window, consulting (and populating) the
/// process-wide cache first. `None` when the session isn't running the real
/// OpenAI path at all (`config.openai_enabled == false` -- no key to
/// authenticate a query with, and no real budget concern since no provider
/// request is ever sent for this session), the query fails, the model isn't
/// listed, or the listing omits `context_length`/`max_output_length`
/// (plain OpenAI's `/models` always takes this path).
pub(super) async fn resolve_model_window(config: &RigAgentConfig) -> Option<ModelWindow> {
    if !config.openai_enabled {
        return None;
    }
    let base_url = config
        .base_url
        .clone()
        .unwrap_or_else(|| DEFAULT_OPENAI_BASE_URL.to_string());
    let model = config.model.clone();
    global_cache()
        .resolve(&base_url, &model, || {
            fetch_model_window(base_url.clone(), model.clone())
        })
        .await
}

/// Total timeout for the one-shot `/models` query. This runs on the session
/// creation critical path (`spawn_rig_session` `block_on`s it before the
/// session loop starts), so a slow or unreachable provider must degrade to
/// the conservative built-in budget rather than hang session creation --
/// the whole graceful-fallback intent depends on this being bounded.
const MODEL_CATALOG_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn fetch_model_window(base_url: String, model: String) -> Option<ModelWindow> {
    let api_key = std::env::var(crate::config::OPENAI_API_KEY_VAR).ok()?;
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let body = reqwest::Client::new()
        .get(url)
        .bearer_auth(api_key)
        .timeout(MODEL_CATALOG_QUERY_TIMEOUT)
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    parse_model_window(&body, &model)
}

#[derive(serde::Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(serde::Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    context_length: Option<usize>,
    #[serde(default)]
    max_output_length: Option<usize>,
}

/// Pure JSON parsing, factored out of [`fetch_model_window`] so it's
/// testable offline with a literal response body instead of a real network
/// call. Returns `None` for anything that doesn't yield both fields for
/// `model`: an unparseable body, `model` absent from `data`, or either
/// field missing (plain OpenAI's `/models` omits both).
fn parse_model_window(body: &str, model: &str) -> Option<ModelWindow> {
    let parsed: ModelsResponse = serde_json::from_str(body).ok()?;
    let entry = parsed.data.into_iter().find(|entry| entry.id == model)?;
    Some(ModelWindow {
        context_length: entry.context_length?,
        max_output_length: entry.max_output_length?,
    })
}

/// Adjusts `config.history_token_budget` in place from the model's served
/// context window (axis A), falling back to whatever budget `config`
/// already carries (the conservative built-in default --
/// `RigAgentConfig::from_env_and_provider` always starts with
/// [`crate::config::DEFAULT_HISTORY_TOKEN_BUDGET`]) when the window can't
/// be resolved or derived from. Async because the model catalog is a
/// network query; called once per session, right after the session's
/// dedicated Tokio runtime is built and before the session loop starts --
/// see `spawn_rig_session`. The process-wide cache above means only the
/// first session for a given `(base_url, model)` pair actually performs the
/// network call; every later session (any provider/model pairing already
/// seen) hits the cache.
pub(super) async fn apply_model_derived_history_budget(
    mut config: RigAgentConfig,
) -> RigAgentConfig {
    if let Some(window) = resolve_model_window(&config).await {
        if let Some(budget) = crate::config::derive_history_token_budget(
            window.context_length,
            window.max_output_length,
        ) {
            config.history_token_budget = budget;
        }
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn parse_model_window_reads_context_and_output_length_for_the_named_model() {
        let body = r#"{
            "data": [
                {"id": "hf:moonshotai/Kimi-K2.7-Code", "context_length": 262144, "max_output_length": 65536},
                {"id": "other-model", "context_length": 8192, "max_output_length": 2048}
            ]
        }"#;

        let window = parse_model_window(body, "hf:moonshotai/Kimi-K2.7-Code")
            .expect("model is listed with both fields");

        assert_eq!(window.context_length, 262_144);
        assert_eq!(window.max_output_length, 65_536);
    }

    #[test]
    fn parse_model_window_is_none_when_the_model_is_not_listed() {
        let body = r#"{"data": [{"id": "some-other-model", "context_length": 8192, "max_output_length": 2048}]}"#;

        assert_eq!(parse_model_window(body, "not-listed"), None);
    }

    #[test]
    fn parse_model_window_is_none_when_fields_are_absent_like_vanilla_openai() {
        // Plain OpenAI's `/models` response shape: no context_length/
        // max_output_length at all.
        let body = r#"{"data": [{"id": "gpt-4o-mini", "object": "model", "owned_by": "openai"}]}"#;

        assert_eq!(parse_model_window(body, "gpt-4o-mini"), None);
    }

    #[test]
    fn parse_model_window_is_none_when_only_one_field_is_present() {
        let body = r#"{"data": [{"id": "partial", "context_length": 8192}]}"#;

        assert_eq!(parse_model_window(body, "partial"), None);
    }

    #[test]
    fn parse_model_window_is_none_for_unparseable_bodies() {
        assert_eq!(parse_model_window("not json", "any-model"), None);
    }

    #[tokio::test]
    async fn model_window_cache_calls_fetch_at_most_once_per_pair() {
        let cache = ModelWindowCache::new();
        let calls = AtomicUsize::new(0);
        let fetch = || {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Some(ModelWindow {
                context_length: 1_000,
                max_output_length: 100,
            }))
        };

        let first = cache
            .resolve("https://example.invalid", "model-a", fetch)
            .await;
        let second = cache
            .resolve("https://example.invalid", "model-a", fetch)
            .await;

        assert_eq!(first, second);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a cached pair must not be re-fetched"
        );
    }

    #[tokio::test]
    async fn model_window_cache_fetches_independently_per_distinct_pair() {
        let cache = ModelWindowCache::new();
        let calls = AtomicUsize::new(0);
        let fetch = || {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Some(ModelWindow {
                context_length: 1_000,
                max_output_length: 100,
            }))
        };

        cache
            .resolve("https://example.invalid", "model-a", fetch)
            .await;
        cache
            .resolve("https://example.invalid", "model-b", fetch)
            .await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "distinct (base_url, model) pairs must each be fetched"
        );
    }

    #[tokio::test]
    async fn model_window_cache_also_caches_a_negative_result() {
        let cache = ModelWindowCache::new();
        let calls = AtomicUsize::new(0);
        let fetch = || {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(None)
        };

        let first = cache
            .resolve("https://example.invalid", "model-a", fetch)
            .await;
        let second = cache
            .resolve("https://example.invalid", "model-a", fetch)
            .await;

        assert_eq!(first, None);
        assert_eq!(second, None);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a negative outcome (unresolvable window) must be cached too"
        );
    }

    #[tokio::test]
    async fn resolve_model_window_skips_the_query_entirely_when_openai_is_disabled() {
        let config = RigAgentConfig {
            openai_enabled: false,
            ..Default::default()
        };

        assert_eq!(resolve_model_window(&config).await, None);
    }
}
