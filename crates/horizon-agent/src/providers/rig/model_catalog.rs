//! The model catalog: the facts a turn's requests are encoded with (which
//! generation options a model takes, and how its vendor's dialect spells
//! them) and a source of the context window Tier 1 clearing is measured
//! against (`docs/agent-compaction-design.md`).
//!
//! It is rig's own catalog (`rig_core::catalog::Catalog::builtin`:
//! models.dev, with its main providers reviewed by hand) with a newer copy
//! of models.dev (`https://models.dev/api.json`, the catalog OpenCode lists
//! models from) laid over it by rig's own rule, which keeps the facts rig
//! reviewed by hand. A model is found by the entry's vendor
//! ([`crate::config::Vendor`]) and its id, by rig's lookup rule: the id as
//! listed, or a dated snapshot of a listed id. A provider rig has no vendor
//! for (DeepInfra, reached as OpenAI's vendor with its own `base_url`) has
//! no models here; its own `/models` listing is what declares a window for
//! it (`super::model_limits`).
//!
//! The copy is fetched once per process the first time a session needs the
//! catalog, and the body written to `$XDG_CACHE_HOME/horizon/models-dev-api.json`
//! (falling back to `~/.cache`); when the fetch fails the cached copy
//! serves when it is newer than rig's own data, and with neither, rig's own
//! catalog serves alone. Until the first session has loaded it, rig's own
//! serves too ([`current`]). `HORIZON_MODEL_CATALOG_URL` and
//! `HORIZON_MODEL_CATALOG_CACHE` relocate the two, for tests and for a
//! machine that mirrors the file.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rig_core::catalog::Catalog;

use super::model_limits::ModelLimits;
use crate::config::{ReasoningLevel, Vendor};

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

/// The catalog the first session loaded ([`catalog`]).
static LOADED: OnceLock<Arc<Catalog>> = OnceLock::new();

/// rig's own catalog, shared rather than cloned per turn.
fn builtin() -> Arc<Catalog> {
    static BUILTIN: OnceLock<Arc<Catalog>> = OnceLock::new();
    BUILTIN
        .get_or_init(|| Arc::new(Catalog::builtin().clone()))
        .clone()
}

/// The catalog as loaded so far, without waiting: the loaded one once a
/// session has loaded it, rig's own until then. What a turn encodes with.
pub(super) fn current() -> Arc<Catalog> {
    LOADED.get().cloned().unwrap_or_else(builtin)
}

/// The process's catalog, loaded on first use. A load that found no copy is
/// kept too: the catalog is per daemon start, not per session.
pub(super) async fn catalog() -> Arc<Catalog> {
    if let Some(loaded) = LOADED.get() {
        return loaded.clone();
    }
    let loaded = Arc::new(load_catalog().await);
    LOADED.get_or_init(|| loaded).clone()
}

/// The context window `catalog` lists for `vendor`'s `model`.
fn window(catalog: &Catalog, vendor: Vendor, model: &str) -> Option<ModelLimits> {
    let context = catalog.get(vendor.id(), model)?.spec.context_window?;
    Some(ModelLimits {
        context_length: u64::from(context),
    })
}

/// The reasoning levels `vendor`'s `model` takes, by the catalog as loaded
/// so far ([`ReasoningLevel::choices`]).
pub(super) fn reasoning_choices(vendor: Vendor, model: &str) -> Vec<ReasoningLevel> {
    let catalog = current();
    ReasoningLevel::choices(catalog.get(vendor.id(), model).map(|found| found.spec))
}

/// The catalog's context window for `vendor`'s `model`, loading the
/// catalog on the first call of the process.
pub(super) async fn catalog_limits(vendor: Vendor, model: &str) -> Option<ModelLimits> {
    window(&*catalog().await, vendor, model)
}

/// rig's own catalog with the fetched copy laid over it; on a failed fetch,
/// with the cached copy when it is newer than rig's own data (an older one
/// would only hide what rig shipped). Says on stderr when the cached copy
/// serves, since a stale copy is the one state the owner might want to
/// know about.
async fn load_catalog() -> Catalog {
    let url = std::env::var(CATALOG_URL_VAR)
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_CATALOG_URL.to_string());
    let cache = cache_path();
    if let Some(body) = fetch(&url).await
        && let Some(fetched) = parse_off_thread(body.clone()).await
    {
        if let Some(path) = &cache {
            write_cache(path.clone(), body).await;
        }
        return Catalog::builtin().with_models_dev(&fetched);
    }
    if let Some(path) = cache
        && newer_than_builtin(&path)
        && let Some(body) = read_cache(path.clone()).await
        && let Some(fetched) = parse_off_thread(body).await
    {
        eprintln!(
            "horizon-agent: model catalog fetch from {url} failed; using the cached copy at {}",
            path.display()
        );
        return Catalog::builtin().with_models_dev(&fetched);
    }
    Catalog::builtin().clone()
}

/// Whether the copy at `path` was written after rig's own data was read
/// from models.dev.
fn newer_than_builtin(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|written| written > Catalog::generated_at())
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

/// The document is megabytes of JSON: read off the session's thread. A
/// provider rig has no vendor for is skipped by rig, not an error.
async fn parse_off_thread(body: String) -> Option<Catalog> {
    tokio::task::spawn_blocking(move || {
        Catalog::from_models_dev(&body)
            .ok()
            .map(|(catalog, _skipped)| catalog)
    })
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

    fn vendor(name: &str) -> Vendor {
        Vendor::resolve(name).unwrap()
    }

    /// A models.dev document in the shape rig reads: Ollama's section with
    /// one model rig's own data lists (a window changed) and one it does
    /// not, plus a provider rig has no vendor for.
    fn document() -> String {
        let row = |context: u64| {
            serde_json::json!({
                "name": "test row",
                "reasoning": true,
                "tool_call": true,
                "modalities": {"input": ["text"], "output": ["text"]},
                "limit": {"context": context, "output": 8192}
            })
        };
        serde_json::json!({
            "ollama": {"models": {
                "kimi-k2.6": row(300_000),
                "horizon-test-model": row(65_536)
            }},
            "deepinfra": {"models": {"deepseek-ai/DeepSeek-V4.1-Flash-1017": row(1_048_576)}}
        })
        .to_string()
    }

    #[test]
    fn windows_are_read_by_vendor_then_model_id() {
        let builtin = Catalog::builtin();
        // Ollama Cloud's models, which models.dev was first read for.
        assert_eq!(
            window(builtin, vendor("ollama"), "kimi-k2.6").map(|limits| limits.context_length),
            Some(262_144)
        );
        // The same id under another vendor is another model.
        assert_eq!(window(builtin, vendor("anthropic"), "kimi-k2.6"), None);
        assert_eq!(window(builtin, vendor("ollama"), "no-such-model"), None);
    }

    #[test]
    fn a_fetched_copy_is_laid_over_rigs_own_and_adds_its_models() {
        let (fetched, skipped) = Catalog::from_models_dev(&document()).unwrap();
        // A provider rig has no vendor for is skipped, not fatal.
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        let catalog = Catalog::builtin().with_models_dev(&fetched);
        assert_eq!(
            window(&catalog, vendor("ollama"), "kimi-k2.6").map(|limits| limits.context_length),
            Some(300_000)
        );
        assert_eq!(
            window(&catalog, vendor("ollama"), "horizon-test-model")
                .map(|limits| limits.context_length),
            Some(65_536)
        );
    }

    #[test]
    fn the_cache_path_follows_the_override_then_the_cache_home() {
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(CATALOG_CACHE_VAR, "/elsewhere/catalog.json") };
        assert_eq!(cache_path(), Some(PathBuf::from("/elsewhere/catalog.json")));
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var(CATALOG_CACHE_VAR) };
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(XDG_CACHE_HOME_VAR, "/cache-home") };
        assert_eq!(
            cache_path(),
            Some(PathBuf::from("/cache-home/horizon/models-dev-api.json"))
        );
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var(XDG_CACHE_HOME_VAR) };
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(HOME_VAR, "/home/someone") };
        assert_eq!(
            cache_path(),
            Some(PathBuf::from(
                "/home/someone/.cache/horizon/models-dev-api.json"
            ))
        );
    }

    /// Not a check: a live fetch of the real catalog, for a reader who
    /// wants to see what it says about a model in use. Run by hand with
    /// `--run-ignored all --no-capture`; the copy goes to a temporary path,
    /// not the real cache.
    #[tokio::test]
    #[ignore = "fetches the live catalog over the network"]
    async fn live_catalog_knows_ollama_cloud() {
        let dir = std::env::temp_dir().join(format!("horizon-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(CATALOG_CACHE_VAR, dir.join("catalog.json")) };
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var(CATALOG_URL_VAR) };
        let limits = catalog_limits(vendor("ollama"), "deepseek-v4.1-flash").await;
        println!("live catalog, ollama / deepseek-v4.1-flash: {limits:?}");
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
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(CATALOG_URL_VAR, "http://127.0.0.1:1/api.json") };
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(CATALOG_CACHE_VAR, &cache) };

        assert_eq!(
            catalog_limits(vendor("ollama"), "horizon-test-model")
                .await
                .map(|limits| limits.context_length),
            Some(65_536)
        );
        // What the session loaded is what a turn now encodes with.
        assert!(
            current()
                .get(vendor("ollama").id(), "horizon-test-model")
                .is_some()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
