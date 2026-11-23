//! Translate the accepted file configuration into runtime configuration.
//! Which vendors exist, and which key variable each reads by default, is
//! rig's to say, so the entries are resolved here, in the crate that has
//! rig, rather than in `horizon-config`; what does not resolve is said on
//! stderr, as `horizon-config` says the file's own warnings.
use horizon_agent::config::{ModelPair, NamedProviderConfig, Vendor};

/// One resolved entry as the runtime holds it, or why it cannot serve.
fn named_provider_config(
    entry: &horizon_config::ResolvedProviderConfig,
) -> Result<NamedProviderConfig, String> {
    let resolved = Vendor::resolve(&entry.vendor).and_then(|vendor| {
        Ok(NamedProviderConfig {
            name: entry.name.clone(),
            vendor,
            base_url: entry.base_url.clone(),
            api_key_env: vendor.api_key_env(entry.api_key_env.clone())?,
            // Resolved centrally by `from_env_and_providers`.
            api_key_present: false,
        })
    });
    resolved.map_err(|error| {
        format!(
            "[[providers]] {:?}: {error} — the entry is dropped",
            entry.name
        )
    })
}

/// Translates the resolved provider surface out of a config file load.
/// Returns the entries in file order plus the `default_model` pair, when it
/// resolves against them, and what did not resolve.
pub(crate) fn named_provider_configs(
    config: &horizon_config::RawConfig,
) -> (Vec<NamedProviderConfig>, Option<ModelPair>, Vec<String>) {
    let resolution = config.resolved_providers();
    let mut warnings = Vec::new();
    let entries: Vec<NamedProviderConfig> = (resolution.providers.iter())
        .filter_map(|entry| {
            named_provider_config(entry)
                .map_err(|warning| warnings.push(warning))
                .ok()
        })
        .collect();
    let default_model = resolution.default_model.map(|pair| ModelPair {
        provider: pair.provider,
        model: pair.model,
    });
    (entries, default_model, warnings)
}

/// Startup and reload share one translation, including the `small_model`
/// (titles) and `decision_model` connections. What does not resolve is said
/// on stderr.
pub(crate) fn agent_config(raw: &horizon_config::RawConfig) -> horizon_agent::config::AgentConfig {
    let (entries, default_model, mut warnings) = named_provider_configs(raw);
    let mut config =
        horizon_agent::config::AgentConfig::from_env_and_providers(entries, default_model);
    config.auxiliary = raw.resolved_small_model().ok().and_then(|small| {
        horizon_agent::auxiliary::AuxiliaryConfig::resolve(
            &small.provider.vendor,
            small.provider.base_url,
            small.provider.api_key_env,
            small.model,
        )
        .map_err(|error| {
            warnings.push(format!(
                "small_model: {error} — titles are disabled, and so is the fallback judge"
            ))
        })
        .ok()
    });
    config.decision = raw.resolved_decision_model().ok().and_then(|decision| {
        let api_key_env = Vendor::resolve(&decision.provider.vendor)
            .and_then(|vendor| vendor.api_key_env(decision.provider.api_key_env));
        api_key_env
            .map(|api_key_env| horizon_agent::decision::DecisionConfig {
                url: decision.url,
                api_key_env,
                model: decision.model,
            })
            .map_err(|error| {
                warnings.push(format!(
                    "decision_model: {error} — approval judgments fall back to small_model's \
                     chat judge and turn-continuation checks are disabled"
                ))
            })
            .ok()
    });
    for warning in warnings {
        eprintln!("horizon config: {warning}");
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_small_and_decision_models_resolve_independently() {
        let raw = horizon_config::parse(
            r#"
            default_model = { provider = "chat", model = "big" }
            small_model = { provider = "helper", model = "small" }
            decision_model = { provider = "ollama", model = "clef:cloud" }
            [[providers]]
            name = "chat"
            vendor = "anthropic"
            api_key_env = "CHAT_KEY"
            [[providers]]
            name = "helper"
            base_url = "https://helper.invalid/v1"
            api_key_env = "HELPER_KEY"
            [[providers]]
            name = "ollama"
            base_url = "https://ollama.com/v1"
            api_key_env = "OLLAMA_API_KEY"
            decision_url = "https://ollama.com/v1/systemone"
        "#,
        )
        .unwrap();
        let before = agent_config(&raw);
        assert_eq!(before.rig.vendor, Vendor::resolve("anthropic").unwrap());
        assert_eq!(before.rig.model, "big");
        let auxiliary = before.auxiliary.as_ref().unwrap();
        assert_eq!(auxiliary.api_key_env, "HELPER_KEY");
        assert_eq!(auxiliary.model, "small");
        assert_eq!(
            auxiliary.base_url.as_deref(),
            Some("https://helper.invalid/v1")
        );
        let decision = before.decision.as_ref().unwrap();
        assert_eq!(decision.api_key_env, "OLLAMA_API_KEY");
        assert_eq!(decision.model, "clef:cloud");
        assert_eq!(decision.url, "https://ollama.com/v1/systemone");
        // An entry without a decision_url cannot serve decision_model.
        let mut no_url = raw.clone();
        no_url.providers[2].decision_url = None;
        assert!(agent_config(&no_url).decision.is_none());
        // An invalid decision_model leaves the other two pairs alone.
        let mut no_decision = raw.clone();
        no_decision.decision_model = Some(horizon_config::RawModelPair {
            provider: "chat".into(),
            model: "clef:cloud".into(),
        });
        let without = agent_config(&no_decision);
        assert!(without.decision.is_none());
        assert_eq!(without.auxiliary, before.auxiliary);
        assert_eq!(without.rig.model, "big");
        no_decision.decision_model = None;
        assert!(agent_config(&no_decision).decision.is_none());
        let mut changed = raw.clone();
        changed.providers[1].base_url = Some("https://new.invalid/v1".into());
        let after = agent_config(&changed);
        assert_ne!(before.auxiliary, after.auxiliary);
        assert_eq!(
            auxiliary.base_url.as_deref(),
            Some("https://helper.invalid/v1")
        );
        // An entry on Anthropic's wire cannot serve small_model; an absent
        // pair disables it too.
        changed.small_model = Some(horizon_config::RawModelPair {
            provider: "chat".into(),
            model: "small".into(),
        });
        assert!(agent_config(&changed).auxiliary.is_none());
        changed.small_model = None;
        assert!(agent_config(&changed).auxiliary.is_none());
        // Neither invalid small_model disturbs the decision connection.
        assert_eq!(agent_config(&changed).decision, before.decision);
    }

    #[test]
    fn a_missing_default_model_leaves_sessions_without_a_model() {
        let raw = horizon_config::parse("[[providers]]\nname = \"chat\"\n").unwrap();
        let config = agent_config(&raw);
        assert_eq!(config.rig.model, "");
        assert_eq!(config.providers.default_model, None);
        assert!(config.auxiliary.is_none());
        assert!(config.decision.is_none());
    }
}
