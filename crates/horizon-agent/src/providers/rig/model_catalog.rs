//! The model catalog (models.dev, the catalog OpenCode lists models from)
//! as the window source for a provider whose `/models` listing declares
//! none -- `docs/agent-compaction-design.md`'s window sources, decided
//! 2026-10-20 (owner): a provider that speaks only the OpenAI-compatible
//! surface has no standard way to say its context length, and reading a
//! provider's own API for it (Ollama's `/api/show`) would be the first
//! provider-specific branch in this crate.
//!
//! One document, `https://models.dev/api.json`, keyed by catalog provider
//! id; each provider carries the base URL it serves (`api`) and its models
//! with `limit.{context,output}`. A Horizon `[[providers]]` entry is matched
//! to a catalog provider by that base URL, never by model id alone (the
//! same id appears under several providers with different windows), and
//! the model by exact id inside it. No match means no window, as before.
//!
//! Fetched once per process the first time a window is needed, and the
//! body written to `$XDG_CACHE_HOME/horizon/models-dev-api.json` (falling
//! back to `~/.cache`); when the fetch fails the cached copy serves, and
//! with neither the catalog is simply absent for the life of the process.
//! `HORIZON_MODEL_CATALOG_URL` and `HORIZON_MODEL_CATALOG_CACHE` relocate
//! the two, for tests and for a machine that mirrors the file.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::Deserialize;

use super::model_limits::ModelLimits;

/// Environment variable naming where the catalog is fetched from.
pub(super) const CATALOG_URL_VAR: &str = "HORIZON_MODEL_CATALOG_URL";
/// Environment variable naming the cached copy's path.
pub(super) const CATALOG_CACHE_VAR: &str = "HORIZON_MODEL_CATALOG_CACHE";
const DEFAULT_CATALOG_URL: &str = "https://models.dev/api.json";
const CACHE_FILE: &str = "models-dev-api.json";
const XDG_CACHE_HOME_VAR: &str = "XDG_CACHE_HOME";
const HOME_VAR: &str = "HOME";
/// The document is a few megabytes; a first session on a slow link waits
/// on this once per process, then never again.
const CATALOG_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The catalog reduced to what a window lookup reads: per provider base
/// URL (normalized), per model id, the declared limits.
#[derive(Debug, Default)]
pub(super) struct Catalog {
    providers: HashMap<String, HashMap<String, ModelLimits>>,
}

#[derive(Deserialize)]
struct RawProvider {
    api: Option<String>,
    #[serde(default)]
    models: HashMap<String, RawModel>,
}

#[derive(Deserialize)]
struct RawModel {
    limit: Option<RawLimit>,
}

#[derive(Deserialize)]
struct RawLimit {
    context: Option<u64>,
    output: Option<u64>,
}

impl Catalog {
    /// Reads the catalog document. A provider without a base URL, and a
    /// model without a context limit, contribute nothing; anything else
    /// malformed fails the whole parse, which reads as "no catalog".
    pub(super) fn parse(body: &str) -> Option<Self> {
        let raw: HashMap<String, RawProvider> = serde_json::from_str(body).ok()?;
        let mut providers: HashMap<String, HashMap<String, ModelLimits>> = HashMap::new();
        for provider in raw.into_values() {
            let Some(api) = provider.api else {
                continue;
            };
            let models = providers.entry(normalize_base_url(&api)).or_default();
            for (id, model) in provider.models {
                let Some(limit) = model.limit else {
                    continue;
                };
                let Some(context_length) = limit.context else {
                    continue;
                };
                models.insert(
                    id,
                    ModelLimits {
                        context_length,
                        max_output_length: limit.output,
                    },
                );
            }
        }
        Some(Self { providers })
    }

    /// The limits the catalog declares for `model` at the provider serving
    /// `base_url`, if it knows both.
    pub(super) fn limits(&self, base_url: &str, model: &str) -> Option<ModelLimits> {
        self.providers
            .get(&normalize_base_url(base_url))?
            .get(model)
            .copied()
    }
}

/// `https://ollama.com/v1/` and `https://ollama.com/v1` are one provider.
fn normalize_base_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// The catalog's limits for `model` at `base_url`, loading the catalog on
/// the first call of the process.
pub(super) async fn catalog_limits(base_url: &str, model: &str) -> Option<ModelLimits> {
    catalog().await?.limits(base_url, model)
}

/// The process's catalog, loaded on first use. A load that found nothing
/// is kept too: the catalog is per daemon start, not per session.
async fn catalog() -> Option<Arc<Catalog>> {
    static CATALOG: OnceLock<Option<Arc<Catalog>>> = OnceLock::new();
    if let Some(loaded) = CATALOG.get() {
        return loaded.clone();
    }
    let loaded = load_catalog().await.map(Arc::new);
    CATALOG.get_or_init(|| loaded).clone()
}

/// Fetches the document and refreshes the cached copy; on any failure
/// reads the cached copy instead. Says on stderr which one served, since a
/// stale cached copy is the one state the owner might want to know about.
async fn load_catalog() -> Option<Catalog> {
    let url = std::env::var(CATALOG_URL_VAR)
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_CATALOG_URL.to_string());
    let cache = cache_path();
    if let Some(body) = fetch(&url).await {
        if let Some(catalog) = parse_off_thread(body.clone()).await {
            if let Some(path) = &cache {
                write_cache(path.clone(), body).await;
            }
            return Some(catalog);
        }
    }
    let path = cache?;
    let body = read_cache(path.clone()).await?;
    let catalog = parse_off_thread(body).await?;
    eprintln!(
        "horizon-agent: model catalog fetch from {url} failed; using the cached copy at {}",
        path.display()
    );
    Some(catalog)
}

async fn fetch(url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(CATALOG_REQUEST_TIMEOUT)
        .build()
        .ok()?;
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.text().await.ok()
}

/// The document is megabytes of JSON: parsed off the session's thread.
async fn parse_off_thread(body: String) -> Option<Catalog> {
    tokio::task::spawn_blocking(move || Catalog::parse(&body))
        .await
        .ok()
        .flatten()
}

async fn read_cache(path: PathBuf) -> Option<String> {
    tokio::task::spawn_blocking(move || std::fs::read_to_string(path).ok())
        .await
        .ok()
        .flatten()
}

/// Writes the copy whole-or-not: a temporary file beside the target,
/// renamed over it, so a crash mid-write never leaves a torn cache.
async fn write_cache(path: PathBuf, body: String) {
    let _ = tokio::task::spawn_blocking(move || {
        let Some(dir) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, body).is_ok() && std::fs::rename(&temporary, &path).is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
    })
    .await;
}

/// `HORIZON_MODEL_CATALOG_CACHE`, else `$XDG_CACHE_HOME/horizon/<file>`,
/// else `~/.cache/horizon/<file>`; `None` with no home at all.
fn cache_path() -> Option<PathBuf> {
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    if let Some(path) = non_empty(std::env::var(CATALOG_CACHE_VAR).ok()) {
        return Some(PathBuf::from(path));
    }
    let cache_home = match non_empty(std::env::var(XDG_CACHE_HOME_VAR).ok()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(non_empty(std::env::var(HOME_VAR).ok())?).join(".cache"),
    };
    Some(cache_home.join("horizon").join(CACHE_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two catalog providers in the document's shape, one of them serving
    /// Ollama Cloud's base URL.
    fn document() -> String {
        serde_json::json!({
            "ollama-cloud": {
                "id": "ollama-cloud",
                "name": "Ollama Cloud",
                "api": "https://ollama.com/v1",
                "models": {
                    "deepseek-v4.1-flash": {"limit": {"context": 1048576, "output": 384000}},
                    "glm-5.3-flash": {"limit": {"context": 1000000, "output": 131072}},
                    "no-limit": {"name": "no limit declared"}
                }
            },
            "other": {
                "id": "other",
                "api": "https://other.invalid/v1",
                "models": {
                    "deepseek-v4.1-flash": {"limit": {"context": 65536, "output": 8192}}
                }
            },
            "no-api": {
                "id": "no-api",
                "models": {"x": {"limit": {"context": 1}}}
            }
        })
        .to_string()
    }

    #[test]
    fn limits_are_read_by_provider_base_url_then_model_id() {
        let catalog = Catalog::parse(&document()).expect("parses");
        assert_eq!(
            catalog.limits("https://ollama.com/v1", "deepseek-v4.1-flash"),
            Some(ModelLimits {
                context_length: 1_048_576,
                max_output_length: Some(384_000),
            })
        );
        // The same id under another provider is another window.
        assert_eq!(
            catalog
                .limits("https://other.invalid/v1", "deepseek-v4.1-flash")
                .map(|limits| limits.context_length),
            Some(65_536)
        );
        // A trailing slash or capitals in the configured base URL still match.
        assert_eq!(
            catalog
                .limits("https://Ollama.com/v1/", "glm-5.3-flash")
                .map(|limits| limits.context_length),
            Some(1_000_000)
        );
        // Unknown provider, unknown model, and a model without a limit.
        assert_eq!(
            catalog.limits("https://nowhere.invalid/v1", "glm-5.3-flash"),
            None
        );
        assert_eq!(catalog.limits("https://ollama.com/v1", "nemo"), None);
        assert_eq!(catalog.limits("https://ollama.com/v1", "no-limit"), None);
        assert!(
            !catalog.providers.contains_key(""),
            "a provider without api is skipped"
        );
    }

    #[test]
    fn a_malformed_document_is_no_catalog() {
        assert!(Catalog::parse("not json").is_none());
        assert!(Catalog::parse("[]").is_none());
    }

    #[test]
    fn the_cache_path_follows_the_override_then_the_cache_home() {
        std::env::set_var(CATALOG_CACHE_VAR, "/elsewhere/catalog.json");
        assert_eq!(cache_path(), Some(PathBuf::from("/elsewhere/catalog.json")));
        std::env::remove_var(CATALOG_CACHE_VAR);
        std::env::set_var(XDG_CACHE_HOME_VAR, "/cache-home");
        assert_eq!(
            cache_path(),
            Some(PathBuf::from("/cache-home/horizon/models-dev-api.json"))
        );
        std::env::remove_var(XDG_CACHE_HOME_VAR);
        std::env::set_var(HOME_VAR, "/home/someone");
        assert_eq!(
            cache_path(),
            Some(PathBuf::from(
                "/home/someone/.cache/horizon/models-dev-api.json"
            ))
        );
    }

    /// Not a check: a live fetch of the real catalog, for a reader who
    /// wants to see what it says about the provider in use. Run by hand
    /// with `--run-ignored all --no-capture`; the copy goes to a
    /// temporary path, not the real cache.
    #[tokio::test]
    #[ignore = "fetches the live catalog over the network"]
    async fn live_catalog_knows_ollama_cloud() {
        let dir = std::env::temp_dir().join(format!("horizon-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(CATALOG_CACHE_VAR, dir.join("catalog.json"));
        std::env::remove_var(CATALOG_URL_VAR);
        let limits = catalog_limits("https://ollama.com/v1", "deepseek-v4.1-flash").await;
        println!("live catalog, ollama-cloud / deepseek-v4.1-flash: {limits:?}");
        println!(
            "cached copy: {} bytes",
            std::fs::metadata(dir.join("catalog.json"))
                .map(|m| m.len())
                .unwrap_or(0)
        );
        assert!(limits.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The fetch fails (nothing listens on the port), the cached copy
    /// serves. Each nextest test is its own process, so the process-wide
    /// catalog this loads is this test's alone.
    #[tokio::test]
    async fn the_cached_copy_serves_when_the_fetch_fails() {
        let dir = std::env::temp_dir().join(format!("horizon-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("catalog.json");
        std::fs::write(&cache, document()).unwrap();
        std::env::set_var(CATALOG_URL_VAR, "http://127.0.0.1:1/api.json");
        std::env::set_var(CATALOG_CACHE_VAR, &cache);

        assert_eq!(
            catalog_limits("https://ollama.com/v1", "glm-5.3-flash")
                .await
                .map(|limits| limits.context_length),
            Some(1_000_000)
        );
        assert_eq!(catalog_limits("https://ollama.com/v1", "nemo").await, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
