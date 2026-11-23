//! A session's reasoning effort: how much its model reasons before
//! answering, as the level words rig's portable `Effort` and the providers'
//! `reasoning_effort` share (`docs/agent-reasoning-effort-design.md`).

use rig_core::catalog::{ModelSpec, ReasoningSupport};
use rig_core::completion::{Effort, Reasoning};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One reasoning level, lowest first. `None` turns reasoning off.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl ReasoningLevel {
    /// Every level, lowest first.
    pub const ALL: [Self; 7] = [
        Self::None,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
    ];

    /// The level's word, as the config file and the wire spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// The level `word` names.
    pub fn parse(word: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|level| level.as_str() == word)
            .ok_or_else(|| {
                let words: Vec<&str> = Self::ALL.iter().map(|level| level.as_str()).collect();
                format!("effort {word:?} is not one of {}", words.join(", "))
            })
    }

    /// The portable option rig spells for the request's dialect.
    pub(crate) fn to_rig(self) -> Reasoning {
        match self {
            Self::None => Reasoning::Off,
            Self::Minimal => Reasoning::Effort(Effort::Minimal),
            Self::Low => Reasoning::Effort(Effort::Low),
            Self::Medium => Reasoning::Effort(Effort::Medium),
            Self::High => Reasoning::Effort(Effort::High),
            Self::XHigh => Reasoning::Effort(Effort::XHigh),
            Self::Max => Reasoning::Effort(Effort::Max),
        }
    }

    /// The level rig's `effort` is, if it is one of the seven.
    fn from_rig(effort: &Effort) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|level| level.as_str() == effort.as_str())
    }

    /// The levels a model takes, as the catalog lists them for it: its
    /// effort levels, with `None` when it can turn reasoning off. Every
    /// level for a model the catalog does not list or whose controls it
    /// does not know -- the provider is then the one to refuse a level --
    /// and none for a model that does not reason.
    pub fn choices(spec: Option<&ModelSpec>) -> Vec<Self> {
        let Some(spec) = spec else {
            return Self::ALL.to_vec();
        };
        match &spec.reasoning {
            ReasoningSupport::None => Vec::new(),
            ReasoningSupport::Listed {
                levels,
                can_disable,
                ..
            } => {
                let mut choices: Vec<Self> = levels.iter().filter_map(Self::from_rig).collect();
                if *can_disable {
                    choices.push(Self::None);
                }
                choices.sort();
                choices.dedup();
                choices
            }
            _ => Self::ALL.to_vec(),
        }
    }

    /// The level a request sends when `self` is wanted and the model takes
    /// `choices`: `self` when it is one of them, else the highest choice
    /// below it, else the lowest choice; nothing for a model that takes no
    /// level.
    pub fn fit(self, choices: &[Self]) -> Option<Self> {
        if choices.contains(&self) {
            return Some(self);
        }
        let below = choices
            .iter()
            .copied()
            .filter(|choice| *choice < self)
            .max();
        below.or_else(|| choices.iter().copied().min())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::catalog::Catalog;
    use rig_core::providers::registry::ProviderId;

    fn spec(vendor: &str, model: &str) -> ModelSpec {
        let id = ProviderId::catalog(vendor).unwrap();
        Catalog::builtin().get_exact(id, model).unwrap().clone()
    }

    #[test]
    fn words_round_trip_through_the_config_and_the_wire() {
        for level in ReasoningLevel::ALL {
            assert_eq!(ReasoningLevel::parse(level.as_str()), Ok(level));
            let json = serde_json::to_value(level).unwrap();
            assert_eq!(json, serde_json::Value::String(level.as_str().into()));
        }
        assert!(
            ReasoningLevel::parse("bogus")
                .unwrap_err()
                .contains("xhigh")
        );
        assert_eq!(ReasoningLevel::default(), ReasoningLevel::Medium);
    }

    #[test]
    fn a_model_the_catalog_does_not_list_takes_every_level() {
        assert_eq!(ReasoningLevel::choices(None), ReasoningLevel::ALL.to_vec());
    }

    #[test]
    fn a_model_that_does_not_reason_takes_none() {
        assert!(ReasoningLevel::choices(Some(&spec("ollama", "llama3.3"))).is_empty());
    }

    #[test]
    fn a_listed_models_choices_follow_its_catalog_row() {
        // Every choice the catalog yields is one rig would send for it.
        let opus = spec("anthropic", "claude-opus-4-5");
        for level in ReasoningLevel::choices(Some(&opus)) {
            assert!(
                opus.validate(
                    &rig_core::completion::GenerationOptions::default().reasoning(level.to_rig())
                )
                .is_ok(),
                "{level:?}"
            );
        }
    }

    #[test]
    fn an_unavailable_level_fits_to_the_nearest_one_below() {
        use ReasoningLevel::*;
        let choices = [Low, High, Max];
        assert_eq!(High.fit(&choices), Some(High));
        assert_eq!(XHigh.fit(&choices), Some(High));
        assert_eq!(Medium.fit(&choices), Some(Low));
        assert_eq!(None.fit(&choices), Some(Low));
        assert_eq!(High.fit(&[]), Option::None);
    }
}
