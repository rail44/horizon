//! Resolve MoA entries and their diagnostics together so accepted members and
//! warnings follow the same validation and ordering rules.

use serde::Deserialize;

use super::{ProvidersResolution, RawConfig};

/// One `[[moa]]` entry as the file writes it. Model ids are written out.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawMoaConfig {
    pub name: String,
    /// The member that writes the answer the pane shows.
    pub aggregator: RawMoaMember,
    /// One read-only `task`-shaped proposer session per entry, each on its
    /// own `{provider, model}`. Duplicates are kept: the same member listed
    /// twice runs twice.
    pub proposers: Vec<RawMoaMember>,
}

/// One `[[moa]]` member: a `[[providers]]` entry name plus a model id.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawMoaMember {
    pub provider: String,
    pub model: String,
}

/// One resolved `[[moa]]` entry — [`RawConfig::resolved_moa`]'s output, with
/// nameless/aggregator-less entries and members naming no `[[providers]]`
/// entry already dropped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMoaConfig {
    pub name: String,
    pub aggregator: ResolvedMoaMember,
    pub proposers: Vec<ResolvedMoaMember>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMoaMember {
    pub provider: String,
    pub model: String,
}

/// The reserved picker group for MoA entries.
pub const MOA_PROVIDER_NAME: &str = "moa";

impl RawConfig {
    /// Resolve named entries with valid aggregators, dropping invalid proposers
    /// individually. Duplicates and file order are preserved.
    pub fn resolved_moa(&self) -> Vec<ResolvedMoaConfig> {
        resolve_moa(self).0
    }
}

/// Value-level diagnostics for the same decisions as [`RawConfig::resolved_moa`].
/// Invalid entries warn and are skipped without failing the whole config.
pub fn moa_config_warnings(config: &RawConfig) -> Vec<String> {
    resolve_moa(config).1
}

fn resolve_moa(config: &RawConfig) -> (Vec<ResolvedMoaConfig>, Vec<String>) {
    let mut resolved = Vec::new();
    let mut warnings = Vec::new();
    if config.moa.is_empty() {
        return (resolved, warnings);
    }
    let providers = config.resolved_providers();
    if providers
        .providers
        .iter()
        .any(|entry| entry.name == MOA_PROVIDER_NAME)
    {
        warnings.push(format!(
            "[[providers]]: the name {MOA_PROVIDER_NAME:?} is reserved for the [[moa]] group in \
             model selection — rename that provider entry, it cannot be selected"
        ));
    }
    let mut seen: Vec<&str> = Vec::new();
    for (index, entry) in config.moa.iter().enumerate() {
        if entry.name.is_empty() {
            warnings.push(format!(
                "[[moa]]: entry {index} has no name, dropping it (name it so it can be selected)"
            ));
            continue;
        }
        // A named entry reserves its name even when its aggregator is invalid.
        if seen.contains(&entry.name.as_str()) {
            warnings.push(format!(
                "[[moa]]: duplicate name {} — the later entry is shadowed",
                entry.name
            ));
        } else {
            seen.push(entry.name.as_str());
        }
        if let Some(entry) = resolve_entry(entry, &providers, &mut warnings) {
            resolved.push(entry);
        }
    }
    (resolved, warnings)
}

fn resolve_entry(
    entry: &RawMoaConfig,
    providers: &ProvidersResolution,
    warnings: &mut Vec<String>,
) -> Option<ResolvedMoaConfig> {
    let aggregator = match resolve_member(&entry.aggregator, providers) {
        Ok(member) => member,
        Err(reason) => {
            warnings.push(format!(
                "[[moa]]: entry {:?} aggregator {reason}, dropping the whole entry",
                entry.name
            ));
            return None;
        }
    };
    let proposers = entry
        .proposers
        .iter()
        .enumerate()
        .filter_map(
            |(position, proposer)| match resolve_member(proposer, providers) {
                Ok(member) => Some(member),
                Err(reason) => {
                    warnings.push(format!(
                        "[[moa]]: entry {:?} proposer {position} {reason}, dropping that proposer",
                        entry.name
                    ));
                    None
                }
            },
        )
        .collect();
    // A list that was empty in the file differs from one emptied by validation.
    if entry.proposers.is_empty() {
        warnings.push(format!(
            "[[moa]]: entry {:?} lists no proposers — the aggregator will answer alone",
            entry.name
        ));
    }
    Some(ResolvedMoaConfig {
        name: entry.name.clone(),
        aggregator,
        proposers,
    })
}

fn resolve_member(
    member: &RawMoaMember,
    providers: &ProvidersResolution,
) -> Result<ResolvedMoaMember, String> {
    if member.model.is_empty() {
        return Err("has no model id".to_string());
    }
    if !providers
        .providers
        .iter()
        .any(|entry| entry.name == member.provider)
    {
        return Err(format!(
            "names no [[providers]] entry ({:?})",
            member.provider
        ));
    }
    Ok(ResolvedMoaMember {
        provider: member.provider.clone(),
        model: member.model.clone(),
    })
}
