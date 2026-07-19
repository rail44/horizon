mod completion;
mod history;
mod mapping;
mod memory;
mod model_catalog;
mod session;
mod stream;

use completion::{
    complete_rig_turn, deterministic_rig_response, deterministic_tool_result_response,
    ToolCallDescriptor, TurnCompletion,
};
use history::load_rig_history;
use mapping::{rig_tool_result_message, rig_workspace_snapshot_call};
use session::spawn_rig_session;
use stream::{StreamDeltaBuffer, StreamDeltaKind, ToolCallProgressBuffer};

use crate::{
    config::RigAgentConfig,
    contract::{Provider as AgentProvider, ProviderId, SessionHandle, StartSession},
    persistence::projection::duckdb::SharedDuckdbStore,
    roles::{RoleDefinition, RoleId},
};

pub(crate) struct Provider {
    config: RigAgentConfig,
    /// Shared, multi-reader-blocking handle onto the live DuckDB projection
    /// -- see [`SharedDuckdbStore`]'s doc comment. Cloned into every
    /// session's own dedicated rig thread (`start_session`/
    /// `spawn_rig_session`), which blocks on it (never this method, and
    /// never sessiond's async accept loop) until the event-log writer's own
    /// rebuild-or-open decision is known.
    duckdb_cell: SharedDuckdbStore,
}

impl Provider {
    pub(crate) fn new(config: RigAgentConfig, duckdb_cell: SharedDuckdbStore) -> Self {
        Self {
            config,
            duckdb_cell,
        }
    }
}

impl AgentProvider for Provider {
    fn provider_id(&self) -> ProviderId {
        ProviderId("builtin.agent.rig".to_string())
    }

    /// Resolves `request.role_id` (defensively -- an unresolvable role here
    /// silently has no effect on this session's config/prompt, but
    /// production sessions never reach this with one:
    /// `contract::ProviderRegistry::start_session` already refused to start
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
    /// (`!self.config.openai_enabled`, i.e. no `OPENAI_API_KEY`): a fallback
    /// turn never calls a provider at all (`completion::complete_rig_turn`
    /// skips `Event::ProviderRequestSent` entirely in that branch), so
    /// reporting a model here would claim a model is in play when none
    /// actually is.
    fn resolved_model(&self, role_id: Option<&RoleId>) -> Option<String> {
        if !self.config.openai_enabled {
            return None;
        }
        let role = role_id.and_then(crate::roles::resolve);
        Some(role_adjusted_config(&self.config, role).model)
    }
}

/// Applies a role's `allowed_tool_ids`/`model` overrides on top of the
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
    if config.openai_enabled {
        format!(
            "Rig provider `{}` initialized with OpenAI model `{}`.{}",
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
