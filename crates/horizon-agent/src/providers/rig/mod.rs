mod clearing;
mod completion;
pub(crate) mod conversation;
mod guards;
mod history;
mod mapping;
mod model_limits;
mod retry;
mod session;
mod session_prompt;
mod stream;

pub(crate) use model_limits::list_model_ids;

use clearing::ClearingState;
#[allow(unused_imports)]
use completion::{complete_rig_turn, ToolCallDescriptor, TurnCompletion};
#[allow(unused_imports)]
use history::load_rig_session_history;
use mapping::rig_workspace_snapshot_call;
use session::spawn_rig_session;
use stream::{StreamDeltaBuffer, StreamDeltaKind, ToolCallProgressBuffer};

use crate::{
    config::{ProviderKind, RigAgentConfig},
    contract::{ProviderId, StartSession},
    persistence::projection::duckdb::SharedDuckdbStore,
    registry::{Provider as AgentProvider, SessionHandle},
    roles::{RoleDefinition, RoleId},
};

pub(crate) struct Provider {
    /// This entry's registry id (`builtin.agent.rig.<name>`; the default
    /// entry is ALSO registered under the standing shell-facing id — see
    /// `registry::ProviderRegistry::builtin_with_config`).
    id: ProviderId,
    /// This entry's resolved config — what a session started on this entry
    /// runs with. For the default entry this is `AgentConfig::rig` itself
    /// (see `registry::builtin_with_config` for why the state's own view is
    /// authoritative), for any other entry its `NamedProviderConfig::
    /// resolved`.
    config: RigAgentConfig,
    /// Shared, multi-reader-blocking handle onto the live DuckDB projection
    /// -- see [`SharedDuckdbStore`]'s doc comment. Cloned into every
    /// session's own dedicated rig thread (`start_session`/
    /// `spawn_rig_session`), which blocks on it (never this method, and
    /// never agentd's async accept loop) until the event-log writer's own
    /// rebuild-or-open decision is known.
    duckdb_cell: SharedDuckdbStore,
}

impl Provider {
    /// Builds one entry's provider. `registry::builtin_with_config` owns
    /// which id(s) the entry registers under and whether it passes the
    /// state's own `rig` view (the default entry) or this entry's own
    /// resolution.
    pub(crate) fn for_entry(
        id: ProviderId,
        config: RigAgentConfig,
        duckdb_cell: SharedDuckdbStore,
    ) -> Self {
        Self {
            id,
            config,
            duckdb_cell,
        }
    }
}

impl AgentProvider for Provider {
    fn provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    /// Resolves `request.role_id` (defensively -- an unresolvable role here
    /// silently has no effect on this session's config/prompt, but
    /// production sessions never reach this with one:
    /// `registry::ProviderRegistry::start_session` already refused to start
    /// them -- see that method's doc comment) and derives a per-session
    /// [`RigAgentConfig`] from it before spawning, per
    /// `docs/plans/agent-foundation/03-roles-and-config-agent.md`.
    fn start_session(&self, request: StartSession) -> SessionHandle {
        let role = request.role_id.as_ref().and_then(crate::roles::resolve);
        let config = role_adjusted_config(&self.config, role);
        spawn_rig_session(request, config, role, self.duckdb_cell.clone())
    }

    /// The same role-adjusted `config.model` [`Self::start_session`] would
    /// run with, without spawning anything -- reuses [`role_adjusted_config`]
    /// so the two never drift. `None` in deterministic fallback mode
    /// (`!self.config.api_key_present`, i.e. no `OPENAI_API_KEY`): a fallback
    /// turn never calls a provider at all (`completion::complete_rig_turn`
    /// skips `Event::ProviderRequestSent` entirely in that branch), so
    /// reporting a model here would claim a model is in play when none
    /// actually is.
    fn resolved_model(&self, role_id: Option<&RoleId>) -> Option<String> {
        // No key, or a kind with no Horizon-side default model at all
        // (an anthropic entry that lists nothing — see
        // `NamedProviderConfig::default_model`): a fallback turn never
        // calls a provider, so reporting a model would claim one is in
        // play when none is.
        if !self.config.api_key_present || self.config.model.is_empty() {
            return None;
        }
        let role = role_id.and_then(crate::roles::resolve);
        let model = role_adjusted_config(&self.config, role).model;
        if model.is_empty() {
            return None;
        }
        Some(model)
    }
}

/// Applies a role's `allowed_tool_ids`/`model`/`iteration_cap` overrides on top of the
/// provider's own (process-wide) [`RigAgentConfig`], producing the config
/// this one session actually runs with. `role: None` (the role-less case)
/// returns `base` cloned unchanged -- byte-identical behavior to before
/// roles existed.
fn role_adjusted_config(
    base: &RigAgentConfig,
    role: Option<&'static RoleDefinition>,
) -> RigAgentConfig {
    let mut config = base.clone();
    let Some(role) = role else {
        return config;
    };
    if let Some(allowed) = role.allowed_tool_ids {
        config.allowed_tool_ids = Some(allowed.iter().map(|id| id.to_string()).collect());
    }
    if let Some(model) = role.model {
        config.model = model.to_string();
    }
    if let Some(iteration_cap) = role.iteration_cap {
        config.iteration_cap = iteration_cap;
    }
    config
}

pub(super) fn rig_initialization_message(
    provider_id: &ProviderId,
    config: &RigAgentConfig,
    loaded_history_messages: usize,
) -> String {
    let memory = if loaded_history_messages == 0 {
        String::new()
    } else {
        format!(" Loaded {loaded_history_messages} persisted Rig history message(s).")
    };
    let kind = match config.kind {
        ProviderKind::OpenAiCompatible => "openai-compatible",
        ProviderKind::Anthropic => "anthropic",
    };
    if config.api_key_present {
        format!(
            "Rig provider `{}` initialized with {kind} model `{}`.{}",
            provider_id.0, config.model, memory
        )
    } else {
        format!(
            "Rig provider `{}` initialized in deterministic fallback mode.{}",
            provider_id.0, memory
        )
    }
}

#[cfg(test)]
mod tests;
