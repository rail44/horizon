//! Translate the accepted file configuration into runtime configuration.
use horizon_agent::config::{ModelPair, NamedProviderConfig, ProviderKind};

/// Translates the resolved provider surface out of a config file load.
/// Returns the entries in file order plus the `default_model` pair, when it
/// resolves against them.
pub(crate) fn named_provider_configs(
    config: &horizon_config::RawConfig,
) -> (Vec<NamedProviderConfig>, Option<ModelPair>) {
    let resolution = config.resolved_providers();
    let entries = resolution
        .providers
        .iter()
        .map(|entry| NamedProviderConfig {
            name: entry.name.clone(),
            kind: match entry.kind {
                horizon_config::RawProviderKind::OpenAiCompatible => ProviderKind::OpenAiCompatible,
                horizon_config::RawProviderKind::Anthropic => ProviderKind::Anthropic,
            },
            base_url: entry.base_url.clone(),
            api_key_env: entry.api_key_env.clone(),
            // Resolved centrally by `from_env_and_providers`.
            api_key_present: false,
        })
        .collect();
    let default_model = resolution.default_model.map(|pair| ModelPair {
        provider: pair.provider,
        model: pair.model,
    });
    (entries, default_model)
}

/// Startup and reload share one translation, including the `small_model`
/// connection.
pub(crate) fn agent_config(raw: &horizon_config::RawConfig) -> horizon_agent::config::AgentConfig {
    let (entries, default_model) = named_provider_configs(raw);
    let mut config =
        horizon_agent::config::AgentConfig::from_env_and_providers(entries, default_model);
    config.auxiliary = raw.resolved_small_model().ok().map(|small| {
        horizon_agent::auxiliary::AuxiliaryConfig::from_env(
            small.provider.base_url,
            small.provider.api_key_env,
            small.model,
        )
    });
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_small_models_resolve_independently() {
        std::env::remove_var("OPENAI_BASE_URL");
        let raw = horizon_config::parse(
            r#"
            default_model = { provider = "chat", model = "big" }
            small_model = { provider = "helper", model = "small" }
            [[providers]]
            name = "chat"
            kind = "anthropic"
            api_key_env = "CHAT_KEY"
            [[providers]]
            name = "helper"
            kind = "openai-compatible"
            base_url = "https://helper.invalid/v1"
            api_key_env = "HELPER_KEY"
        "#,
        )
        .unwrap();
        let before = agent_config(&raw);
        assert_eq!(before.rig.kind, ProviderKind::Anthropic);
        assert_eq!(before.rig.model, "big");
        let auxiliary = before.auxiliary.as_ref().unwrap();
        assert_eq!(auxiliary.api_key_env, "HELPER_KEY");
        assert_eq!(auxiliary.model, "small");
        assert_eq!(
            auxiliary.base_url.as_deref(),
            Some("https://helper.invalid/v1")
        );
        let mut changed = raw.clone();
        changed.providers[1].base_url = Some("https://new.invalid/v1".into());
        let after = agent_config(&changed);
        assert_ne!(before.auxiliary, after.auxiliary);
        assert_eq!(
            auxiliary.base_url.as_deref(),
            Some("https://helper.invalid/v1")
        );
        // An anthropic entry cannot serve small_model; an absent pair
        // disables it too.
        changed.small_model = Some(horizon_config::RawModelPair {
            provider: "chat".into(),
            model: "small".into(),
        });
        assert!(agent_config(&changed).auxiliary.is_none());
        changed.small_model = None;
        assert!(agent_config(&changed).auxiliary.is_none());
    }

    #[test]
    fn a_missing_default_model_leaves_sessions_without_a_model() {
        let raw = horizon_config::parse("[[providers]]\nname = \"chat\"\n").unwrap();
        let config = agent_config(&raw);
        assert_eq!(config.rig.model, "");
        assert_eq!(config.providers.default_model, None);
        assert!(config.auxiliary.is_none());
    }
}
