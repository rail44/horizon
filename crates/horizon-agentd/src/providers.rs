//! Translate the accepted file configuration into runtime configuration.
use horizon_agent::config::{MoaEntry, MoaMember, NamedProviderConfig, ProviderKind};

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
/// Returns the entries in file order plus the default entry's name.
pub(crate) fn named_provider_configs(
    config: &horizon_config::RawConfig,
) -> (Vec<NamedProviderConfig>, String) {
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
            default_model: entry.default_model.clone(),
        })
        .collect();
    (entries, resolution.default_name)
}

/// Startup and reload share one translation, including the auxiliary provider.
pub(crate) fn agent_config(raw: &horizon_config::RawConfig) -> horizon_agent::config::AgentConfig {
    let (entries, default_name) = named_provider_configs(raw);
    let mut config = horizon_agent::config::AgentConfig::from_env_and_providers(
        entries,
        default_name,
        moa_configs(raw),
    );
    config.auxiliary = raw.resolved_auxiliary_provider().ok().map(|entry| {
        horizon_agent::auxiliary::AuxiliaryConfig::from_env(entry.base_url, entry.api_key_env)
    });
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_and_auxiliary_providers_resolve_independently() {
        std::env::remove_var("OPENAI_BASE_URL");
        let raw = horizon_config::parse(
            r#"
            default_provider = "chat"
            auxiliary_provider = "helper"
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
        let auxiliary = before.auxiliary.as_ref().unwrap();
        assert_eq!(auxiliary.api_key_env, "HELPER_KEY");
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
        changed.auxiliary_provider = Some("chat".into());
        assert!(agent_config(&changed).auxiliary.is_none());
    }
}
