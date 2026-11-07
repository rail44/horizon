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
//! The catalog writes `api` only for providers reached through a generic
//! SDK that needs the URL (`@ai-sdk/openai-compatible` and the like); a
//! provider with its own SDK package -- DeepInfra, Groq, OpenAI, Anthropic,
//! Mistral, xAI, ... -- has `api: null`, its URL living in that package's
//! code. Those are matched instead by the registrable domain of their
//! `doc` link against the configured base URL's (`api.deepinfra.com` and
//! `deepinfra.com/models` share `deepinfra.com`), only when no provider
//! serves the base URL exactly, and only when one such provider claims the
//! domain (Azure's two entries share `microsoft.com` and match nothing).
//! The registrable domain is approximated as the host's last two labels:
//! no `api: null` provider's `doc` sits under a two-label public suffix
//! such as `co.jp`, and the exact model id is still required. Decided
//! 2026-11-08 (owner), over matching the key variable's name against the
//! catalog's `env`, which an owner names freely, and over extracting each
//! SDK package's default URL, which is not reliably machine-readable.
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

/// Per model id, the declared limits.
type ModelTable = HashMap<String, ModelLimits>;

/// The catalog reduced to what a window lookup reads: per provider base
/// URL (normalized), per model id, the declared limits; and for providers
/// without a base URL, the same keyed by their `doc` link's registrable
/// domain, `None` where several providers claim one domain.
#[derive(Debug, Default)]
pub(super) struct Catalog {
    providers: HashMap<String, ModelTable>,
    by_doc_domain: HashMap<String, Option<ModelTable>>,
}

#[derive(Deserialize)]
struct RawProvider {
    api: Option<String>,
    doc: Option<String>,
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
}

impl Catalog {
    /// Reads the catalog document. A provider with neither a base URL nor a
    /// usable `doc` domain, and a model without a context limit, contribute
    /// nothing; anything else malformed fails the whole parse, which reads
    /// as "no catalog".
    pub(super) fn parse(body: &str) -> Option<Self> {
        let raw: HashMap<String, RawProvider> = serde_json::from_str(body).ok()?;
        let mut catalog = Self::default();
        for provider in raw.into_values() {
            let models = model_table(provider.models);
            if let Some(api) = provider.api {
                catalog
                    .providers
                    .entry(normalize_base_url(&api))
                    .or_default()
                    .extend(models);
            } else if let Some(domain) = provider.doc.as_deref().and_then(registrable_domain) {
                catalog
                    .by_doc_domain
                    .entry(domain)
                    .and_modify(|claimed| *claimed = None)
                    .or_insert(Some(models));
            }
        }
        Some(catalog)
    }

    /// The limits the catalog declares for `model` at the provider serving
    /// `base_url`, if it knows both: by exact base URL, else by the one
    /// URL-less provider whose `doc` shares the base URL's domain.
    pub(super) fn limits(&self, base_url: &str, model: &str) -> Option<ModelLimits> {
        let models = match self.providers.get(&normalize_base_url(base_url)) {
            Some(models) => models,
            None => self
                .by_doc_domain
                .get(&registrable_domain(base_url)?)?
                .as_ref()?,
        };
        models.get(model).copied()
    }
}

fn model_table(models: HashMap<String, RawModel>) -> ModelTable {
    models
        .into_iter()
        .filter_map(|(id, model)| {
            let context_length = model.limit?.context?;
            Some((id, ModelLimits { context_length }))
        })
        .collect()
}

/// `https://ollama.com/v1/` and `https://ollama.com/v1` are one provider.
fn normalize_base_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// The URL's host reduced to its last two labels (`api.deepinfra.com` →
/// `deepinfra.com`); `None` for an IP address, a single-label host such
/// as `localhost`, or an unparsable URL.
fn registrable_domain(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    let host = parsed.domain()?.trim_end_matches('.').to_ascii_lowercase();
    let mut labels = host.rsplit('.');
    let tld = labels.next().filter(|label| !label.is_empty())?;
    let name = labels.next().filter(|label| !label.is_empty())?;
    Some(format!("{name}.{tld}"))
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

    /// URL-less providers in the document's shape: one with its own SDK
    /// (DeepInfra), two sharing a `doc` domain (Azure's pair), and one
    /// URL-carrying provider on a domain a URL-less one also documents.
    fn url_less_document() -> String {
        serde_json::json!({
            "deepinfra": {
                "id": "deepinfra",
                "npm": "@ai-sdk/deepinfra",
                "doc": "https://deepinfra.com/models",
                "models": {
                    "deepseek-ai/DeepSeek-V4.1-Flash-1017": {"limit": {"context": 1048576}}
                }
            },
            "azure": {
                "id": "azure",
                "doc": "https://learn.microsoft.com/en-us/azure/models",
                "models": {"gpt-5": {"limit": {"context": 400000}}}
            },
            "azure-cognitive-services": {
                "id": "azure-cognitive-services",
                "doc": "https://learn.microsoft.com/en-us/azure/ai-services/models",
                "models": {"gpt-5": {"limit": {"context": 272000}}}
            },
            "groq": {
                "id": "groq",
                "doc": "https://console.groq.com/docs/models",
                "models": {"kimi": {"limit": {"context": 1000}}}
            },
            "groq-exact": {
                "id": "groq-exact",
                "api": "https://api.groq.com/openai/v1",
                "models": {"kimi": {"limit": {"context": 2000}}}
            }
        })
        .to_string()
    }

    #[test]
    fn a_url_less_provider_is_matched_by_its_doc_domain() {
        let catalog = Catalog::parse(&url_less_document()).expect("parses");
        assert_eq!(
            catalog
                .limits(
                    "https://api.deepinfra.com/v1/openai",
                    "deepseek-ai/DeepSeek-V4.1-Flash-1017"
                )
                .map(|limits| limits.context_length),
            Some(1_048_576)
        );
        // The model id still has to match exactly.
        assert_eq!(
            catalog.limits("https://api.deepinfra.com/v1/openai", "DeepSeek-V4.1"),
            None
        );
    }

    #[test]
    fn a_doc_domain_claimed_twice_matches_nothing() {
        let catalog = Catalog::parse(&url_less_document()).expect("parses");
        assert_eq!(
            catalog.limits("https://myres.openai.azure.microsoft.com/v1", "gpt-5"),
            None
        );
    }

    #[test]
    fn an_exact_base_url_wins_over_a_doc_domain() {
        let catalog = Catalog::parse(&url_less_document()).expect("parses");
        assert_eq!(
            catalog
                .limits("https://api.groq.com/openai/v1", "kimi")
                .map(|limits| limits.context_length),
            Some(2000)
        );
        // Another path on the same host has no exact provider: the domain
        // match answers.
        assert_eq!(
            catalog
                .limits("https://api.groq.com/v2", "kimi")
                .map(|limits| limits.context_length),
            Some(1000)
        );
    }

    #[test]
    fn the_registrable_domain_is_the_hosts_last_two_labels() {
        assert_eq!(
            registrable_domain("https://api.deepinfra.com/v1/openai").as_deref(),
            Some("deepinfra.com")
        );
        assert_eq!(
            registrable_domain("https://API.X.ai./v1").as_deref(),
            Some("x.ai")
        );
        assert_eq!(registrable_domain("http://127.0.0.1:8080/v1"), None);
        assert_eq!(registrable_domain("http://localhost:11434/v1"), None);
        assert_eq!(registrable_domain("not a url"), None);
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
