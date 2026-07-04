//! Agent provider, tool, and persistence configuration.
//!
//! Per `docs/agent-tools-design.md`'s "Config" section and `AGENTS.md`'s
//! "Configuration" section: values here flow from (in precedence order)
//! environment variables, then Horizon's single config file
//! (`crate::config`), then a built-in default. Secrets (`OPENAI_API_KEY`)
//! are environment-only and never read from the config file. This module is
//! the single place that names the env vars and built-in defaults; keep it
//! authoritative rather than duplicating them elsewhere.

use std::path::PathBuf;

use rig_core::providers::openai;

use crate::config::RawConfig;

/// Presence gates the OpenAI-backed rig completion path (see
/// [`RigAgentConfig::openai_enabled`]). The **value** of this variable is
/// read directly by `providers::rig::completion` when it builds the OpenAI
/// client for a turn (never from the config file — see the module doc).
/// Horizon only checks whether it is set here, so the session can decide up
/// front whether to attempt the OpenAI path at all or fall back to a
/// deterministic in-process responder (useful offline and in tests).
pub(crate) const OPENAI_API_KEY_VAR: &str = "OPENAI_API_KEY";

/// Overrides the rig completion model id. Falls back to the config file's
/// `[provider].model`, then [`openai::GPT_4O_MINI`].
const RIG_MODEL_VAR: &str = "HORIZON_RIG_MODEL";

/// Rig/OpenAI's own base-URL env var (already honored implicitly by
/// `openai::CompletionsClient::from_env()`); kept authoritative here too so
/// it wins over `[provider].base_url` in the config file, per Horizon's
/// "existing env vars keep working and win" precedence rule.
const OPENAI_BASE_URL_VAR: &str = "OPENAI_BASE_URL";

/// Overrides the path of the append-only agent event log (JSONL). Falls
/// back to a fixed path under the OS temp directory.
const EVENT_LOG_PATH_VAR: &str = "HORIZON_AGENT_EVENT_LOG";

/// Overrides the path of the DuckDB projection database used to replay
/// per-session rig history. Unset means no persisted memory: sessions
/// start with empty history and nothing is written.
const STATE_DB_PATH_VAR: &str = "HORIZON_AGENT_STATE_DB";

// --- built-in defaults for the `[agent]` tuning knobs ----------------------
//
// These were previously hardcoded constants scattered across the tool
// modules (see each field's doc comment for where). They're now the
// fallback used when the config file doesn't set the corresponding key —
// see `config.example.toml` at the repo root, which documents every one of
// them with its default.
//
// The two traversal caps keep the `cfg(test)` shrink they already had
// (see the `agent-tools-design.md` traversal cap tests) as a *separate*,
// always-compiled pair of constants: `default_fs_grep_max_bytes`/
// `default_fs_traversal_max_files` pick the test-shrunk value under
// `cfg(test)` so the existing cap-tripping tests keep exercising the cap
// without creating tens of thousands of files, while the *_PRODUCTION_DEFAULT
// constants stay the real numbers regardless of `cfg(test)` — so a test can
// still assert the example file documents the real production default (see
// `tests::parses_and_matches_the_example_config_file`).
const DEFAULT_BASH_TIMEOUT_DEFAULT_SECS: u64 = 120;
const DEFAULT_BASH_TIMEOUT_MAX_SECS: u64 = 600;
const DEFAULT_BASH_OUTPUT_CAP_CHARS: usize = 30_000;
const DEFAULT_BASH_DRAIN_GRACE_SECS: u64 = 2;
const DEFAULT_FS_READ_LINE_CAP: usize = 2000;
const DEFAULT_ITERATION_CAP: u32 = 25;
const DEFAULT_DOOM_LOOP_WINDOW: usize = 3;

const FS_GREP_MAX_BYTES_PRODUCTION_DEFAULT: u64 = 64 * 1024 * 1024;
const FS_TRAVERSAL_MAX_FILES_PRODUCTION_DEFAULT: usize = 20_000;
#[cfg(test)]
const FS_GREP_MAX_BYTES_TEST_DEFAULT: u64 = 1024;
#[cfg(test)]
const FS_TRAVERSAL_MAX_FILES_TEST_DEFAULT: usize = 20;

#[cfg(not(test))]
fn default_fs_grep_max_bytes() -> u64 {
    FS_GREP_MAX_BYTES_PRODUCTION_DEFAULT
}
#[cfg(test)]
fn default_fs_grep_max_bytes() -> u64 {
    FS_GREP_MAX_BYTES_TEST_DEFAULT
}

#[cfg(not(test))]
fn default_fs_traversal_max_files() -> usize {
    FS_TRAVERSAL_MAX_FILES_PRODUCTION_DEFAULT
}
#[cfg(test)]
fn default_fs_traversal_max_files() -> usize {
    FS_TRAVERSAL_MAX_FILES_TEST_DEFAULT
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentConfig {
    pub(crate) rig: RigAgentConfig,
    pub(crate) persistence: AgentPersistenceConfig,
    pub(crate) tools: AgentToolsConfig,
}

impl AgentConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            rig: RigAgentConfig::from_env(),
            persistence: AgentPersistenceConfig::from_env(),
            tools: AgentToolsConfig::from_env(),
        }
    }
}

/// Rig provider configuration: model/base-URL selection (`[provider]`, plus
/// the env vars above) and the turn-loop guard tuning (`[agent]`
/// `iteration_cap`/`doom_loop_window`) — see `providers::rig::session`'s
/// `TurnLoopGuard`, which this is threaded into unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RigAgentConfig {
    /// Whether `OPENAI_API_KEY` is set. When `false`, the rig provider
    /// answers with a deterministic fallback responder instead of calling
    /// OpenAI (see `providers::rig::completion::complete_rig_turn`).
    pub(crate) openai_enabled: bool,
    /// Completion model id passed to `rig_core`'s OpenAI client.
    pub(crate) model: String,
    /// Explicit base URL for the OpenAI client, if any. `None` means "use
    /// rig's own default" (`https://api.openai.com/v1`) — see
    /// `providers::rig::completion`'s client construction for how this is
    /// applied via the client builder's `.base_url(..)`.
    pub(crate) base_url: Option<String>,
    /// Consecutive-tool-turn iteration cap (`docs/agent-tools-design.md`,
    /// "Error Model and Loop Guards"). Was the hardcoded
    /// `TOOL_TURN_ITERATION_CAP` constant in `providers::rig::session`.
    pub(crate) iteration_cap: u32,
    /// Doom-loop fingerprint window size, same section of the design doc.
    /// Was the hardcoded `DOOM_LOOP_WINDOW` constant in
    /// `providers::rig::session`.
    pub(crate) doom_loop_window: usize,
}

impl Default for RigAgentConfig {
    fn default() -> Self {
        Self {
            openai_enabled: false,
            model: openai::GPT_4O_MINI.to_string(),
            base_url: None,
            iteration_cap: DEFAULT_ITERATION_CAP,
            doom_loop_window: DEFAULT_DOOM_LOOP_WINDOW,
        }
    }
}

impl RigAgentConfig {
    pub(crate) fn from_env() -> Self {
        Self::from_env_and_file(crate::config::load())
    }

    pub(crate) fn from_env_and_file(file: &RawConfig) -> Self {
        Self {
            openai_enabled: std::env::var_os(OPENAI_API_KEY_VAR).is_some(),
            model: resolve_model(
                std::env::var(RIG_MODEL_VAR).ok(),
                file.provider.model.clone(),
            ),
            base_url: resolve_base_url(
                std::env::var(OPENAI_BASE_URL_VAR).ok(),
                file.provider.base_url.clone(),
            ),
            iteration_cap: file.agent.iteration_cap.unwrap_or(DEFAULT_ITERATION_CAP),
            doom_loop_window: file
                .agent
                .doom_loop_window
                .unwrap_or(DEFAULT_DOOM_LOOP_WINDOW),
        }
    }
}

/// Pure precedence resolution for the rig model id: env var wins, then the
/// config file's `[provider].model`, then rig's own default model. Kept
/// free of I/O (env/file reads happen at the call site) so precedence is
/// unit-testable without mutating process environment — `cargo test` runs
/// tests in parallel within one process, so real env mutation in a test
/// would race every other test reading the same variable.
fn resolve_model(env_value: Option<String>, file_value: Option<String>) -> String {
    env_value
        .or(file_value)
        .unwrap_or_else(|| openai::GPT_4O_MINI.to_string())
}

/// Same precedence as [`resolve_model`], for the OpenAI base URL. `None`
/// means "let rig use its own default" — there is no Horizon-side default
/// URL to fall back to.
fn resolve_base_url(env_value: Option<String>, file_value: Option<String>) -> Option<String> {
    env_value.or(file_value)
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

/// `[agent]` tuning for the bash and fs tools — see each field's doc
/// comment for the tool module it replaces a hardcoded constant in.
/// `Copy` because it's cheap and gets stored on `tools::state::
/// ToolSessionState` and threaded onto the bash background thread
/// (`tools::bash::spawn`) alongside the `Send`-only cwd handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AgentToolsConfig {
    pub(crate) bash: BashToolConfig,
    pub(crate) fs: FsToolConfig,
}

impl Default for BashToolConfig {
    fn default() -> Self {
        AgentToolsConfig::default().bash
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BashToolConfig {
    /// Wall-clock timeout default, in seconds. Was `bash::exec`'s
    /// `DEFAULT_TIMEOUT_SECS`.
    pub(crate) timeout_default_secs: u64,
    /// Hard cap on the per-call `timeout_secs` override. Was `bash::exec`'s
    /// `MAX_TIMEOUT_SECS`.
    pub(crate) timeout_max_secs: u64,
    /// In-context output cap, in characters. Was `bash::output`'s
    /// `IN_CONTEXT_CAP_CHARS`.
    pub(crate) output_cap_chars: usize,
    /// Post-exit pipe-drain grace period, in seconds. Was `bash::exec`'s
    /// `DRAIN_GRACE`.
    pub(crate) drain_grace_secs: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FsToolConfig {
    /// Default number of lines `fs.read` returns when the caller doesn't
    /// pass `limit`. Was `fs::read`'s `DEFAULT_LIMIT`.
    pub(crate) read_line_cap: usize,
    /// Maximum total bytes `fs.grep` reads in one traversal. Was
    /// `fs::grep`'s `MAX_GREP_BYTES`.
    pub(crate) grep_max_bytes: u64,
    /// Maximum files a single `fs.glob`/`fs.grep` traversal visits. Was
    /// `fs::traverse`'s `MAX_VISITED_FILES`.
    pub(crate) traversal_max_files: usize,
}

impl Default for AgentToolsConfig {
    fn default() -> Self {
        Self::from_file(&RawConfig::default())
    }
}

impl AgentToolsConfig {
    pub(crate) fn from_env() -> Self {
        Self::from_file(crate::config::load())
    }

    fn from_file(file: &RawConfig) -> Self {
        Self {
            bash: BashToolConfig {
                timeout_default_secs: file
                    .agent
                    .bash_timeout_default_secs
                    .unwrap_or(DEFAULT_BASH_TIMEOUT_DEFAULT_SECS),
                timeout_max_secs: file
                    .agent
                    .bash_timeout_max_secs
                    .unwrap_or(DEFAULT_BASH_TIMEOUT_MAX_SECS),
                output_cap_chars: file
                    .agent
                    .bash_output_cap_chars
                    .unwrap_or(DEFAULT_BASH_OUTPUT_CAP_CHARS),
                drain_grace_secs: file
                    .agent
                    .bash_drain_grace_secs
                    .unwrap_or(DEFAULT_BASH_DRAIN_GRACE_SECS),
            },
            fs: FsToolConfig {
                read_line_cap: file
                    .agent
                    .fs_read_line_cap
                    .unwrap_or(DEFAULT_FS_READ_LINE_CAP),
                grep_max_bytes: file
                    .agent
                    .fs_grep_max_bytes
                    .unwrap_or_else(default_fs_grep_max_bytes),
                traversal_max_files: file
                    .agent
                    .fs_traversal_max_files
                    .unwrap_or_else(default_fs_traversal_max_files),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RawAgentConfig, RawProviderConfig};

    // --- precedence: env beats file beats built-in default -----------------

    #[test]
    fn model_prefers_env_over_file_over_default() {
        assert_eq!(
            resolve_model(
                Some("env-model".to_string()),
                Some("file-model".to_string())
            ),
            "env-model"
        );
        assert_eq!(
            resolve_model(None, Some("file-model".to_string())),
            "file-model"
        );
        assert_eq!(resolve_model(None, None), openai::GPT_4O_MINI);
    }

    #[test]
    fn base_url_prefers_env_over_file_and_is_none_by_default() {
        assert_eq!(
            resolve_base_url(
                Some("https://env.invalid".to_string()),
                Some("https://file.invalid".to_string())
            ),
            Some("https://env.invalid".to_string())
        );
        assert_eq!(
            resolve_base_url(None, Some("https://file.invalid".to_string())),
            Some("https://file.invalid".to_string())
        );
        assert_eq!(resolve_base_url(None, None), None);
    }

    #[test]
    fn rig_agent_config_reads_iteration_and_doom_loop_settings_from_file() {
        let file = RawConfig {
            agent: RawAgentConfig {
                iteration_cap: Some(7),
                doom_loop_window: Some(2),
                ..Default::default()
            },
            ..Default::default()
        };

        let config = RigAgentConfig::from_env_and_file(&file);

        assert_eq!(config.iteration_cap, 7);
        assert_eq!(config.doom_loop_window, 2);
    }

    #[test]
    fn rig_agent_config_falls_back_to_built_in_defaults_when_file_is_empty() {
        let config = RigAgentConfig::from_env_and_file(&RawConfig::default());

        assert_eq!(config.iteration_cap, DEFAULT_ITERATION_CAP);
        assert_eq!(config.doom_loop_window, DEFAULT_DOOM_LOOP_WINDOW);
        assert_eq!(config.base_url, None);
    }

    #[test]
    fn agent_tools_config_reads_every_knob_from_file() {
        let file = RawConfig {
            agent: RawAgentConfig {
                bash_timeout_default_secs: Some(1),
                bash_timeout_max_secs: Some(2),
                bash_output_cap_chars: Some(3),
                bash_drain_grace_secs: Some(4),
                fs_read_line_cap: Some(5),
                fs_grep_max_bytes: Some(6),
                fs_traversal_max_files: Some(7),
                ..Default::default()
            },
            ..Default::default()
        };

        let config = AgentToolsConfig::from_file(&file);

        assert_eq!(config.bash.timeout_default_secs, 1);
        assert_eq!(config.bash.timeout_max_secs, 2);
        assert_eq!(config.bash.output_cap_chars, 3);
        assert_eq!(config.bash.drain_grace_secs, 4);
        assert_eq!(config.fs.read_line_cap, 5);
        assert_eq!(config.fs.grep_max_bytes, 6);
        assert_eq!(config.fs.traversal_max_files, 7);
    }

    #[test]
    fn agent_tools_config_defaults_when_file_is_absent() {
        let config = AgentToolsConfig::default();

        assert_eq!(
            config.bash.timeout_default_secs,
            DEFAULT_BASH_TIMEOUT_DEFAULT_SECS
        );
        assert_eq!(config.bash.timeout_max_secs, DEFAULT_BASH_TIMEOUT_MAX_SECS);
        assert_eq!(config.bash.output_cap_chars, DEFAULT_BASH_OUTPUT_CAP_CHARS);
        assert_eq!(config.bash.drain_grace_secs, DEFAULT_BASH_DRAIN_GRACE_SECS);
        assert_eq!(config.fs.read_line_cap, DEFAULT_FS_READ_LINE_CAP);
    }

    // --- guards template drift: config.example.toml's [agent] values must --
    // --- match the real, non-test-shrunk built-in defaults ------------------

    #[test]
    fn parses_and_matches_the_example_config_file() {
        let example_path = concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.toml");
        let contents = std::fs::read_to_string(example_path)
            .expect("config.example.toml must exist at the repo root");
        let parsed: RawConfig =
            toml::from_str(&contents).expect("config.example.toml must be valid TOML");

        assert_eq!(
            parsed.agent.bash_timeout_default_secs,
            Some(DEFAULT_BASH_TIMEOUT_DEFAULT_SECS),
            "config.example.toml's bash_timeout_default_secs has drifted from the built-in default"
        );
        assert_eq!(
            parsed.agent.bash_timeout_max_secs,
            Some(DEFAULT_BASH_TIMEOUT_MAX_SECS)
        );
        assert_eq!(
            parsed.agent.bash_output_cap_chars,
            Some(DEFAULT_BASH_OUTPUT_CAP_CHARS)
        );
        assert_eq!(
            parsed.agent.bash_drain_grace_secs,
            Some(DEFAULT_BASH_DRAIN_GRACE_SECS)
        );
        assert_eq!(
            parsed.agent.fs_read_line_cap,
            Some(DEFAULT_FS_READ_LINE_CAP)
        );
        assert_eq!(
            parsed.agent.fs_grep_max_bytes,
            Some(FS_GREP_MAX_BYTES_PRODUCTION_DEFAULT),
            "config.example.toml documents the real production default, not the cfg(test) shrink"
        );
        assert_eq!(
            parsed.agent.fs_traversal_max_files,
            Some(FS_TRAVERSAL_MAX_FILES_PRODUCTION_DEFAULT)
        );
        assert_eq!(parsed.agent.iteration_cap, Some(DEFAULT_ITERATION_CAP));
        assert_eq!(
            parsed.agent.doom_loop_window,
            Some(DEFAULT_DOOM_LOOP_WINDOW)
        );

        // [provider]/[keybindings]/[theme] ship commented out in the example
        // (they layer on top of other defaults, not simple constants) —
        // confirms the whole file still parses with them absent, rather
        // than only the [agent] section being exercised above.
        assert_eq!(parsed.provider, RawProviderConfig::default());
        assert!(parsed.keybindings.is_empty());
        assert!(parsed.theme.is_empty());
    }
}
