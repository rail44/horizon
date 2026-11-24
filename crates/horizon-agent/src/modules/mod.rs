//! What customizes a session, as modules that plug into fixed stages of
//! the turn loop (`docs/agent-core-extension-design.md`, "段"): the prompt
//! sections a model call is sent with, and the decision whether a turn the
//! model ended goes on. The loop itself -- sending the sections, running
//! another round, recording what happened -- stays in the core
//! (`providers::rig::session`). A session's modules are composed when it
//! starts ([`builtin`]).

pub(crate) mod continuation;
mod sections;

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::RigAgentConfig;
use crate::contract::ContinuationVerdict;
use crate::prompt::SessionEnvironment;
use crate::roles::RoleDefinition;

pub(crate) use continuation::{ContinuationCheck, DecisionContinuation};

/// What the resolve stage knows about the session.
pub(crate) struct ResolveContext<'a> {
    pub(crate) environment: &'a SessionEnvironment,
    pub(crate) config: &'a RigAgentConfig,
    pub(crate) role: Option<&'static RoleDefinition>,
    pub(crate) trusted_project: bool,
}

/// One named part of the system prompt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Section {
    pub(crate) name: &'static str,
    pub(crate) text: String,
}

/// A round the model ended with a plain reply.
pub(crate) struct RoundContext<'a> {
    /// The input that started the turn.
    pub(crate) request: &'a str,
    /// The reply the round ended with.
    pub(crate) reply: &'a str,
    /// Tool calls the turn's rounds requested.
    pub(crate) tool_calls: usize,
    /// Continuations the turn has run.
    pub(crate) continuations_so_far: u32,
}

/// A module's answer at the end of a round: whether to run another round
/// with `prompt`, and the record of how it decided.
pub(crate) struct RoundDecision {
    pub(crate) prompt: Option<&'static str>,
    pub(crate) verdict: ContinuationVerdict,
}

#[async_trait]
pub(crate) trait SessionModule: Send + Sync {
    /// The resolve stage: sections this module adds to the system prompt.
    fn sections(&self, _cx: &ResolveContext<'_>) -> Vec<Section> {
        Vec::new()
    }

    /// The continue stage: `None` when this module has no say.
    async fn after_round(&self, _cx: &RoundContext<'_>) -> Option<RoundDecision> {
        None
    }
}

/// The modules every session starts with, in prompt order.
pub(crate) fn builtin(
    config: &RigAgentConfig,
    role: Option<&'static RoleDefinition>,
) -> Vec<Arc<dyn SessionModule>> {
    let mut modules: Vec<Arc<dyn SessionModule>> = vec![
        Arc::new(sections::DelegationRouting),
        Arc::new(sections::RoleSection),
        Arc::new(sections::Skills),
        Arc::new(sections::RepositoryInstructions),
        Arc::new(sections::KnowledgeIndex),
    ];
    if let Some(check) = ContinuationCheck::for_session(config, role) {
        modules.push(Arc::new(DecisionContinuation::new(check)));
    }
    modules
}

/// The prompt sections `modules` give a session.
pub(crate) fn sections(
    modules: &[Arc<dyn SessionModule>],
    cx: &ResolveContext<'_>,
) -> Vec<Section> {
    modules
        .iter()
        .flat_map(|module| module.sections(cx))
        .collect()
}
