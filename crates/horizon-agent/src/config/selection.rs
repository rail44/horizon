//! Resolve a model choice against one catalog snapshot before applying it.

use super::{
    apply_provider_entry, MoaEntry, MoaPass, MoaTable, NamedProviderConfig, ProvidersTable,
    RigAgentConfig, MOA_PROVIDER_NAME,
};

pub struct ResolvedModelSelection<'a> {
    entry: &'a NamedProviderConfig,
    model: &'a str,
    moa: Option<&'a MoaEntry>,
}

impl ResolvedModelSelection<'_> {
    pub fn model(&self) -> &str {
        self.model
    }

    /// Apply only model/provider fields; role restrictions and turn policy stay.
    pub(crate) fn apply(&self, config: &mut RigAgentConfig) {
        apply_provider_entry(config, self.entry, self.model);
        config.moa = self.moa.map(|entry| MoaPass {
            name: entry.name.clone(),
            proposers: entry.proposers.clone(),
        });
    }
}

/// Share validation rules while the caller chooses the catalog generation:
/// the daemon uses current config, and a running provider uses its startup config.
/// Plain-provider keys are rechecked at application; MoA availability is captured.
pub fn resolve_model_selection<'a>(
    table: &'a ProvidersTable,
    moa_table: &'a MoaTable,
    provider: &str,
    model: &'a str,
) -> Result<ResolvedModelSelection<'a>, String> {
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
            entry,
            model: &moa.aggregator.model,
            moa: Some(moa),
        })
    } else {
        let entry = table
            .entry(provider)
            .ok_or_else(|| format!("Unknown provider `{provider}`."))?;
        Ok(ResolvedModelSelection {
            entry,
            model,
            moa: None,
        })
    }
}
