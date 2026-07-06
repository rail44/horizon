//! Horizon's single configuration file.
//!
//! See `AGENTS.md`'s "Configuration" section for the user-facing summary
//! and `config.example.toml` at the repo root for every knob with its
//! default. This module owns locating and parsing the TOML file into a
//! [`RawConfig`]; `agent::config`, `app::keymap`, and `ui::theme` each read
//! the section relevant to them and apply their own env-var precedence and
//! built-in defaults on top (env var > this file > built-in default).
//!
//! Design choices:
//! - **One location, no layered merging.** Unlike tools that merge a
//!   system/user/project config chain, Horizon reads exactly one file:
//!   `$XDG_CONFIG_HOME/horizon/config.toml`, falling back to
//!   `~/.config/horizon/config.toml`, overridable wholesale via
//!   `HORIZON_CONFIG` (mainly for tests and for running more than one
//!   Horizon configuration side by side). Simpler to reason about at this
//!   project's size than a merged chain.
//! - **Never crash on a bad file.** A missing file is the common case
//!   (defaults apply, silently); a present-but-unparsable file falls back
//!   to defaults with a warning on stderr — the same "warn and skip, never
//!   fail startup" policy `app::keymap` and `ui::theme` apply per-entry to
//!   an unrecognized keybinding or theme color.
//! - **Applied at startup only.** Nothing here watches the file for
//!   changes; restart Horizon to pick up edits.
//! - **Secrets stay out.** Nothing under `[provider]` accepts an API key —
//!   `OPENAI_API_KEY` (and any future provider secret) is environment-only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

/// Overrides the config file path outright, bypassing the XDG/home lookup
/// below entirely. Primarily for tests and for running multiple Horizon
/// configurations side by side.
#[cfg(not(test))]
const CONFIG_PATH_VAR: &str = "HORIZON_CONFIG";
#[cfg(not(test))]
const XDG_CONFIG_HOME_VAR: &str = "XDG_CONFIG_HOME";
#[cfg(not(test))]
const HOME_VAR: &str = "HOME";

/// The config file's schema. Every field is optional (or an empty map) so
/// that a file which only sets a handful of knobs is valid, and so is no
/// file at all (`RawConfig::default()`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct RawConfig {
    pub(crate) agent: RawAgentConfig,
    pub(crate) provider: RawProviderConfig,
    pub(crate) terminal: RawTerminalConfig,
    pub(crate) ui: RawUiConfig,
    /// Key chord string (e.g. `"ctrl+shift+t"`) to `CommandId` string (e.g.
    /// `"new-terminal"`) — parsed and validated by `app::keymap`. Also
    /// accepts the reserved pseudo-command `"open-palette"` (not a real
    /// `CommandId`), which overrides the chord that opens the command
    /// palette itself.
    pub(crate) keybindings: HashMap<String, String>,
    /// `[theme]`: the app's one color scheme. See [`RawThemeConfig`].
    pub(crate) theme: RawThemeConfig,
}

/// `[agent]`: tuning values for the bash/fs tools and the turn-loop guards.
/// See `agent::config::AgentToolsConfig` and `RigAgentConfig` for the
/// built-in defaults each field falls back to when unset here.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub(crate) struct RawAgentConfig {
    pub(crate) bash_timeout_default_secs: Option<u64>,
    pub(crate) bash_timeout_max_secs: Option<u64>,
    pub(crate) bash_output_cap_chars: Option<usize>,
    pub(crate) bash_drain_grace_secs: Option<u64>,
    pub(crate) fs_read_line_cap: Option<usize>,
    pub(crate) fs_grep_max_bytes: Option<u64>,
    pub(crate) fs_traversal_max_files: Option<usize>,
    /// Default number of matches `fs.grep` returns when a call doesn't pass
    /// its own `limit` — distinct from `fs_grep_max_bytes`/
    /// `fs_traversal_max_files` above, which cap how much of the tree a
    /// single traversal *scans*, not how many of the matches found get
    /// returned.
    pub(crate) fs_grep_result_limit: Option<usize>,
    /// Same idea as `fs_grep_result_limit`, for `fs.glob`.
    pub(crate) fs_glob_result_limit: Option<usize>,
    pub(crate) iteration_cap: Option<u32>,
    pub(crate) doom_loop_window: Option<usize>,
    /// Token budget for the conversation history sent to the provider on
    /// each turn. See `agent::config::RigAgentConfig::history_token_budget`
    /// for the built-in default and why it's applied unconditionally.
    pub(crate) history_token_budget: Option<usize>,
    /// How often, in milliseconds, streamed assistant-text/reasoning deltas
    /// and tool-call-argument progress are coalesced into an emitted event.
    /// See `providers::rig::stream`'s `StreamDeltaBuffer`/
    /// `ToolCallProgressBuffer`.
    pub(crate) stream_flush_interval_ms: Option<u64>,
    /// Character count that forces an early flush of a streamed
    /// assistant-text/reasoning delta, ahead of the time-based flush above.
    pub(crate) stream_flush_chars: Option<usize>,
    /// How often, in seconds, the workspace pane header's agent
    /// turn-in-flight elapsed-time display ("running · 12s") re-renders.
    /// See `workspace::view::pane`'s `schedule_tick`.
    pub(crate) pane_status_tick_secs: Option<u64>,
    /// Overrides the append-only agent event log (JSONL) path. The
    /// `HORIZON_AGENT_EVENT_LOG` env var, if set, wins over this.
    pub(crate) event_log_path: Option<String>,
    /// Overrides the DuckDB projection database path used to replay
    /// per-session rig history. The `HORIZON_AGENT_STATE_DB` env var, if
    /// set, wins over this. Unset (here and via env) means no persisted
    /// memory.
    pub(crate) state_db_path: Option<String>,
    /// Character cap on the "Repository instructions" system-prompt section
    /// built from `AGENTS.md`/`CLAUDE.md` files found while walking from
    /// the session's working directory up to the repository root. See
    /// `agent::config::RigAgentConfig::repository_instructions_cap_chars`
    /// for the built-in default and its rationale.
    pub(crate) repository_instructions_cap_chars: Option<usize>,
}

/// `[provider]`: model selection, base URL, and request parameters for the
/// built-in rig/OpenAI provider. Never a place for secrets — see the module
/// doc.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct RawProviderConfig {
    pub(crate) model: Option<String>,
    pub(crate) base_url: Option<String>,
    /// Sampling temperature passed to rig's completion request. Unset (the
    /// default) means "let the provider use its own default" — rig never
    /// sends the field at all in that case.
    pub(crate) temperature: Option<f64>,
    /// Max output tokens passed to rig's completion request. Unset (the
    /// default) means "let the provider use its own default".
    pub(crate) max_tokens: Option<u64>,
}

/// `[terminal]`: cell rendering metrics, scrollback, the spawned shell, and
/// the `TERM` identity presented to it. See `terminal::config` for the
/// built-in defaults each field falls back to when unset here.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct RawTerminalConfig {
    pub(crate) font_size: Option<f32>,
    pub(crate) line_height: Option<f64>,
    pub(crate) scrollback_lines: Option<usize>,
    /// Overrides the spawned shell program. The `SHELL` env var, if set,
    /// wins over this (matching the existing "existing env vars keep
    /// winning" precedence rule).
    pub(crate) shell: Option<String>,
    /// Extra argv entries passed to the spawned shell (e.g. `["-l"]` for a
    /// login shell). No corresponding env var.
    pub(crate) shell_args: Option<Vec<String>>,
    /// The `TERM` value presented to the spawned shell.
    pub(crate) term: Option<String>,
}

/// `[theme]`: the app's one color scheme — named role overrides for the
/// chrome palette (flattened into this struct's `colors` map, e.g.
/// `"accent"`, `"terminal_cursor"`) plus the nested `[theme.ansi]` table for
/// the 16 base ANSI slots. Both are parsed and validated by `ui::theme`
/// (`colors` against its accessor names, `ansi` field-by-field). Keeping
/// `ansi` a named field alongside the flattened map — rather than putting
/// everything in one flat namespace — leaves room for a future named-scheme
/// layer (e.g. `[theme.schemes.dracula]`) to nest in the same way without
/// reshaping either table's keys.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct RawThemeConfig {
    pub(crate) ansi: RawThemeAnsiConfig,
    /// Palette name (matching a `ui::theme` accessor, e.g. `"accent"`) to a
    /// `#rrggbb`/`#rgb` hex string. Flattened so this and `ansi` above share
    /// the same `[theme]` table in TOML.
    #[serde(flatten)]
    pub(crate) colors: HashMap<String, String>,
}

/// `[theme.ansi]`: the 16 base ANSI color slots, each an optional
/// `#rrggbb`/`#rgb` hex string. See `ui::theme::ansi` for the built-in
/// defaults each falls back to when unset here.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct RawThemeAnsiConfig {
    pub(crate) black: Option<String>,
    pub(crate) red: Option<String>,
    pub(crate) green: Option<String>,
    pub(crate) yellow: Option<String>,
    pub(crate) blue: Option<String>,
    pub(crate) magenta: Option<String>,
    pub(crate) cyan: Option<String>,
    pub(crate) white: Option<String>,
    pub(crate) bright_black: Option<String>,
    pub(crate) bright_red: Option<String>,
    pub(crate) bright_green: Option<String>,
    pub(crate) bright_yellow: Option<String>,
    pub(crate) bright_blue: Option<String>,
    pub(crate) bright_magenta: Option<String>,
    pub(crate) bright_cyan: Option<String>,
    pub(crate) bright_white: Option<String>,
}

/// `[ui]`: cross-domain UI primitives — the app-wide font family (shared by
/// the terminal, agent transcript, and workspace agent controls) and the
/// window's initial size. See `ui::fonts` and `app::config` for the
/// built-in defaults.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct RawUiConfig {
    pub(crate) font_family: Option<String>,
    pub(crate) window_width: Option<f64>,
    pub(crate) window_height: Option<f64>,
}

/// Loads and caches the config file for the lifetime of the process. Config
/// is applied at startup only, so every call after the first returns the
/// same cached value instead of re-reading the file.
///
/// Under `#[cfg(test)]` this resolves to built-in defaults unconditionally:
/// tests assert about built-in defaults and must not be affected by the
/// developer's personal config at `~/.config/horizon/config.toml`. Tests
/// that genuinely need to parse a file use `load_from_path` directly (see
/// `src/config/tests.rs`) or read `config.example.toml` by path (the
/// example-file drift guards).
pub(crate) fn load() -> &'static RawConfig {
    static CONFIG: OnceLock<RawConfig> = OnceLock::new();
    #[cfg(test)]
    {
        CONFIG.get_or_init(RawConfig::default)
    }
    #[cfg(not(test))]
    {
        CONFIG.get_or_init(|| load_from_path(resolve_config_path().as_deref()))
    }
}

#[cfg(not(test))]
fn resolve_config_path() -> Option<PathBuf> {
    resolve_config_path_from(
        std::env::var(CONFIG_PATH_VAR).ok(),
        std::env::var(XDG_CONFIG_HOME_VAR).ok(),
        std::env::var(HOME_VAR).ok(),
    )
}

/// Pure path-resolution logic, factored out of [`resolve_config_path`] so it
/// can be unit-tested without mutating process environment variables —
/// `cargo test` runs tests in parallel within one process, so real env
/// mutation in a test would race every other test reading the same
/// variable.
fn resolve_config_path_from(
    horizon_config: Option<String>,
    xdg_config_home: Option<String>,
    home: Option<String>,
) -> Option<PathBuf> {
    if let Some(path) = non_empty(horizon_config) {
        return Some(PathBuf::from(path));
    }
    let config_home = match non_empty(xdg_config_home) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(non_empty(home)?).join(".config"),
    };
    Some(config_home.join("horizon").join("config.toml"))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

fn load_from_path(path: Option<&Path>) -> RawConfig {
    let Some(path) = path else {
        return RawConfig::default();
    };
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        // No file written yet is the common case, not a warning.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return RawConfig::default(),
        Err(error) => {
            eprintln!(
                "horizon config: could not read {}: {error} -- using built-in defaults",
                path.display()
            );
            return RawConfig::default();
        }
    };
    parse(&contents).unwrap_or_else(|error| {
        eprintln!(
            "horizon config: could not parse {}: {error} -- using built-in defaults",
            path.display()
        );
        RawConfig::default()
    })
}

fn parse(contents: &str) -> Result<RawConfig, toml::de::Error> {
    toml::from_str(contents)
}

#[cfg(test)]
mod tests;
