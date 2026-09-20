//! The `horizon_config` → `horizon_agent::config` provider seam: the one
//! translation of `horizon_config`'s resolved `[[providers]]` entries (or
//! the legacy `[provider]` fold-in) into `horizon_agent::config::NamedProviderConfig`s. `main` (startup) and `AgentdState::
//! reload_provider_config` (reload) both call through here so the two
//! callers cannot drift — and so Horizon's env precedence stays where
//! `horizon_agent::config` owns it (this module never reads env itself).
use horizon_agent::config::{MoaEntry, MoaMember, NamedProviderConfig, ProviderKind};

/// Translates the resolved `[[moa]]` surface out of the same config file
/// load (`docs/agent-moa-design.md`). Entries `horizon-config` already
/// refused (no name, an aggregator naming no `[[providers]]` entry) are gone
/// by here, having warned on stderr.
pub(crate) fn moa_configs(config: &horizon_config::RawConfig) -> Vec<MoaEntry> {
    let member = |member: &horizon_config::ResolvedMoaMember| MoaMember {
        provider: member.provider.clone(),
        model: member.model.clone(),
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
/// Returns the entries in file order (aliases included, document order
/// preserved by `horizon-config`) plus the default entry's name.
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
            models: entry.models.clone(),
        })
        .collect();
    (entries, resolution.default_name)
}
