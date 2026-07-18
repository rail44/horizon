//! Horizon's single configuration file.
//!
//! See `AGENTS.md`'s "Configuration" section for the user-facing summary
//! and `config.example.toml` at the repo root for every knob with its
//! default. This module owns locating and parsing the TOML file into a
//! [`RawConfig`]; `horizon-sessiond`'s `config` module and the shell
//! crate's `keymap`/`theme`/`terminal` modules each read the section
//! relevant to them and apply their own env-var precedence and built-in
//! defaults on top (env var > this file > built-in default).
//!
//! The 2026-07-18 config-narrowing wave (owner decision) cut the surface
//! to exactly: `[provider]` `model`/`base_url`; `[terminal]` `font_size`;
//! `[ui]` `font_family`; `[keybindings]`; `[theme]`'s seed plus
//! `[theme.ansi]`'s six hues. Everything that used to be tunable beyond
//! that (the entire former `[agent]` section, `[provider]`
//! `temperature`/`max_tokens`, `[terminal]` `line_height`/`term`/`shell`/
//! `shell_args`/`scrollback_lines`, `[ui]` `window_width`/`window_height`)
//! is now a fixed built-in default or constant in the crate that owns it
//! (`horizon-agent`'s `config` module for the former `[agent]` knobs; the
//! shell crate's `terminal`/`main` modules for the rest) — this crate no
//! longer parses any of them into a field at all. [`warnings::warn`] still
//! recognizes their *names*, so a config file that still sets one gets a
//! "no longer configurable" warning instead of silently doing nothing.
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
//!   fail startup" policy [`warnings`] and the shell crate's `theme` module
//!   apply per-entry to an unrecognized keybinding, theme color, or (as of
//!   this wave) any other section's key.
//! - **Applied at startup only, except `[theme]`/`[keybindings]`.** Nothing
//!   here watches the file for changes. The `Reload Config` command (the
//!   `CommandId::ReloadConfig` arm in the shell crate's `workspace.rs`,
//!   fed by [`reload`]) re-reads the file and applies `[theme]`
//!   (`theme::reload_from`) and `[keybindings]` (`workspace::apply_bindings`)
//!   live. `[provider]` picks up on `Reload Session Runtime` (a fresh
//!   `horizon-sessiond` process re-reads the file, no full UI restart
//!   needed); `[terminal]`/`[ui]` need a full UI restart.
//! - **Secrets stay out.** Nothing under `[provider]` accepts an API key —
//!   `OPENAI_API_KEY` (and any future provider secret) is environment-only.

mod warnings;

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

/// `[provider]`: model selection and base URL for the built-in rig/OpenAI
/// provider. Never a place for secrets — see the module doc. `temperature`/
/// `max_tokens` were retired in the 2026-07-18 config-narrowing wave (see
/// the module doc) — a file that still sets either now gets a
/// [`warnings`] warning instead of the field silently doing nothing.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawProviderConfig {
    pub model: Option<String>,
    pub base_url: Option<String>,
}

/// `[terminal]`: cell rendering metrics for the spawned shell. See
/// `terminal::font_size` (the shell crate) for the built-in default
/// `font_size` falls back to when unset here. `line_height`/`term`/
/// `shell`/`shell_args`/`scrollback_lines` were retired in the 2026-07-18
/// config-narrowing wave (see the module doc) — each is now a fixed
/// built-in default or formula in the shell crate.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawTerminalConfig {
    pub font_size: Option<f32>,
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
    /// The WCAG contrast-ratio target for `text_primary` against
    /// `surface_base`, feeding `ui::theme`'s seed derivation
    /// (`docs/theme-design.md`). Clamped to `[4.5, 21.0]` and defaulted
    /// (both by `ui::theme`, not here) when absent or unparsable.
    /// Deserialized leniently via [`deserialize_lenient_f64`] rather than
    /// as a plain `Option<f64>`: a plain typed field would fail *the whole
    /// config file's* parse on a type mismatch (e.g. a quoted string),
    /// whereas every other `[theme]` value (the hex-string roles below,
    /// `ansi`'s slots) only ever drops that *one* entry to its built-in
    /// default -- see that function's doc for the mechanism.
    #[serde(deserialize_with = "deserialize_lenient_f64")]
    pub text_contrast: Option<f64>,
    /// Palette name (matching a `ui::theme` accessor, e.g. `"accent"`) to a
    /// `#rrggbb`/`#rgb` hex string. Flattened so this and `ansi` above share
    /// the same `[theme]` table in TOML.
    #[serde(flatten)]
    pub colors: HashMap<String, String>,
}

/// Deserializes an optional TOML value into `Option<f64>`, accepting both
/// TOML integers and floats and silently discarding (`None`, not a parse
/// error) any other type -- unlike `#[serde(default)]` on a plain
/// `Option<f64>` field, which errors the *entire file's* parse on a type
/// mismatch. Mirrors the "warn and skip, never fail startup" policy
/// `ui::theme` already applies per-entry to hex-string `[theme]` values
/// (an unparsable one falls back to that role's built-in default, not a
/// startup failure) -- this is the same policy applied at the TOML-type
/// level instead of the hex-string level, since `text_contrast` is a bare
/// number rather than a string `ui::theme` parses itself.
fn deserialize_lenient_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<toml::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| {
        value
            .as_float()
            .or_else(|| value.as_integer().map(|value| value as f64))
    }))
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

/// `[ui]`: the app-wide font family, shared by the terminal, agent
/// transcript, and workspace agent controls. See `terminal::resolved_font`
/// (the shell crate) for the built-in default this falls back to when
/// unset here. `window_width`/`window_height` were retired in the
/// 2026-07-18 config-narrowing wave (see the module doc) — the window now
/// always opens at a fixed built-in size (`main.rs`, the shell crate).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawUiConfig {
    pub font_family: Option<String>,
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

/// The config file path [`load`]/[`reload`] themselves resolve to
/// (`HORIZON_CONFIG` > `XDG_CONFIG_HOME` > `HOME`), exposed for a caller
/// that needs to *write* to the same file those read from -- the theme
/// settings view's explicit Save action (`docs/theme-settings-view-design.md`)
/// is the one caller today. `None` means the same thing it means for
/// [`load`]: no `HOME`/`XDG_CONFIG_HOME` to fall back to.
///
/// `#[cfg(test)]` resolves to `None` unconditionally, mirroring [`load`]/
/// [`reload`]'s own gate for the same reason: a test process must never
/// observe the developer's real environment or resolve to their real
/// `~/.config/horizon/config.toml`. Tests that need to exercise real path
/// resolution use [`resolve_config_path_from`] directly (see this module's
/// own tests), same as `load`/`reload`'s existing test seams.
pub fn resolved_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        resolve_config_path()
    }
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
        Ok(config) => {
            // Retired/unrecognized-key warnings (`[agent]`/`[provider]`/
            // `[terminal]`/`[ui]` -- see `warnings`' module doc) run here,
            // once per successful parse, so both `load_from_path` (startup)
            // and `reload_from_path` (`Reload Config`) get them through this
            // one shared call site.
            warnings::warn(&contents);
            ConfigRead::Parsed(Box::new(config))
        }
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
/// startup-only cache -- `Reload Config`'s entry point (the
/// `CommandId::ReloadConfig` arm in the shell crate's `workspace.rs`).
/// Unlike [`load_from_path`] (used
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
/// `~/.config/horizon/config.toml`. The `CommandId::ReloadConfig` arm
/// (this function's one caller) is therefore not unit-tested directly --
/// like `reload_session_runtime`, which spawns a real process, there is
/// nothing left to exercise here once `reload_from_path` (this module's
/// tests) and the theme/keymap apply functions it feeds (`theme::reload_from`'s
/// and the shell crate's `keymap::resolve_keybindings`'s own tests) are
/// each covered on their own.
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
