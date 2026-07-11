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
//! - **Applied at startup only, except `[theme]`/`[keybindings]`.** Nothing
//!   here watches the file for changes; restart Horizon to pick up edits to
//!   `[agent]`/`[provider]`/`[terminal]`/`[ui]`. The `Reload Config` command
//!   (`app::command_actions::reload_config`, [`reload`]) re-reads the file
//!   and applies `[theme]` (`ui::theme::apply_reload`) and `[keybindings]`
//!   (`app::keymap::Keymap::reload`) live — see that command's doc comment
//!   for why those two sections and not the rest.
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
pub struct RawConfig {
    pub agent: RawAgentConfig,
    pub provider: RawProviderConfig,
    pub terminal: RawTerminalConfig,
    pub ui: RawUiConfig,
    /// Key chord string (e.g. `"ctrl+shift+t"`) to `CommandId` string (e.g.
    /// `"new-terminal"`) — parsed and validated by `app::keymap`. Also
    /// accepts the reserved pseudo-command `"open-palette"` (not a real
    /// `CommandId`), which overrides the chord that opens the command
    /// palette itself.
    pub keybindings: HashMap<String, String>,
    /// `[theme]`: the app's one color scheme. See [`RawThemeConfig`].
    pub theme: RawThemeConfig,
}

/// `[agent]`: tuning values for the bash/fs tools and the turn-loop guards.
/// See `agent::config::AgentToolsConfig` and `RigAgentConfig` for the
/// built-in defaults each field falls back to when unset here.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct RawAgentConfig {
    pub bash_timeout_default_secs: Option<u64>,
    pub bash_timeout_max_secs: Option<u64>,
    pub bash_output_cap_chars: Option<usize>,
    pub bash_drain_grace_secs: Option<u64>,
    pub fs_read_line_cap: Option<usize>,
    pub fs_grep_max_bytes: Option<u64>,
    pub fs_traversal_max_files: Option<usize>,
    /// Default number of matches `fs.grep` returns when a call doesn't pass
    /// its own `limit` — distinct from `fs_grep_max_bytes`/
    /// `fs_traversal_max_files` above, which cap how much of the tree a
    /// single traversal *scans*, not how many of the matches found get
    /// returned.
    pub fs_grep_result_limit: Option<usize>,
    /// Same idea as `fs_grep_result_limit`, for `fs.glob`.
    pub fs_glob_result_limit: Option<usize>,
    pub iteration_cap: Option<u32>,
    pub doom_loop_window: Option<usize>,
    /// Token budget for the conversation history sent to the provider on
    /// each turn. See `agent::config::RigAgentConfig::history_token_budget`
    /// for the built-in default and why it's applied unconditionally.
    pub history_token_budget: Option<usize>,
    /// How often, in milliseconds, streamed assistant-text/reasoning deltas
    /// and tool-call-argument progress are coalesced into an emitted event.
    /// See `providers::rig::stream`'s `StreamDeltaBuffer`/
    /// `ToolCallProgressBuffer`.
    pub stream_flush_interval_ms: Option<u64>,
    /// Character count that forces an early flush of a streamed
    /// assistant-text/reasoning delta, ahead of the time-based flush above.
    pub stream_flush_chars: Option<usize>,
    /// How often, in seconds, the workspace pane header's agent
    /// turn-in-flight elapsed-time display ("running · 12s") re-renders.
    /// See `workspace::view::pane`'s `schedule_tick`.
    pub pane_status_tick_secs: Option<u64>,
    /// Overrides the append-only agent event log (JSONL) path. The
    /// `HORIZON_AGENT_EVENT_LOG` env var, if set, wins over this.
    pub event_log_path: Option<String>,
    /// Overrides the DuckDB projection database path used to replay
    /// per-session rig history. The `HORIZON_AGENT_STATE_DB` env var, if
    /// set, wins over this. Unset (here and via env) means no persisted
    /// memory.
    pub state_db_path: Option<String>,
    /// Character cap on the "Repository instructions" system-prompt section
    /// built from `AGENTS.md`/`CLAUDE.md` files found while walking from
    /// the session's working directory up to the repository root. See
    /// `agent::config::RigAgentConfig::repository_instructions_cap_chars`
    /// for the built-in default and its rationale.
    pub repository_instructions_cap_chars: Option<usize>,
}

/// `[provider]`: model selection, base URL, and request parameters for the
/// built-in rig/OpenAI provider. Never a place for secrets — see the module
/// doc.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawProviderConfig {
    pub model: Option<String>,
    pub base_url: Option<String>,
    /// Sampling temperature passed to rig's completion request. Unset (the
    /// default) means "let the provider use its own default" — rig never
    /// sends the field at all in that case.
    pub temperature: Option<f64>,
    /// Max output tokens passed to rig's completion request. Unset (the
    /// default) means "let the provider use its own default".
    pub max_tokens: Option<u64>,
}

/// `[terminal]`: cell rendering metrics, scrollback, the spawned shell, and
/// the `TERM` identity presented to it. See `terminal::config` for the
/// built-in defaults each field falls back to when unset here.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawTerminalConfig {
    pub font_size: Option<f32>,
    pub line_height: Option<f64>,
    pub scrollback_lines: Option<usize>,
    /// Overrides the spawned shell program. The `SHELL` env var, if set,
    /// wins over this (matching the existing "existing env vars keep
    /// winning" precedence rule).
    pub shell: Option<String>,
    /// Extra argv entries passed to the spawned shell (e.g. `["-l"]` for a
    /// login shell). No corresponding env var.
    pub shell_args: Option<Vec<String>>,
    /// The `TERM` value presented to the spawned shell.
    pub term: Option<String>,
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
pub struct RawThemeConfig {
    pub ansi: RawThemeAnsiConfig,
    /// Palette name (matching a `ui::theme` accessor, e.g. `"accent"`) to a
    /// `#rrggbb`/`#rgb` hex string. Flattened so this and `ansi` above share
    /// the same `[theme]` table in TOML.
    #[serde(flatten)]
    pub colors: HashMap<String, String>,
}

/// `[theme.ansi]`: the 16 base ANSI color slots, each an optional
/// `#rrggbb`/`#rgb` hex string. See `ui::theme::ansi` for the built-in
/// defaults each falls back to when unset here.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawThemeAnsiConfig {
    pub black: Option<String>,
    pub red: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub blue: Option<String>,
    pub magenta: Option<String>,
    pub cyan: Option<String>,
    pub white: Option<String>,
    pub bright_black: Option<String>,
    pub bright_red: Option<String>,
    pub bright_green: Option<String>,
    pub bright_yellow: Option<String>,
    pub bright_blue: Option<String>,
    pub bright_magenta: Option<String>,
    pub bright_cyan: Option<String>,
    pub bright_white: Option<String>,
}

/// `[ui]`: cross-domain UI primitives — the app-wide font family (shared by
/// the terminal, agent transcript, and workspace agent controls) and the
/// window's initial size. See `ui::fonts` and `app::config` for the
/// built-in defaults.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawUiConfig {
    pub font_family: Option<String>,
    pub window_width: Option<f64>,
    pub window_height: Option<f64>,
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
pub fn load() -> &'static RawConfig {
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

/// The outcome of trying to read and parse the config file at some path,
/// before either [`load_from_path`] (startup: every non-success case folds
/// into `RawConfig::default()` plus a stderr warning) or [`reload_from_path`]
/// (`Reload Config`: a parse/read error must NOT reset to defaults, since
/// that would blow away a working theme/keymap over a typo -- see that
/// function's doc comment) decides what to do with it. Factored out so the
/// two callers can't drift on what counts as "missing" vs. "malformed".
enum ConfigRead {
    /// No file at all -- the common case, not a warning; equivalent to
    /// `RawConfig::default()`.
    Missing,
    /// Boxed: `RawConfig` is a few hundred bytes (every section's fields,
    /// several `HashMap`s) while the other variants are a plain `String` --
    /// boxing keeps this enum from ballooning to the size of its largest
    /// variant (`clippy::large_enum_variant`).
    Parsed(Box<RawConfig>),
    /// The file exists but could not be read (permissions, a symlink loop,
    /// ...).
    ReadError(String),
    /// The file exists and was read, but isn't valid TOML (or doesn't match
    /// `RawConfig`'s shape).
    ParseError(String),
}

fn read_config(path: Option<&Path>) -> ConfigRead {
    let Some(path) = path else {
        return ConfigRead::Missing;
    };
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        // No file written yet is the common case, not a warning.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return ConfigRead::Missing,
        Err(error) => {
            return ConfigRead::ReadError(format!("could not read {}: {error}", path.display()))
        }
    };
    match parse(&contents) {
        Ok(config) => ConfigRead::Parsed(Box::new(config)),
        Err(error) => {
            ConfigRead::ParseError(format!("could not parse {}: {error}", path.display()))
        }
    }
}

fn load_from_path(path: Option<&Path>) -> RawConfig {
    match read_config(path) {
        ConfigRead::Missing => RawConfig::default(),
        ConfigRead::Parsed(config) => *config,
        ConfigRead::ReadError(message) | ConfigRead::ParseError(message) => {
            eprintln!("horizon config: {message} -- using built-in defaults");
            RawConfig::default()
        }
    }
}

/// Re-reads and parses the config file fresh from disk, bypassing [`load`]'s
/// startup-only cache -- `Reload Config`'s entry point
/// (`app::command_actions::reload_config`). Unlike [`load_from_path`] (used
/// only at startup, where nothing has been "applied" yet, so falling back to
/// defaults on any error is always safe), a reload distinguishes a missing
/// file (`Ok(RawConfig::default())` -- rewriting the process's whole
/// theme/keymap state back to defaults because the file got deleted is a
/// legitimate reload outcome, not a failure) from a read or parse error
/// (`Err`, so the caller can leave the currently applied theme/keymap
/// untouched instead of resetting them over a typo). Not gated by
/// `#[cfg(test)]` itself (unlike [`resolve_config_path`]): it takes the path
/// as a plain argument rather than reading the environment, so it's exactly
/// as safe to compile and call from a test as `load_from_path` is -- see
/// this module's tests.
pub fn reload_from_path(path: Option<&Path>) -> Result<RawConfig, String> {
    match read_config(path) {
        ConfigRead::Missing => Ok(RawConfig::default()),
        ConfigRead::Parsed(config) => Ok(*config),
        ConfigRead::ReadError(message) | ConfigRead::ParseError(message) => Err(message),
    }
}

/// `Reload Config`'s path resolution + fresh parse: re-resolves the config
/// path (in case `HORIZON_CONFIG`/`XDG_CONFIG_HOME`/`HOME` changed since
/// startup -- not the common case, but no more expensive to re-check than to
/// assume) and re-reads the file, entirely bypassing [`load`]'s cache. See
/// [`reload_from_path`] for the missing-file/error distinction.
///
/// Under `#[cfg(test)]` this resolves to built-in defaults unconditionally,
/// mirroring [`load`]'s own test-mode behavior for the same reason: a test
/// process must never observe the developer's real
/// `~/.config/horizon/config.toml`. `app::command_actions::reload_config`
/// (this function's one caller) is therefore not unit-tested directly --
/// like `reload_agent_runtime`, which spawns a real process, there is
/// nothing left to exercise here once `reload_from_path` (this module's
/// tests) and the theme/keymap apply functions it feeds
/// (`ui::theme::apply_reload`'s and `app::keymap::Keymap::reload`'s own
/// tests) are each covered on their own.
pub fn reload() -> Result<RawConfig, String> {
    #[cfg(test)]
    {
        Ok(RawConfig::default())
    }
    #[cfg(not(test))]
    {
        reload_from_path(resolve_config_path().as_deref())
    }
}

fn parse(contents: &str) -> Result<RawConfig, toml::de::Error> {
    toml::from_str(contents)
}

#[cfg(test)]
mod tests;
