//! Translate the accepted file configuration into runtime configuration.
use horizon_agent::config::{MoaEntry, MoaMember, ModelPair, NamedProviderConfig, ProviderKind};

/// Translates the resolved `[[moa]]` surface out of the same config file
/// load (`docs/agent-moa-design.md`). Entries `horizon-config` already
/// refused (no name, an aggregator naming no `[[providers]]` entry) are gone
/// by here, having warned on stderr.
pub(crate) fn moa_configs(config: &horizon_config::RawConfig) -> Vec<MoaEntry> {
    // Availability and the key variable's name are filled in centrally by
    // `from_env_and_providers`, from the `[[providers]]` entry each member
    // names.
    let member = |member: &horizon_config::ResolvedMoaMember| {
        MoaMember::new(member.provider.clone(), member.model.clone())
    };
    config
        .resolved_moa()
        .iter()
        .map(|entry| MoaEntry {
            name: entry.name.clone(),
            aggregator: member(&entry.aggregator),
            proposers: entry.proposers.iter().map(member).collect(),
        })
        .collect()
}

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
/// (titles) and `decision_model` connections.
pub(crate) fn agent_config(raw: &horizon_config::RawConfig) -> horizon_agent::config::AgentConfig {
    let (entries, default_model) = named_provider_configs(raw);
    let mut config = horizon_agent::config::AgentConfig::from_env_and_providers(
        entries,
        default_model,
        moa_configs(raw),
    );
    config.auxiliary = raw.resolved_small_model().ok().map(|small| {
        horizon_agent::auxiliary::AuxiliaryConfig::from_env(
            small.provider.base_url,
            small.provider.api_key_env,
            small.model,
        )
    });
    config.decision = raw.resolved_decision_model().ok().map(|decision| {
        horizon_agent::decision::DecisionConfig::from_env(
            decision.provider.base_url,
            decision.provider.api_key_env,
            decision.model,
        )
    });
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_small_and_decision_models_resolve_independently() {
        std::env::remove_var("OPENAI_BASE_URL");
        let raw = horizon_config::parse(
            r#"
            default_model = { provider = "chat", model = "big" }
            small_model = { provider = "helper", model = "small" }
            decision_model = { provider = "ollama", model = "clef:cloud" }
            [[providers]]
            name = "chat"
            kind = "anthropic"
            api_key_env = "CHAT_KEY"
            [[providers]]
            name = "helper"
            kind = "openai-compatible"
            base_url = "https://helper.invalid/v1"
            api_key_env = "HELPER_KEY"
            [[providers]]
            name = "ollama"
            base_url = "https://ollama.com/v1"
            api_key_env = "OLLAMA_API_KEY"
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
        let decision = before.decision.as_ref().unwrap();
        assert_eq!(decision.api_key_env, "OLLAMA_API_KEY");
        assert_eq!(decision.model, "clef:cloud");
        assert_eq!(decision.base_url.as_deref(), Some("https://ollama.com/v1"));
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
        // An anthropic entry cannot serve small_model; an absent pair
        // disables it too.
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
