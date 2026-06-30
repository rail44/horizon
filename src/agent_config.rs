use std::path::PathBuf;

use rig_core::providers::openai;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentConfig {
    pub rig: RigAgentConfig,
    pub persistence: AgentPersistenceConfig,
}

impl AgentConfig {
    pub fn from_env() -> Self {
        Self {
            rig: RigAgentConfig::from_env(),
            persistence: AgentPersistenceConfig::from_env(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RigAgentConfig {
    pub openai_enabled: bool,
    pub model: String,
}

impl RigAgentConfig {
    pub fn from_env() -> Self {
        Self {
            openai_enabled: std::env::var_os("OPENAI_API_KEY").is_some(),
            model: std::env::var("HORIZON_RIG_MODEL")
                .unwrap_or_else(|_| openai::GPT_4O_MINI.to_string()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPersistenceConfig {
    pub event_log_path: PathBuf,
    pub duckdb_path: Option<PathBuf>,
}

impl AgentPersistenceConfig {
    pub fn from_env() -> Self {
        Self {
            event_log_path: std::env::var_os("HORIZON_AGENT_EVENT_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("horizon-agent-events.jsonl")),
            duckdb_path: std::env::var_os("HORIZON_AGENT_STATE_DB").map(PathBuf::from),
        }
    }
}
