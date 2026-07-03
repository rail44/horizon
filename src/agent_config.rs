use std::path::PathBuf;

use rig_core::providers::openai;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentConfig {
    pub(crate) rig: RigAgentConfig,
    pub(crate) persistence: AgentPersistenceConfig,
}

impl AgentConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            rig: RigAgentConfig::from_env(),
            persistence: AgentPersistenceConfig::from_env(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RigAgentConfig {
    pub(crate) openai_enabled: bool,
    pub(crate) model: String,
}

impl RigAgentConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            openai_enabled: std::env::var_os("OPENAI_API_KEY").is_some(),
            model: std::env::var("HORIZON_RIG_MODEL")
                .unwrap_or_else(|_| openai::GPT_4O_MINI.to_string()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentPersistenceConfig {
    pub(crate) event_log_path: PathBuf,
    pub(crate) duckdb_path: Option<PathBuf>,
}

impl AgentPersistenceConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            event_log_path: std::env::var_os("HORIZON_AGENT_EVENT_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("horizon-agent-events.jsonl")),
            duckdb_path: std::env::var_os("HORIZON_AGENT_STATE_DB").map(PathBuf::from),
        }
    }
}
