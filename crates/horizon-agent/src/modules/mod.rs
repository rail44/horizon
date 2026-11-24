//! What customizes a session, as modules that plug into fixed stages of
//! the turn loop (`docs/agent-core-extension-design.md`, "段"): the prompt
//! sections a model call is sent with, the verdict on a tool call, and the
//! decision whether a turn the model ended goes on. The loop itself --
//! sending the sections, carrying out a verdict, running another round,
//! recording what happened -- stays in the core (`providers::rig::session`,
//! `tools::gate`). A session's modules are composed when it starts
//! ([`builtin`]).

pub(crate) mod continuation;
mod sections;
mod tier;

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::RigAgentConfig;
use crate::contract::ContinuationVerdict;
use crate::prompt::SessionEnvironment;
use crate::roles::RoleDefinition;

pub(crate) use continuation::{ContinuationCheck, DecisionContinuation};
pub(crate) use tier::TierPolicy;

/// What a tool call reaches, as its tool says (`tools::tool::Tool::reach`):
/// the facts the verdict stage decides from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reach {
    /// The catalog's permission for the tool; `None` for a tool the catalog
    /// does not list.
    pub(crate) permission: Option<crate::contract::ToolPermission>,
    pub(crate) containment: Containment,
    /// What a person would be asked, when the call is put to one.
    pub(crate) ask: Option<(String, crate::contract::ApprovalKind)>,
    /// Whether it writes where no containment can refuse after the fact
    /// (the file tools, an area request, `bash` without a sandbox).
    pub(crate) uncontained_write: bool,
    /// The shared primary checkout it writes, which the session may not.
    pub(crate) primary_checkout: Option<std::path::PathBuf>,
    /// The repository Git metadata it writes.
    pub(crate) git_metadata: Option<String>,
}

/// Where a call runs, as far as what contains it goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Containment {
    /// Reads inside what the session may read, or touches only the session.
    Inside,
    /// Reads outside the session's workspace root.
    ReadsOutside,
    /// Writes or runs inside a containment: the OS sandbox, or the file
    /// tools' own root check.
    Contained,
    /// Reaches past the containment, as the network does; `allowed` when
    /// the session already allows what it reaches.
    CrossesBoundary { allowed: bool },
    /// Runs with nothing to contain it.
    Uncontained,
    /// Runs code a package supplies, which answers for it.
    Package,
}

/// A call put to the verdict stage's reviewers before a person is asked:
/// one that takes it answers later on `results` as
/// `ToolCompletion::ApprovalJudged`.
pub(crate) struct ReviewContext<'a> {
    pub(crate) session_id: crate::contract::SessionId,
    pub(crate) candidate: &'a crate::judge::ApprovalCandidate,
    pub(crate) results: &'a crossbeam_channel::Sender<crate::tools::ToolCompletion>,
}

/// What the verdict stage knows about a call.
pub(crate) struct CallContext<'a> {
    pub(crate) state: &'a crate::tools::ToolSessionState,
    pub(crate) call: &'a crate::tools::input::PreparedCall<'a>,
    pub(crate) reach: &'a Reach,
}

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

    /// The verdict stage: whether the call runs, waits for an approval, or
    /// is refused; `None` when this module has no say.
    fn verdict(&self, _cx: &CallContext<'_>) -> Option<crate::policy::ToolPlan> {
        None
    }

    /// The verdict stage, for a call about to be put to a person: whether
    /// this module takes the call and decides it itself, later.
    fn review(&self, _cx: &ReviewContext<'_>) -> bool {
        false
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

/// The verdict stage's modules a session starts with.
pub(crate) fn verdict_modules() -> Vec<Arc<dyn SessionModule>> {
    vec![Arc::new(TierPolicy)]
}

/// The verdict `modules` give a call: the first that has a say; with none,
/// the call is put to a person.
pub(crate) fn verdict(
    modules: &[Arc<dyn SessionModule>],
    cx: &CallContext<'_>,
) -> crate::policy::ToolPlan {
    modules
        .iter()
        .find_map(|module| module.verdict(cx))
        .unwrap_or_else(|| {
            crate::policy::ToolPlan::Approval(Box::new(crate::contract::ApprovalRequest {
                call_id: cx.call.call_id.clone(),
                occurrence_id: cx.call.occurrence_id.clone(),
                reason: format!(
                    "`{}` requested Horizon approval for this tool call.",
                    cx.call.tool_id
                ),
                kind: crate::contract::ApprovalKind::Standard,
            }))
        })
}
