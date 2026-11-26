//! Horizon's own guidance to its agent, added as a prompt section like any
//! package's: who the agent is in Horizon, how it talks, and what it
//! confirms before destroying something. The core prompt says only that
//! the model is a coding agent and where it starts (`horizon_agent::prompt`).

use std::sync::Arc;

use horizon_agent::packages::PackagesBuilder;
use horizon_agent::prompt::{PromptSection, SectionFacts};
use horizon_agent_host::extension::Extension;

/// Adds [`Guidance`] to every session.
pub(crate) struct HorizonGuidance;

impl Extension for HorizonGuidance {
    fn packages(&self, packages: PackagesBuilder) -> PackagesBuilder {
        packages.sections(vec![Arc::new(Guidance)])
    }
}

/// The section's text.
pub(crate) const GUIDANCE: &str = "You are the Horizon agent, embedded in the Horizon desktop shell.\n\
     Answer directly; use tools only when they add information you don't already have.\n\
     Your session outlives this conversation window: it survives application restarts, and its \
     full history is retained beyond what you can currently see.\n\
     \n\
     Communication:\n\
     - Be concise; don't restate what the transcript already shows.\n\
     - Report outcomes faithfully: state failures and partial results plainly rather than \
     presenting them as success.\n\
     \n\
     Treat these as destructive — confirm they match what the user asked before doing them:\n\
     - Overwriting or deleting files or data.\n\
     - Force-pushing, resetting, or discarding uncommitted changes.\n\
     - Dropping, truncating, or migrating a database.\n\
     - Any command whose effects reach outside the current workspace.";

struct Guidance;

impl PromptSection for Guidance {
    fn name(&self) -> &'static str {
        "horizon"
    }

    fn text(&self, _session: &SectionFacts<'_>) -> Option<String> {
        Some(GUIDANCE.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guidance carries what the core prompt no longer does: the
    /// destructive-action caution and the communication norms.
    #[test]
    fn the_guidance_carries_the_caution_and_the_norms() {
        let lower = GUIDANCE.to_ascii_lowercase();
        assert!(lower.contains("destructive"));
        assert!(lower.contains("be concise"));
        assert!(lower.contains("report outcomes faithfully"));
    }

    /// It reaches a session as a package section, through the host's
    /// extensions.
    #[test]
    fn the_guidance_is_a_package_section() {
        let packages =
            horizon_agent_host::extension::Extensions::new(vec![Arc::new(HorizonGuidance)])
                .packages();
        assert!(format!("{packages:?}").contains("horizon"));
    }
}
