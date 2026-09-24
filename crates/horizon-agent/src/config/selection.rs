//! Resolve a model choice against one catalog snapshot before applying it.

use super::{
    apply_provider_entry, MoaPass, MoaTable, NamedProviderConfig, ProvidersTable, RigAgentConfig,
    MOA_PROVIDER_NAME,
};

/// A validated snapshot passed inside the daemon, never accepted over the wire.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedModelSelection {
    entry: NamedProviderConfig,
    model: String,
    moa: Option<MoaPass>,
    requested_provider: String,
    requested_model: String,
}

impl ResolvedModelSelection {
    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn requested_provider(&self) -> &str {
        &self.requested_provider
    }

    pub fn requested_model(&self) -> &str {
        &self.requested_model
    }

    /// Apply only model/provider fields; role restrictions and turn policy stay.
    pub(crate) fn apply(&self, config: &mut RigAgentConfig) {
        apply_provider_entry(config, &self.entry, &self.model);
        config.moa = self.moa.clone();
    }
}

/// Resolve once against the daemon's current catalog before queuing a switch.
/// Plain-provider keys are rechecked at application; MoA availability is captured.
pub fn resolve_model_selection(
    table: &ProvidersTable,
    moa_table: &MoaTable,
    provider: &str,
    model: &str,
) -> Result<ResolvedModelSelection, String> {
    if model.is_empty() {
        return Err("A model id is required.".into());
    }
    if provider == MOA_PROVIDER_NAME {
        let moa = moa_table
            .entry(model)
            .ok_or_else(|| format!("Unknown moa entry `{model}`."))?;
        if !moa.aggregator.api_key_present {
            return Err(format!(
                "moa entry `{model}` is unavailable: {}.",
                moa.aggregator.unavailable_reason()
            ));
        }
        let entry = table.entry(&moa.aggregator.provider).ok_or_else(|| {
            format!(
                "moa entry `{model}` names no provider `{}`.",
                moa.aggregator.provider
            )
        })?;
        Ok(ResolvedModelSelection {
            entry: entry.clone(),
            model: moa.aggregator.model.clone(),
            moa: Some(MoaPass {
                name: moa.name.clone(),
                proposers: moa.proposers.clone(),
            }),
            requested_provider: provider.to_owned(),
            requested_model: model.to_owned(),
        })
    } else {
        let entry = table
            .entry(provider)
            .ok_or_else(|| format!("Unknown provider `{provider}`."))?;
        Ok(ResolvedModelSelection {
            entry: entry.clone(),
            model: model.to_owned(),
            moa: None,
            requested_provider: provider.to_owned(),
            requested_model: model.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderKind;
    use crate::contract::Command;

    fn catalog() -> ProvidersTable {
        ProvidersTable {
            entries: vec![NamedProviderConfig {
                name: "selected".into(),
                kind: ProviderKind::Anthropic,
                base_url: Some("https://selected.invalid".into()),
                api_key_env: "HORIZON_SELECTION_TEST_KEY".into(),
                api_key_present: false,
                default_model: None,
            }],
            default_name: "selected".into(),
        }
    }

    #[test]
    fn queued_selection_keeps_its_resolved_provider_after_the_catalog_changes() {
        let mut table = catalog();
        let selection =
            resolve_model_selection(&table, &MoaTable::default(), "selected", "chosen").unwrap();
        table.entries.clear();
        let mut config = RigAgentConfig {
            model: "old-model".into(),
            iteration_cap: 7,
            ..Default::default()
        };
        selection.apply(&mut config);
        assert_eq!(config.model, "chosen");
        assert_eq!(config.kind, ProviderKind::Anthropic);
        assert_eq!(config.api_key_env, "HORIZON_SELECTION_TEST_KEY");
        assert_eq!(config.iteration_cap, 7, "role policy survives the switch");
    }

    #[test]
    fn resolved_provider_settings_cannot_cross_the_command_wire_boundary() {
        let selection =
            resolve_model_selection(&catalog(), &MoaTable::default(), "selected", "chosen")
                .unwrap();
        assert!(serde_json::to_value(Command::ApplySessionModel(Box::new(selection))).is_err());
        assert!(serde_json::from_value::<Command>(serde_json::json!({
            "ApplySessionModel": { "entry": { "base_url": "https://unconfigured.invalid" } }
        }))
        .is_err());
        assert!(serde_json::from_value::<Command>(serde_json::json!({
            "SetSessionModel": { "provider": "selected", "model": "chosen" }
        }))
        .is_ok());
    }
}
