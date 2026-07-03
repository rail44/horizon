//! Agent provider and persistence configuration.
//!
//! Per `docs/agent-tools-design.md`'s "Config" section: provider/model
//! selection and the API key flow through environment variables and the
//! existing rig provider path, with no configuration UI. This module is
//! the single place that names those variables; keep it authoritative
//! rather than duplicating var names elsewhere.

use std::path::PathBuf;

use rig_core::providers::openai;

/// Presence gates the OpenAI-backed rig completion path (see
/// [`RigAgentConfig::openai_enabled`]). The **value** of this variable is
/// never read here — `rig_core`'s `openai::CompletionsClient::from_env()`
/// (called per-turn in `providers::rig::completion`) reads it directly to
/// authenticate. Horizon only checks whether it is set, so the session can
/// decide up front whether to attempt the OpenAI path at all or fall back
/// to a deterministic in-process responder (useful offline and in tests).
const OPENAI_API_KEY_VAR: &str = "OPENAI_API_KEY";

/// Overrides the rig completion model id. Falls back to
/// [`openai::GPT_4O_MINI`] when unset.
const RIG_MODEL_VAR: &str = "HORIZON_RIG_MODEL";

/// Overrides the path of the append-only agent event log (JSONL). Falls
/// back to a fixed path under the OS temp directory.
const EVENT_LOG_PATH_VAR: &str = "HORIZON_AGENT_EVENT_LOG";

/// Overrides the path of the DuckDB projection database used to replay
/// per-session rig history. Unset means no persisted memory: sessions
/// start with empty history and nothing is written.
const STATE_DB_PATH_VAR: &str = "HORIZON_AGENT_STATE_DB";

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

/// Rig provider configuration. Model selection and API-key *presence* are
/// both sourced from environment variables (see [`RIG_MODEL_VAR`] and
/// [`OPENAI_API_KEY_VAR`]) — there is no configuration UI in v1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RigAgentConfig {
    /// Whether `OPENAI_API_KEY` is set. When `false`, the rig provider
    /// answers with a deterministic fallback responder instead of calling
    /// OpenAI (see `providers::rig::completion::complete_rig_turn`).
    pub(crate) openai_enabled: bool,
    /// Completion model id passed to `rig_core`'s OpenAI client.
    pub(crate) model: String,
}

impl RigAgentConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            openai_enabled: std::env::var_os(OPENAI_API_KEY_VAR).is_some(),
            model: std::env::var(RIG_MODEL_VAR).unwrap_or_else(|_| openai::GPT_4O_MINI.to_string()),
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
            event_log_path: std::env::var_os(EVENT_LOG_PATH_VAR)
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("horizon-agent-events.jsonl")),
            duckdb_path: std::env::var_os(STATE_DB_PATH_VAR).map(PathBuf::from),
        }
    }
}
