use std::path::PathBuf;

mod completion;
mod history;
mod mapping;
mod session;
mod stream;

use completion::{
    complete_rig_turn, deterministic_rig_response, deterministic_tool_result_response,
};
use history::load_rig_history;
use mapping::{rig_tool_result_message, rig_workspace_snapshot_call};
use session::spawn_rig_session;
use stream::{StreamDeltaBuffer, StreamDeltaKind};

use crate::{
    agent::config::RigAgentConfig,
    agent::contract::{Provider as AgentProvider, ProviderId, SessionHandle, StartSession},
};

pub(crate) struct Provider {
    config: RigAgentConfig,
    memory_duckdb_path: Option<PathBuf>,
}

impl Provider {
    pub(crate) fn new(config: RigAgentConfig, memory_duckdb_path: Option<PathBuf>) -> Self {
        Self {
            config,
            memory_duckdb_path,
        }
    }
}

impl AgentProvider for Provider {
    fn provider_id(&self) -> ProviderId {
        ProviderId("builtin.agent.rig".to_string())
    }

    fn start_session(&self, request: StartSession) -> SessionHandle {
        spawn_rig_session(
            request,
            self.config.clone(),
            self.memory_duckdb_path.clone(),
        )
    }
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
