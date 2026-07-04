//! Horizon's seam onto the `horizon-agent` crate (the agent mechanism —
//! contract, providers, tools, persistence — split out per
//! `docs/agent-runtime-split-design.md` so it can be iterated on without
//! floem/workspace dependencies). This module re-exports the crate's public
//! modules under the same `agent::*` paths the rest of Horizon already
//! uses, and supplies the handful of things the crate deliberately can't
//! hold itself: conversion between the crate's own `SessionId` and
//! Horizon's shared `session::SessionId`, feeding Horizon's config file into
//! the crate's config resolvers, and the `workspace.snapshot` host tool
//! (`host_tools`), which needs `Workspace`.
//!
//! Everything else — views, the app-runtime wiring that spawns sessions and
//! owns the event-log writer — stays local to Horizon; see `agent::view`
//! and `app::runtime::agent`.

pub(crate) mod agentd_client;
mod host_tools;
pub(crate) mod view;

pub(crate) use horizon_agent::{config, contract, frame, live, persistence, tools};
pub(crate) use host_tools::WorkspaceHostTools;

use contract::SessionId as AgentSessionId;

impl From<crate::session::SessionId> for AgentSessionId {
    fn from(id: crate::session::SessionId) -> Self {
        Self::from_uuid(id.as_uuid())
    }
}

impl From<AgentSessionId> for crate::session::SessionId {
    fn from(id: AgentSessionId) -> Self {
        Self::from_uuid(id.as_uuid())
    }
}

/// Resolves the crate's [`config::AgentConfig`] from environment variables
/// and Horizon's config file. The crate can't call `crate::config::load()`
/// itself (see its `config` module's doc comment on the crate boundary);
/// this is the one production call site, `app::state::AppState::new`.
pub(crate) fn load_agent_config() -> config::AgentConfig {
    config::AgentConfig::from_env_and_file(&agent_file_config_from_raw(crate::config::load()))
}

/// Converts Horizon's config-file schema into the crate's mirror of the
/// `[agent]`/`[provider]` sections it reads — see
/// [`config::AgentFileConfig`]'s doc comment for why the crate needs its
/// own copy of this shape instead of depending on `crate::config::RawConfig`
/// directly.
fn agent_file_config_from_raw(raw: &crate::config::RawConfig) -> config::AgentFileConfig {
    config::AgentFileConfig {
        agent: config::AgentFileAgentConfig {
            bash_timeout_default_secs: raw.agent.bash_timeout_default_secs,
            bash_timeout_max_secs: raw.agent.bash_timeout_max_secs,
            bash_output_cap_chars: raw.agent.bash_output_cap_chars,
            bash_drain_grace_secs: raw.agent.bash_drain_grace_secs,
            fs_read_line_cap: raw.agent.fs_read_line_cap,
            fs_grep_max_bytes: raw.agent.fs_grep_max_bytes,
            fs_traversal_max_files: raw.agent.fs_traversal_max_files,
            fs_grep_result_limit: raw.agent.fs_grep_result_limit,
            fs_glob_result_limit: raw.agent.fs_glob_result_limit,
            iteration_cap: raw.agent.iteration_cap,
            doom_loop_window: raw.agent.doom_loop_window,
            stream_flush_interval_ms: raw.agent.stream_flush_interval_ms,
            stream_flush_chars: raw.agent.stream_flush_chars,
            pane_status_tick_secs: raw.agent.pane_status_tick_secs,
            event_log_path: raw.agent.event_log_path.clone(),
            state_db_path: raw.agent.state_db_path.clone(),
        },
        provider: config::AgentFileProviderConfig {
            model: raw.provider.model.clone(),
            base_url: raw.provider.base_url.clone(),
            temperature: raw.provider.temperature,
            max_tokens: raw.provider.max_tokens,
        },
    }
}

/// How often, in seconds, the workspace pane header's agent turn-in-flight
/// elapsed-time display re-renders (`workspace::view::pane`'s
/// `schedule_tick`). Bridges to the crate's pure
/// [`config::pane_status_tick_secs`] with Horizon's config file value — see
/// that function's doc comment for why the crate can't read the file
/// itself.
pub(crate) fn pane_status_tick_secs() -> u64 {
    config::pane_status_tick_secs(crate::config::load().agent.pane_status_tick_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- guards template drift: config.example.toml's [agent] values must --
    // --- match the horizon-agent crate's built-in defaults ------------------
    //
    // Moved here (from the crate's own config.rs tests) at the split: this
    // needs both Horizon's real `RawConfig`/TOML parsing and the crate's
    // built-in default constants, so it has to live on the Horizon side of
    // the seam.
    #[test]
    fn parses_and_matches_the_example_config_file() {
        let example_path = concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.toml");
        let contents = std::fs::read_to_string(example_path)
            .expect("config.example.toml must exist at the repo root");
        let parsed: crate::config::RawConfig =
            toml::from_str(&contents).expect("config.example.toml must be valid TOML");

        assert_eq!(
            parsed.agent.bash_timeout_default_secs,
            Some(config::DEFAULT_BASH_TIMEOUT_DEFAULT_SECS),
            "config.example.toml's bash_timeout_default_secs has drifted from the built-in default"
        );
        assert_eq!(
            parsed.agent.bash_timeout_max_secs,
            Some(config::DEFAULT_BASH_TIMEOUT_MAX_SECS)
        );
        assert_eq!(
            parsed.agent.bash_output_cap_chars,
            Some(config::DEFAULT_BASH_OUTPUT_CAP_CHARS)
        );
        assert_eq!(
            parsed.agent.bash_drain_grace_secs,
            Some(config::DEFAULT_BASH_DRAIN_GRACE_SECS)
        );
        assert_eq!(
            parsed.agent.fs_read_line_cap,
            Some(config::DEFAULT_FS_READ_LINE_CAP)
        );
        assert_eq!(
            parsed.agent.fs_grep_max_bytes,
            Some(config::FS_GREP_MAX_BYTES_PRODUCTION_DEFAULT),
            "config.example.toml documents the real production default, not the cfg(test) shrink"
        );
        assert_eq!(
            parsed.agent.fs_traversal_max_files,
            Some(config::FS_TRAVERSAL_MAX_FILES_PRODUCTION_DEFAULT)
        );
        assert_eq!(
            parsed.agent.iteration_cap,
            Some(config::DEFAULT_ITERATION_CAP)
        );
        assert_eq!(
            parsed.agent.doom_loop_window,
            Some(config::DEFAULT_DOOM_LOOP_WINDOW)
        );
        assert_eq!(
            parsed.agent.fs_grep_result_limit,
            Some(config::DEFAULT_FS_GREP_RESULT_LIMIT)
        );
        assert_eq!(
            parsed.agent.fs_glob_result_limit,
            Some(config::DEFAULT_FS_GLOB_RESULT_LIMIT)
        );
        assert_eq!(
            parsed.agent.stream_flush_interval_ms,
            Some(config::DEFAULT_STREAM_FLUSH_INTERVAL_MS)
        );
        assert_eq!(
            parsed.agent.stream_flush_chars,
            Some(config::DEFAULT_STREAM_FLUSH_CHARS)
        );
        assert_eq!(
            parsed.agent.pane_status_tick_secs,
            Some(config::DEFAULT_PANE_STATUS_TICK_SECS)
        );
        // `event_log_path`/`state_db_path` ship commented out (the real
        // default depends on the environment -- `$XDG_DATA_HOME`/`$HOME` for
        // the former, "no persisted memory" for the latter -- so there's no
        // single literal value worth showing "live"), same as [provider]'s
        // `model`/`base_url` below.
        //
        // Strengthened past a plain `None` check: resolve the parsed value
        // through the same precedence function production code uses
        // (`resolve_event_log_path`/`resolve_state_db_path`, with no env
        // override, since this test is only about the *file* value) and
        // assert the result equals the real built-in default. This is what
        // actually catches an active placeholder line like
        // `event_log_path = "/path/to/horizon-agent-events.jsonl"`: such a
        // line would parse to `Some(..)`, resolve to that literal path
        // (after tilde-expansion, a no-op here), and mismatch the XDG-based
        // default computed by `default_event_log_path_from` -- whereas a
        // bare `None`-equality check on the raw field alone doesn't prove
        // anything about what the value resolves to if a maintainer ever
        // changes the placeholder without also updating a hardcoded `None`
        // assertion.
        assert_eq!(parsed.agent.event_log_path, None);
        assert_eq!(
            config::resolve_event_log_path(None, parsed.agent.event_log_path.clone(), None, None),
            config::default_event_log_path_from(None, None),
            "config.example.toml's event_log_path must stay commented out (or, if ever \
             made live, resolve to the real built-in default -- not a placeholder path)"
        );
        assert_eq!(parsed.agent.state_db_path, None);
        assert_eq!(
            config::resolve_state_db_path(None, parsed.agent.state_db_path.clone(), None),
            None,
            "config.example.toml's state_db_path must stay commented out -- the built-in \
             default is \"no persisted memory\" (None), not a placeholder path"
        );

        // [provider]/[keybindings]/[theme] ship commented out in the example
        // (they layer on top of other defaults, not simple constants) —
        // confirms the whole file still parses with them absent, rather
        // than only the [agent] section being exercised above.
        assert_eq!(parsed.provider, crate::config::RawProviderConfig::default());
        assert!(parsed.keybindings.is_empty());
        assert!(parsed.theme.is_empty());
        assert!(
            !parsed.agent.agentd,
            "agentd must stay commented out (default false) in the example file"
        );
    }
}
