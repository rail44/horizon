//! Horizon's seam onto the `horizon-agent` crate (the agent mechanism —
//! contract, providers, tools, persistence — split out per
//! `docs/agent-runtime-split-design.md` so it can be iterated on without
//! floem/workspace dependencies). This module re-exports the crate's public
//! modules under the same `agent::*` paths the rest of Horizon already
//! uses, and supplies the handful of things the crate deliberately can't
//! hold itself: conversion between the crate's own `SessionId` and
//! Horizon's shared `session::SessionId`, and the `workspace.snapshot` host
//! tool (`host_tools`), which needs `Workspace`.
//!
//! As of step 4, `horizon-agentd` is the only place a session's providers,
//! tools, and persistence actually run -- Horizon never opens its own copy
//! of the crate's config/persistence machinery any more (the config-file-
//! feeding glue that used to build an in-process `AgentConfig` is gone;
//! `host_tools::WorkspaceHostTools`, the in-process `HostTools` impl,
//! survives only as a `#[cfg(test)]` exercise of the seam -- see
//! `docs/agent-runtime-split-design.md`'s step 4 notes). Everything that's
//! still local to Horizon — views, the app-runtime wiring that
//! spawns/reconnects sessions — lives in `agent::view`,
//! `agent::agentd_client`, `agent::agentd_runtime`, and `app::runtime::agent`.

pub(crate) mod agentd_client;
pub(crate) mod agentd_runtime;
mod host_tools;
pub(crate) mod view;

pub(crate) use horizon_agent::{config, contract, frame, live, tools};

use contract::SessionId as AgentSessionId;

// Free functions instead of `From` impls: with `SessionId` extracted to
// `horizon-workspace`, both id types are foreign here and the orphan
// rule forbids the impls this shell used to define.
pub(crate) fn agent_session_id(id: crate::session::SessionId) -> AgentSessionId {
    AgentSessionId::from_uuid(id.as_uuid())
}

pub(crate) fn workspace_session_id(id: AgentSessionId) -> crate::session::SessionId {
    crate::session::SessionId::from_uuid(id.as_uuid())
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
            parsed.agent.history_token_budget,
            Some(config::DEFAULT_HISTORY_TOKEN_BUDGET)
        );
        assert_eq!(
            parsed.agent.repository_instructions_cap_chars,
            Some(config::DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS)
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
        // `event_log_path`/`state_db_path` ship commented out (both
        // built-in defaults depend on the environment -- `$XDG_DATA_HOME`/
        // `$HOME` -- so there's no single literal value worth showing
        // "live"), same as [provider]'s `model`/`base_url` below. Both
        // always resolve to a real path now -- the DuckDB projection has no
        // "unset = disabled" state to opt into any more (see
        // `resolve_state_db_path`'s doc comment); setting either just
        // relocates the file.
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
            config::resolve_state_db_path(None, parsed.agent.state_db_path.clone(), None, None),
            Some(config::default_state_db_path_from(None, None)),
            "config.example.toml's state_db_path must stay commented out (or, if ever made \
             live, resolve to the real built-in default -- not a placeholder path)"
        );

        // [provider]/[keybindings]/[theme]'s flat color overrides ship
        // commented out in the example (they layer on top of other
        // defaults, not simple constants) — confirms the whole file still
        // parses with them absent, rather than only the [agent] section
        // being exercised above. [theme.ansi] is active (see
        // `ui::theme::ansi`'s own drift guard for that table).
        assert_eq!(parsed.provider, crate::config::RawProviderConfig::default());
        assert!(parsed.keybindings.is_empty());
        assert!(parsed.theme.colors.is_empty());
    }
}
