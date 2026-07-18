//! Agent provider, tool, and persistence configuration.
//!
//! Per `docs/agent-tools-design.md`'s "Config" section and `AGENTS.md`'s
//! "Configuration" section: values here flow from (in precedence order)
//! environment variables, then Horizon's single config file (read by the
//! caller, never this crate -- see below), then a built-in default.
//! Secrets (`OPENAI_API_KEY`) are environment-only and never read from the
//! config file. This module is the single place that names the env vars
//! and built-in defaults; keep it authoritative rather than duplicating
//! them elsewhere.
//!
//! **Crate boundary.** This crate has no dependency on `horizon-config` (or
//! on Horizon) and so cannot parse or locate Horizon's config file itself.
//! As of the 2026-07-18 config-narrowing wave, the only file-sourced
//! values this crate's config still varies on are `[provider]`
//! `model`/`base_url` -- [`AgentConfig::from_env_and_provider`] takes
//! those two as plain `Option<String>` arguments; the caller
//! (`horizon-sessiond`'s `main`, which owns the real `horizon_config::
//! load()` call) resolves them from the file first. Every other former
//! `[agent]`/`[provider]` file knob (tool caps, turn-loop guard
//! thresholds, stream-flush cadence, history/instructions budgets,
//! `temperature`/`max_tokens`) is now a fixed built-in constant -- see
//! each `DEFAULT_*` constant below. `event_log_path`/`state_db_path`
//! similarly lost their file keys; `HORIZON_AGENT_EVENT_LOG`/
//! `HORIZON_AGENT_STATE_DB` plus the XDG-based built-in default remain the
//! only override path.

use std::path::PathBuf;

use rig_core::providers::openai;

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
/// back to `$XDG_DATA_HOME/horizon/agent-events.jsonl` (see
/// [`default_event_log_path_from`]). A leading `~/` is expanded against
/// `$HOME`.
const EVENT_LOG_PATH_VAR: &str = "HORIZON_AGENT_EVENT_LOG";

/// Overrides the path of the DuckDB projection database used to replay
/// per-session rig history. The projection always runs now (see
/// [`default_state_db_path_from`]) -- there is no "unset = disabled"
/// state to opt into; setting this just relocates the file. A leading
/// `~/` is expanded against `$HOME`, same as `event_log_path` above.
const STATE_DB_PATH_VAR: &str = "HORIZON_AGENT_STATE_DB";

/// `$HOME`, read once per resolution call to expand a leading `~/` in a
/// path-typed env value (`HORIZON_AGENT_EVENT_LOG`/`HORIZON_AGENT_STATE_DB`)
/// and, absent `$XDG_DATA_HOME`, to build the event log's and DuckDB
/// projection's default paths — see [`default_event_log_path_from`] and
/// [`default_state_db_path_from`].
const HOME_VAR: &str = "HOME";

/// XDG base-directory spec's data-home var, used for both the event log's
/// and the DuckDB projection's built-in default paths — see
/// [`default_event_log_path_from`] and [`default_state_db_path_from`].
const XDG_DATA_HOME_VAR: &str = "XDG_DATA_HOME";

// --- built-in defaults for the former `[agent]` tuning knobs ---------------
//
// These used to be file-configurable (a `[agent]` section in Horizon's
// config file); the 2026-07-18 config-narrowing wave retired that whole
// section (see the module doc), so every one of them is now a fixed
// built-in constant with no override path at all except the two explicitly
// noted otherwise (`event_log_path`/`state_db_path`, still env-overridable
// via `HORIZON_AGENT_EVENT_LOG`/`HORIZON_AGENT_STATE_DB`) — see
// `config.example.toml` at the repo root for the user-facing summary of
// what's still configurable at all.
//
// The two traversal caps keep the `cfg(test)` shrink they already had
// (see the `agent-tools-design.md` traversal cap tests) as a *separate*,
// always-compiled pair of constants: `default_fs_grep_max_bytes`/
// `default_fs_traversal_max_files` pick the test-shrunk value under
// `cfg(test)` so the existing cap-tripping tests keep exercising the cap
// without creating tens of thousands of files, while the *_PRODUCTION_DEFAULT
// constants stay the real numbers regardless of `cfg(test)`.
pub(crate) const DEFAULT_BASH_TIMEOUT_DEFAULT_SECS: u64 = 120;
pub(crate) const DEFAULT_BASH_TIMEOUT_MAX_SECS: u64 = 600;
pub(crate) const DEFAULT_BASH_OUTPUT_CAP_CHARS: usize = 30_000;
pub(crate) const DEFAULT_BASH_DRAIN_GRACE_SECS: u64 = 2;
pub(crate) const DEFAULT_FS_READ_LINE_CAP: usize = 2000;
/// Default number of matches `fs.grep` returns when a call doesn't pass its
/// own `limit`. Was `fs::grep`'s `DEFAULT_LIMIT`.
pub(crate) const DEFAULT_FS_GREP_RESULT_LIMIT: usize = 100;
/// Same idea as [`DEFAULT_FS_GREP_RESULT_LIMIT`], for `fs.glob`. Was
/// `fs::glob`'s `DEFAULT_LIMIT`.
pub(crate) const DEFAULT_FS_GLOB_RESULT_LIMIT: usize = 200;
/// Consecutive-tool-driven-turn safety-net cap
/// (`docs/agent-tools-design.md`'s "Error Model and Loop Guards"). Fixed at
/// 100 (`docs/issues/002-agent-iteration-cap-halts-real-work.md`'s
/// resolution, 2026-07-18): the previous default of 25 fired on ordinary
/// agentic work well before anything resembling a real runaway loop. Not
/// configurable at all any more -- the `[agent] iteration_cap` key it used
/// to read (before that same resolution) was removed from the config
/// schema entirely in the 2026-07-18 config-narrowing wave. `pub` so
/// `src/agent/turns/receipt.rs` can render the exact number in a
/// guard-halted turn's paused receipt text without duplicating it.
pub const DEFAULT_ITERATION_CAP: u32 = 100;
/// Doom-loop (identical-consecutive-tool-result) window, same section of
/// the design doc and same fixed-not-configurable treatment as
/// [`DEFAULT_ITERATION_CAP`]: fixed at 5 (was 3), no longer configurable
/// via `[agent] doom_loop_window`.
pub const DEFAULT_DOOM_LOOP_WINDOW: usize = 5;
/// Was `providers::rig::stream`'s `STREAM_FLUSH_INTERVAL`.
pub(crate) const DEFAULT_STREAM_FLUSH_INTERVAL_MS: u64 = 100;
/// Was `providers::rig::stream`'s `STREAM_FLUSH_CHARS`.
pub(crate) const DEFAULT_STREAM_FLUSH_CHARS: usize = 320;
/// Token budget for the conversation history sent to the provider on each
/// turn (`providers::rig::completion`'s `history_token_window_policy`,
/// applying `rig_memory::TokenWindowMemory`). 60,000 is conservative rather
/// than tight against any particular model's real context window: the
/// counter behind it (`rig_memory::HeuristicTokenCounter`'s OpenAI preset)
/// approximates tokens from UTF-8 byte lengths and can over- or
/// under-count by up to ~30% on real content, and the budget only bounds
/// history -- it leaves headroom on top for the system prompt, the new
/// turn's prompt, and the tool responses a turn is still free to request
/// after this history is sent. Applies regardless of provider, but was
/// chosen with Horizon's current provider in mind: an OpenAI-compatible
/// endpoint fronting Kimi.
pub(crate) const DEFAULT_HISTORY_TOKEN_BUDGET: usize = 60_000;
/// Character cap on the composed "Repository instructions" system-prompt
/// section built by `instructions::extra_sections` from `AGENTS.md`/
/// `CLAUDE.md` files found while walking from the session's working
/// directory up to the repository root. 24,000 characters is roughly
/// 4x the size of this repository's own `AGENTS.md` (~6KB at the time this
/// default was chosen), generous enough for a normal single-file repo
/// instruction set while still bounding a worst case (a deep monorepo with
/// an instruction file at every level) well clear of
/// [`DEFAULT_HISTORY_TOKEN_BUDGET`]'s headroom -- at a roughly 4-characters-
/// per-token rule of thumb this is ~6,000 tokens, a fraction of the 60,000
/// token history budget, leaving the rest for conversation history and the
/// turn's own prompt.
pub(crate) const DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS: usize = 24_000;

pub const FS_GREP_MAX_BYTES_PRODUCTION_DEFAULT: u64 = 64 * 1024 * 1024;
pub const FS_TRAVERSAL_MAX_FILES_PRODUCTION_DEFAULT: usize = 20_000;
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

#[derive(Clone, Debug, PartialEq)]
pub struct AgentConfig {
    pub rig: RigAgentConfig,
    pub persistence: AgentPersistenceConfig,
    pub tools: AgentToolsConfig,
}

impl AgentConfig {
    /// Builds this crate's whole config from environment variables plus the
    /// two `[provider]` values the caller already resolved from Horizon's
    /// config file — see the module doc for why this crate can't resolve
    /// them itself. `None` for either means the file didn't set it, same as
    /// an absent key.
    pub fn from_env_and_provider(model: Option<String>, base_url: Option<String>) -> Self {
        Self {
            rig: RigAgentConfig::from_env_and_provider(model, base_url),
            persistence: AgentPersistenceConfig::from_env(),
            tools: AgentToolsConfig::default(),
        }
    }
}

/// Rig provider configuration: model/base-URL selection (`[provider]`, plus
/// the env vars above), the turn-loop guard's fixed thresholds
/// (`iteration_cap`/`doom_loop_window`, always [`DEFAULT_ITERATION_CAP`]/
/// [`DEFAULT_DOOM_LOOP_WINDOW`] -- see `providers::rig::session`'s
/// `TurnLoopGuard`, which this is threaded into unchanged) — and the
/// streamed-delta coalescing cadence (always [`DEFAULT_STREAM_FLUSH_INTERVAL_MS`]/
/// [`DEFAULT_STREAM_FLUSH_CHARS`]) used by `providers::rig::stream`.
#[derive(Clone, Debug, PartialEq)]
pub struct RigAgentConfig {
    /// Whether `OPENAI_API_KEY` is set. When `false`, the rig provider
    /// answers with a deterministic fallback responder instead of calling
    /// OpenAI (see `providers::rig::completion::complete_rig_turn`).
    pub openai_enabled: bool,
    /// Completion model id passed to `rig_core`'s OpenAI client.
    pub model: String,
    /// Explicit base URL for the OpenAI client, if any. `None` means "use
    /// rig's own default" (`https://api.openai.com/v1`) — see
    /// `providers::rig::completion`'s client construction for how this is
    /// applied via the client builder's `.base_url(..)`.
    pub base_url: Option<String>,
    /// Consecutive-tool-turn iteration cap (`docs/agent-tools-design.md`,
    /// "Error Model and Loop Guards"). Always [`DEFAULT_ITERATION_CAP`] --
    /// kept as a field (rather than having `providers::rig::session` read
    /// the constant directly) so tests can still construct a
    /// `RigAgentConfig` with a small cap to exercise the guard without
    /// looping to the real threshold.
    pub iteration_cap: u32,
    /// Doom-loop fingerprint window size, same section of the design doc
    /// and same fixed-not-configurable treatment as `iteration_cap` --
    /// always [`DEFAULT_DOOM_LOOP_WINDOW`].
    pub doom_loop_window: usize,
    /// How often, in milliseconds, streamed deltas are coalesced into an
    /// emitted event. Was `providers::rig::stream`'s
    /// `STREAM_FLUSH_INTERVAL`. Always [`DEFAULT_STREAM_FLUSH_INTERVAL_MS`].
    pub stream_flush_interval_ms: u64,
    /// Character count that forces an early flush ahead of the interval
    /// above. Was `providers::rig::stream`'s `STREAM_FLUSH_CHARS`. Always
    /// [`DEFAULT_STREAM_FLUSH_CHARS`].
    pub stream_flush_chars: usize,
    /// Token budget applied to the conversation history sent to the
    /// provider on each turn -- see [`DEFAULT_HISTORY_TOKEN_BUDGET`] for
    /// why 60,000 was chosen. Always active (no "0/unset disables
    /// windowing" escape hatch), matching this struct's other tuning
    /// knobs (`iteration_cap`, `doom_loop_window`, ...): always
    /// [`DEFAULT_HISTORY_TOKEN_BUDGET`]. This only shapes the *view* sent
    /// to the provider (`providers::rig::completion::
    /// windowed_history_for_request`) -- `rig_history` itself, and the
    /// DuckDB-persisted event log it's rebuilt from, are never truncated.
    pub history_token_budget: usize,
    /// Character cap applied to the composed "Repository instructions"
    /// system-prompt section -- see
    /// [`DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS`] for why 24,000 was
    /// chosen. Always that constant. Read by `providers::rig::session::
    /// spawn_rig_session` when it builds that section via
    /// `instructions::extra_sections`.
    pub repository_instructions_cap_chars: usize,
    /// Restricts which tool ids `providers::rig::completion::
    /// rig_tool_definitions` advertises to the provider. `None` (the only
    /// value [`Self::from_env_and_provider`] itself ever produces -- this
    /// field is process-wide config, not per-session) means "no
    /// restriction, every tool in `tools::definitions()`" -- current
    /// behavior, unchanged. This back-compatible extension point
    /// (`docs/research/agent-prompting.md` Part 2.5) now has its first
    /// consumer: `providers::rig::Provider::start_session` derives a
    /// per-session `RigAgentConfig` with `Some(..)` here when the session
    /// has a role that restricts tools (`roles::RoleDefinition::
    /// allowed_tool_ids`).
    pub allowed_tool_ids: Option<Vec<String>>,
}

impl Default for RigAgentConfig {
    fn default() -> Self {
        Self {
            openai_enabled: false,
            model: openai::GPT_4O_MINI.to_string(),
            base_url: None,
            iteration_cap: DEFAULT_ITERATION_CAP,
            doom_loop_window: DEFAULT_DOOM_LOOP_WINDOW,
            stream_flush_interval_ms: DEFAULT_STREAM_FLUSH_INTERVAL_MS,
            stream_flush_chars: DEFAULT_STREAM_FLUSH_CHARS,
            history_token_budget: DEFAULT_HISTORY_TOKEN_BUDGET,
            repository_instructions_cap_chars: DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS,
            allowed_tool_ids: None,
        }
    }
}

impl RigAgentConfig {
    pub fn from_env_and_provider(model: Option<String>, base_url: Option<String>) -> Self {
        Self {
            openai_enabled: std::env::var_os(OPENAI_API_KEY_VAR).is_some(),
            model: resolve_model(std::env::var(RIG_MODEL_VAR).ok(), model),
            base_url: resolve_base_url(std::env::var(OPENAI_BASE_URL_VAR).ok(), base_url),
            iteration_cap: DEFAULT_ITERATION_CAP,
            doom_loop_window: DEFAULT_DOOM_LOOP_WINDOW,
            stream_flush_interval_ms: DEFAULT_STREAM_FLUSH_INTERVAL_MS,
            stream_flush_chars: DEFAULT_STREAM_FLUSH_CHARS,
            history_token_budget: DEFAULT_HISTORY_TOKEN_BUDGET,
            repository_instructions_cap_chars: DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS,
            allowed_tool_ids: None,
        }
    }
}

/// Pure precedence resolution for the rig model id: env var wins, then the
/// config file's `[provider].model` (already resolved by the caller — see
/// the module doc), then rig's own default model. Kept free of I/O (env
/// reads happen at the call site) so precedence is unit-testable without
/// mutating process environment — `cargo test` runs tests in parallel
/// within one process, so real env mutation in a test would race every
/// other test reading the same variable.
fn resolve_model(env_value: Option<String>, provider_value: Option<String>) -> String {
    env_value
        .or(provider_value)
        .unwrap_or_else(|| openai::GPT_4O_MINI.to_string())
}

/// Same precedence as [`resolve_model`], for the OpenAI base URL. `None`
/// means "let rig use its own default" — there is no Horizon-side default
/// URL to fall back to.
fn resolve_base_url(env_value: Option<String>, provider_value: Option<String>) -> Option<String> {
    env_value.or(provider_value)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPersistenceConfig {
    pub event_log_path: PathBuf,
    pub duckdb_path: Option<PathBuf>,
}

impl AgentPersistenceConfig {
    /// No file input any more (see the module doc): `event_log_path`/
    /// `state_db_path` lost their `[agent]` file keys in the 2026-07-18
    /// config-narrowing wave, leaving `HORIZON_AGENT_EVENT_LOG`/
    /// `HORIZON_AGENT_STATE_DB` plus the XDG-based built-in default as the
    /// only override path.
    pub fn from_env() -> Self {
        let home = std::env::var(HOME_VAR).ok();
        let xdg_data_home = std::env::var(XDG_DATA_HOME_VAR).ok();
        Self {
            event_log_path: resolve_event_log_path(
                std::env::var(EVENT_LOG_PATH_VAR).ok(),
                xdg_data_home.clone(),
                home.clone(),
            ),
            duckdb_path: resolve_state_db_path(
                std::env::var(STATE_DB_PATH_VAR).ok(),
                xdg_data_home,
                home,
            ),
        }
    }
}

/// Pure precedence resolution for the event log path: `HORIZON_AGENT_EVENT_LOG`
/// wins, then [`default_event_log_path_from`]'s XDG-based built-in default.
/// An env value gets a leading `~/` expanded against `home` (see
/// [`expand_tilde`]). Kept free of I/O (the env read happens at the call
/// site) for the same testability reason as [`resolve_model`].
pub(crate) fn resolve_event_log_path(
    env_value: Option<String>,
    xdg_data_home: Option<String>,
    home: Option<String>,
) -> PathBuf {
    env_value
        .map(|value| expand_tilde(&value, home.as_deref()))
        .unwrap_or_else(|| default_event_log_path_from(xdg_data_home, home))
}

/// Resolves the `horizon` data directory shared by the event log's and the
/// DuckDB projection's built-in defaults: `$XDG_DATA_HOME`, falling back
/// to `~/.local/share` when `XDG_DATA_HOME` is unset or empty, and further
/// to the OS temp dir if even `$HOME` is unset. Factored out of
/// [`default_event_log_path_from`] so [`default_state_db_path_from`]
/// mirrors its exact resolution shape instead of duplicating it.
fn agent_data_home_from(xdg_data_home: Option<String>, home: Option<String>) -> PathBuf {
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    match non_empty(xdg_data_home) {
        Some(dir) => PathBuf::from(dir),
        None => match non_empty(home) {
            Some(home) => PathBuf::from(home).join(".local").join("share"),
            None => std::env::temp_dir(),
        },
    }
}

/// The event log's built-in default when `HORIZON_AGENT_EVENT_LOG` doesn't
/// set a path: `$XDG_DATA_HOME/horizon/agent-events.jsonl`, falling back
/// to `~/.local/share/horizon/agent-events.jsonl` when `XDG_DATA_HOME` is
/// unset or empty, and further to the OS temp dir (namespaced under a
/// `horizon` subdirectory, so it doesn't collide with unrelated temp
/// files) if even `$HOME` is unset. Durable across reboots in the common
/// case — unlike the OS temp dir this replaced, which contradicted the
/// event log's role as the source of truth for agent session history (see
/// `persistence`). The writer (`persistence::event_log::writer`) already
/// creates the path's parent directories on first write, so this can name
/// a path that doesn't exist yet.
pub(crate) fn default_event_log_path_from(
    xdg_data_home: Option<String>,
    home: Option<String>,
) -> PathBuf {
    agent_data_home_from(xdg_data_home, home)
        .join("horizon")
        .join("agent-events.jsonl")
}

/// The DuckDB projection's built-in default when `HORIZON_AGENT_STATE_DB`
/// doesn't set a path: `$XDG_DATA_HOME/horizon/agent-state.duckdb`,
/// mirroring [`default_event_log_path_from`]'s exact fallback chain (same
/// `$XDG_DATA_HOME` > `~/.local/share` > OS temp dir chain via
/// [`agent_data_home_from`]), just under a different filename. The
/// projection has no "unset = disabled" state any more: it is a
/// rebuildable, non-authoritative derived view of the JSONL log (see
/// `docs/agent-duckdb-state-design.md` and the `agent-inspect` skill), so
/// there is no meaningful reason to leave it off by default. `Store::open`
/// (`persistence::projection::duckdb`) creates the path's parent
/// directories on first use, same as the event log's writer.
pub(crate) fn default_state_db_path_from(
    xdg_data_home: Option<String>,
    home: Option<String>,
) -> PathBuf {
    agent_data_home_from(xdg_data_home, home)
        .join("horizon")
        .join("agent-state.duckdb")
}

/// Same precedence as [`resolve_event_log_path`], for the DuckDB state
/// path: `HORIZON_AGENT_STATE_DB` wins, then [`default_state_db_path_from`]'s
/// XDG-based built-in default. Same tilde-expansion treatment as
/// `resolve_event_log_path`. Keeps returning `Option<PathBuf>` (it now
/// always resolves to `Some` in practice) rather than switching to a bare
/// `PathBuf`, so [`AgentPersistenceConfig::duckdb_path`]'s existing
/// `Option<PathBuf>` shape -- and every `if let Some(duckdb_path) = ...`
/// built on it (e.g. `horizon-sessiond`'s startup rebuild) -- doesn't need
/// to change shape along with this default.
pub(crate) fn resolve_state_db_path(
    env_value: Option<String>,
    xdg_data_home: Option<String>,
    home: Option<String>,
) -> Option<PathBuf> {
    Some(
        env_value
            .map(|value| expand_tilde(&value, home.as_deref()))
            .unwrap_or_else(|| default_state_db_path_from(xdg_data_home, home)),
    )
}

/// Expands a leading `~/` in a path-typed env value against `home`,
/// mirroring shell tilde-expansion for the common case
/// (`HORIZON_AGENT_EVENT_LOG`/`HORIZON_AGENT_STATE_DB` above). A value
/// without a leading `~/` (including a bare `~`) passes through unchanged,
/// as does a `~/`-prefixed value when `home` is `None` or empty — there
/// being nothing to expand it against. Takes `home` as a parameter rather
/// than reading `$HOME` itself so callers stay unit-testable without
/// mutating process environment — see [`resolve_model`]'s doc comment for
/// why. A duplicate of Horizon's own `crate::config::expand_tilde` (this
/// crate can't depend on that module — see the module doc); kept in sync
/// by inspection since it's a small, stable helper.
fn expand_tilde(value: &str, home: Option<&str>) -> PathBuf {
    match value.strip_prefix("~/") {
        Some(rest) => match home.filter(|home| !home.is_empty()) {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(value),
        },
        None => PathBuf::from(value),
    }
}

/// Former `[agent]` tuning for the bash and fs tools, now built entirely
/// from fixed constants (see the module doc) -- see each field's doc
/// comment for the tool module it replaces a hardcoded constant in.
/// `Copy` because it's cheap and gets stored on `tools::state::
/// ToolSessionState` and threaded onto the bash background thread
/// (`tools::bash::spawn`) alongside the `Send`-only cwd handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentToolsConfig {
    pub bash: BashToolConfig,
    pub fs: FsToolConfig,
}

impl Default for BashToolConfig {
    fn default() -> Self {
        AgentToolsConfig::default().bash
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BashToolConfig {
    /// Wall-clock timeout default, in seconds. Was `bash::exec`'s
    /// `DEFAULT_TIMEOUT_SECS`.
    pub timeout_default_secs: u64,
    /// Hard cap on the per-call `timeout_secs` override. Was `bash::exec`'s
    /// `MAX_TIMEOUT_SECS`.
    pub timeout_max_secs: u64,
    /// In-context output cap, in characters. Was `bash::output`'s
    /// `IN_CONTEXT_CAP_CHARS`.
    pub output_cap_chars: usize,
    /// Post-exit pipe-drain grace period, in seconds. Was `bash::exec`'s
    /// `DRAIN_GRACE`.
    pub drain_grace_secs: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FsToolConfig {
    /// Default number of lines `fs.read` returns when the caller doesn't
    /// pass `limit`. Was `fs::read`'s `DEFAULT_LIMIT`.
    pub read_line_cap: usize,
    /// Maximum total bytes `fs.grep` reads in one traversal. Was
    /// `fs::grep`'s `MAX_GREP_BYTES`.
    pub grep_max_bytes: u64,
    /// Maximum files a single `fs.glob`/`fs.grep` traversal visits. Was
    /// `fs::traverse`'s `MAX_VISITED_FILES`.
    pub traversal_max_files: usize,
    /// Default number of matches `fs.grep` *returns* when a call doesn't
    /// pass its own `limit` — distinct from `grep_max_bytes`/
    /// `traversal_max_files` above, which cap how much of the tree a single
    /// traversal scans. Was `fs::grep`'s `DEFAULT_LIMIT`.
    pub grep_result_limit: usize,
    /// Same idea as `grep_result_limit`, for `fs.glob`. Was `fs::glob`'s
    /// `DEFAULT_LIMIT`.
    pub glob_result_limit: usize,
}

impl Default for AgentToolsConfig {
    fn default() -> Self {
        Self {
            bash: BashToolConfig {
                timeout_default_secs: DEFAULT_BASH_TIMEOUT_DEFAULT_SECS,
                timeout_max_secs: DEFAULT_BASH_TIMEOUT_MAX_SECS,
                output_cap_chars: DEFAULT_BASH_OUTPUT_CAP_CHARS,
                drain_grace_secs: DEFAULT_BASH_DRAIN_GRACE_SECS,
            },
            fs: FsToolConfig {
                read_line_cap: DEFAULT_FS_READ_LINE_CAP,
                grep_max_bytes: default_fs_grep_max_bytes(),
                traversal_max_files: default_fs_traversal_max_files(),
                grep_result_limit: DEFAULT_FS_GREP_RESULT_LIMIT,
                glob_result_limit: DEFAULT_FS_GLOB_RESULT_LIMIT,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- precedence: env beats the resolved provider value beats built-in
    // default -----------------------------------------------------------

    #[test]
    fn model_prefers_env_over_provider_over_default() {
        assert_eq!(
            resolve_model(
                Some("env-model".to_string()),
                Some("provider-model".to_string())
            ),
            "env-model"
        );
        assert_eq!(
            resolve_model(None, Some("provider-model".to_string())),
            "provider-model"
        );
        assert_eq!(resolve_model(None, None), openai::GPT_4O_MINI);
    }

    #[test]
    fn base_url_prefers_env_over_provider_and_is_none_by_default() {
        assert_eq!(
            resolve_base_url(
                Some("https://env.invalid".to_string()),
                Some("https://provider.invalid".to_string())
            ),
            Some("https://env.invalid".to_string())
        );
        assert_eq!(
            resolve_base_url(None, Some("https://provider.invalid".to_string())),
            Some("https://provider.invalid".to_string())
        );
        assert_eq!(resolve_base_url(None, None), None);
    }

    #[test]
    fn rig_agent_config_falls_back_to_built_in_defaults_when_provider_values_are_none() {
        let config = RigAgentConfig::from_env_and_provider(None, None);

        assert_eq!(config.iteration_cap, DEFAULT_ITERATION_CAP);
        assert_eq!(config.doom_loop_window, DEFAULT_DOOM_LOOP_WINDOW);
        assert_eq!(config.base_url, None);
        assert_eq!(
            config.stream_flush_interval_ms,
            DEFAULT_STREAM_FLUSH_INTERVAL_MS
        );
        assert_eq!(config.stream_flush_chars, DEFAULT_STREAM_FLUSH_CHARS);
        assert_eq!(config.history_token_budget, DEFAULT_HISTORY_TOKEN_BUDGET);
        assert_eq!(
            config.repository_instructions_cap_chars,
            DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS
        );
        assert_eq!(config.allowed_tool_ids, None);
    }

    #[test]
    fn rig_agent_config_reads_model_and_base_url_from_the_resolved_provider_values() {
        let config = RigAgentConfig::from_env_and_provider(
            Some("provider-model".to_string()),
            Some("https://provider.invalid".to_string()),
        );

        assert_eq!(config.model, "provider-model");
        assert_eq!(
            config.base_url,
            Some("https://provider.invalid".to_string())
        );
    }

    #[test]
    fn event_log_path_prefers_env_over_default() {
        assert_eq!(
            resolve_event_log_path(
                Some("/env/log.jsonl".to_string()),
                Some("/xdg/data".to_string()),
                Some("/home/user".to_string()),
            ),
            PathBuf::from("/env/log.jsonl")
        );
        assert_eq!(
            resolve_event_log_path(None, Some("/xdg/data".to_string()), None),
            PathBuf::from("/xdg/data/horizon/agent-events.jsonl")
        );
    }

    #[test]
    fn event_log_path_defaults_to_xdg_data_home_when_env_is_unset() {
        assert_eq!(
            default_event_log_path_from(
                Some("/xdg/data".to_string()),
                Some("/home/user".to_string())
            ),
            PathBuf::from("/xdg/data/horizon/agent-events.jsonl")
        );
    }

    #[test]
    fn event_log_path_falls_back_to_home_dot_local_share_without_xdg_data_home() {
        assert_eq!(
            default_event_log_path_from(None, Some("/home/user".to_string())),
            PathBuf::from("/home/user/.local/share/horizon/agent-events.jsonl")
        );
        // An empty (but present) XDG_DATA_HOME is treated the same as unset.
        assert_eq!(
            default_event_log_path_from(Some(String::new()), Some("/home/user".to_string())),
            PathBuf::from("/home/user/.local/share/horizon/agent-events.jsonl")
        );
    }

    #[test]
    fn event_log_path_falls_back_to_temp_dir_when_home_and_xdg_data_home_are_both_unset() {
        assert_eq!(
            default_event_log_path_from(None, None),
            std::env::temp_dir()
                .join("horizon")
                .join("agent-events.jsonl")
        );
    }

    #[test]
    fn event_log_path_expands_leading_tilde_from_env_source() {
        assert_eq!(
            resolve_event_log_path(
                Some("~/logs/agent-events.jsonl".to_string()),
                None,
                Some("/home/user".to_string()),
            ),
            PathBuf::from("/home/user/logs/agent-events.jsonl"),
            "HORIZON_AGENT_EVENT_LOG must expand a leading ~/ against HOME"
        );
    }

    #[test]
    fn state_db_path_prefers_env_over_default() {
        assert_eq!(
            resolve_state_db_path(
                Some("/env/state.duckdb".to_string()),
                Some("/xdg/data".to_string()),
                Some("/home/user".to_string()),
            ),
            Some(PathBuf::from("/env/state.duckdb"))
        );
        assert_eq!(
            resolve_state_db_path(None, Some("/xdg/data".to_string()), None),
            Some(PathBuf::from("/xdg/data/horizon/agent-state.duckdb"))
        );
    }

    #[test]
    fn state_db_path_defaults_to_xdg_data_home_when_env_is_unset() {
        assert_eq!(
            default_state_db_path_from(
                Some("/xdg/data".to_string()),
                Some("/home/user".to_string())
            ),
            PathBuf::from("/xdg/data/horizon/agent-state.duckdb")
        );
    }

    #[test]
    fn state_db_path_falls_back_to_home_dot_local_share_without_xdg_data_home() {
        assert_eq!(
            default_state_db_path_from(None, Some("/home/user".to_string())),
            PathBuf::from("/home/user/.local/share/horizon/agent-state.duckdb")
        );
        // An empty (but present) XDG_DATA_HOME is treated the same as unset.
        assert_eq!(
            default_state_db_path_from(Some(String::new()), Some("/home/user".to_string())),
            PathBuf::from("/home/user/.local/share/horizon/agent-state.duckdb")
        );
    }

    #[test]
    fn state_db_path_falls_back_to_temp_dir_when_home_and_xdg_data_home_are_both_unset() {
        assert_eq!(
            default_state_db_path_from(None, None),
            std::env::temp_dir()
                .join("horizon")
                .join("agent-state.duckdb")
        );
    }

    #[test]
    fn state_db_path_expands_leading_tilde_from_env_source() {
        assert_eq!(
            resolve_state_db_path(
                Some("~/state/agent.duckdb".to_string()),
                None,
                Some("/home/user".to_string()),
            ),
            Some(PathBuf::from("/home/user/state/agent.duckdb"))
        );
    }

    #[test]
    fn agent_tools_config_default_uses_built_in_constants() {
        let config = AgentToolsConfig::default();

        assert_eq!(
            config.bash.timeout_default_secs,
            DEFAULT_BASH_TIMEOUT_DEFAULT_SECS
        );
        assert_eq!(config.bash.timeout_max_secs, DEFAULT_BASH_TIMEOUT_MAX_SECS);
        assert_eq!(config.bash.output_cap_chars, DEFAULT_BASH_OUTPUT_CAP_CHARS);
        assert_eq!(config.bash.drain_grace_secs, DEFAULT_BASH_DRAIN_GRACE_SECS);
        assert_eq!(config.fs.read_line_cap, DEFAULT_FS_READ_LINE_CAP);
        assert_eq!(config.fs.grep_result_limit, DEFAULT_FS_GREP_RESULT_LIMIT);
        assert_eq!(config.fs.glob_result_limit, DEFAULT_FS_GLOB_RESULT_LIMIT);
    }
}
