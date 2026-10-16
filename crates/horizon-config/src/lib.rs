//! Horizon's single configuration file.
//!
//! See `AGENTS.md`'s "Configuration" section for the user-facing summary
//! and `config.example.toml` at the repo root for every knob with its
//! default. This module owns locating and parsing the TOML file into a
//! [`RawConfig`]; `horizon-agentd`'s `config` module and the shell
//! crate's `keymap`/`theme`/`terminal` modules each read the section
//! relevant to them and apply their own env-var precedence and built-in
//! defaults on top (env var > this file > built-in default).
//!
//! Named `[[providers]]` entries describe endpoints and credentials only.
//! Models are chosen by two top-level `{ provider, model }` pairs:
//! `default_model` for conversation sessions and `small_model` (which must
//! name an OpenAI-compatible entry) for titles and approval judgments.
//! No provider entries means the built-in OpenAI-compatible `default` entry.
//! Environment values override file values; secrets remain environment-only.
//! A missing file uses defaults. Invalid files warn at startup; reload errors
//! retain the previously applied configuration. Reload applies provider changes
//! to new sessions and title calls, alongside theme and keybindings.

pub mod grants;
mod moa;
mod warnings;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub use grants::{ProjectGrant, RawGrantsConfig, RawProjectGrant};
pub use moa::{
    moa_config_warnings, RawMoaConfig, RawMoaMember, ResolvedMoaConfig, ResolvedMoaMember,
    MOA_PROVIDER_NAME,
};

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
    /// Named provider entries in file order.
    pub providers: Vec<RawNamedProviderConfig>,
    /// `default_model = { provider, model }`: what a conversation session
    /// runs when nothing else selected a model. `None` (or a pair that does
    /// not resolve) means no model at all: sessions start without one.
    #[serde(deserialize_with = "deserialize_lenient_model_pair")]
    pub default_model: Option<RawModelPair>,
    /// `small_model = { provider, model }`: the model titles and approval
    /// judgments run on. The provider must be an openai-compatible entry;
    /// `None` (or an invalid pair) disables both.
    #[serde(deserialize_with = "deserialize_lenient_model_pair")]
    pub small_model: Option<RawModelPair>,
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
    /// `[grants]`: per-project filesystem trees and network destinations
    /// agent sessions start with. See the [`grants`] module doc for why
    /// this is user-owned config rather than anything the repository or
    /// the approval flow can write.
    pub grants: RawGrantsConfig,
    /// `trusted_projects`: absolute repository toplevels whose repository
    /// content (`.horizon/skills/` skills, `AGENTS.md`/`CLAUDE.md`
    /// instructions) an agent session may load into its system prompt. A
    /// session whose project root is NOT listed here gets embedded skills
    /// only and no repository instructions — the prompt-injection surface
    /// `skills`' module doc's trust note used to accept unconditionally is
    /// now gated behind this user-owned, per-project decision (owner
    /// decision 2026-08-05). Same semantics as `[[grants.project]]` `root`:
    /// each entry is the project's main-repository toplevel, a session in
    /// an isolated worktree resolves back to it, and matching is exact.
    pub trusted_projects: Vec<String>,
    /// `[[moa]]`: Mixture-of-Agents entries, each a named combination of one
    /// aggregator and a list of proposers over the `[[providers]]` entries
    /// above (`docs/agent-moa-design.md`). Empty unless the file sets it.
    /// Selected like a provider — the model picker shows a `moa` group whose
    /// items are these names — and reloaded like `[[providers]]`: new
    /// sessions see the change.
    pub moa: Vec<RawMoaConfig>,
}

/// Which rig-backed provider *client* an entry builds: the first-scope kinds
/// (owner-agreed: `openai-compatible` + `anthropic`). `openai-compatible`
/// (the default) is any endpoint speaking OpenAI's chat-completions wire
/// (rig's `openai::CompletionsClient`); `anthropic` is rig's
/// `anthropic::Client`. rig-core 0.42 bundles both with no feature gates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RawProviderKind {
    #[default]
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    Anthropic,
}

impl RawProviderKind {
    /// The environment variable *name* an entry's API key is read from when
    /// the file doesn't override `api_key_env` — per kind, mirroring rig's
    /// own `api_key_env` defaults. The config file never carries the key
    /// itself: it only ever names the variable (the module doc's
    /// secrets-stay-out rule, now one variable *name* per provider).
    pub fn default_api_key_env(self) -> &'static str {
        match self {
            RawProviderKind::OpenAiCompatible => "OPENAI_API_KEY",
            RawProviderKind::Anthropic => "ANTHROPIC_API_KEY",
        }
    }
}

/// One `[[providers]]` entry as the file writes it: an endpoint and the
/// name of its credential variable. There is no model here: the picker's
/// candidates come from the provider's own `/models` listing, and the
/// top-level `default_model`/`small_model` pairs name the models that run
/// when nothing else selected one.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawNamedProviderConfig {
    pub name: String,
    pub kind: Option<RawProviderKind>,
    pub base_url: Option<String>,
    /// The environment variable **name** (never a value) the entry's API key
    /// is read from. `None` means the kind's own default
    /// ([`RawProviderKind::default_api_key_env`]).
    pub api_key_env: Option<String>,
}

/// A top-level `{ provider, model }` pair as the file writes it
/// (`default_model`, `small_model`): a `[[providers]]` entry name plus a
/// model id.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawModelPair {
    pub provider: String,
    pub model: String,
}

/// A `{ provider, model }` pair whose provider names a resolved entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedModelPair {
    pub provider: String,
    pub model: String,
}

/// [`RawConfig::resolved_small_model`]'s output: the openai-compatible entry
/// the `small_model` pair names, plus the model id to run on it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSmallModel {
    pub provider: ResolvedProviderConfig,
    pub model: String,
}

/// One resolved provider entry — [`RawConfig::resolved_providers`]'s output:
/// entry defaults applied and
/// nameless entries dropped. This is the shape `horizon-agentd` hands to
/// `horizon_agent::config`, which owns the env-var precedence on top
/// (`OPENAI_BASE_URL`/`ANTHROPIC_BASE_URL` keep winning over the file's
/// `base_url`; keys are read from the named variable).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProviderConfig {
    pub name: String,
    pub kind: RawProviderKind,
    pub base_url: Option<String>,
    pub api_key_env: String,
}

/// [`RawConfig::resolved_providers`]'s whole output: the effective entry
/// list (file order) and the conversation default model, when the
/// `default_model` pair resolves against that list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvidersResolution {
    pub providers: Vec<ResolvedProviderConfig>,
    pub default_model: Option<ResolvedModelPair>,
}

/// The implicit entry used when no providers are configured.
pub const DEFAULT_PROVIDER_NAME: &str = "default";

impl RawConfig {
    /// Apply entry defaults, drop nameless entries, and resolve the conversation
    /// default model. An empty file surface yields the built-in
    /// OpenAI-compatible entry.
    pub fn resolved_providers(&self) -> ProvidersResolution {
        let mut providers: Vec<ResolvedProviderConfig> = Vec::new();
        if self.providers.is_empty() {
            providers.push(ResolvedProviderConfig {
                name: DEFAULT_PROVIDER_NAME.to_string(),
                kind: RawProviderKind::OpenAiCompatible,
                base_url: None,
                api_key_env: RawProviderKind::OpenAiCompatible
                    .default_api_key_env()
                    .to_string(),
            });
        } else {
            for entry in &self.providers {
                if entry.name.is_empty() {
                    continue;
                }
                let kind = entry.kind.unwrap_or_default();
                providers.push(ResolvedProviderConfig {
                    name: entry.name.clone(),
                    kind,
                    base_url: entry.base_url.clone(),
                    api_key_env: entry
                        .api_key_env
                        .clone()
                        .unwrap_or_else(|| kind.default_api_key_env().to_string()),
                });
            }
        }
        let default_model =
            resolve_pair(DEFAULT_MODEL_KEY, self.default_model.as_ref(), &providers)
                .ok()
                .map(|(_, pair)| pair);
        ProvidersResolution {
            providers,
            default_model,
        }
    }

    /// The `default_model` pair resolved against the provider entries, or
    /// why it does not (absent, no model id, unknown entry).
    pub fn resolved_default_model(&self) -> Result<ResolvedModelPair, String> {
        let providers = self.resolved_providers().providers;
        resolve_pair(DEFAULT_MODEL_KEY, self.default_model.as_ref(), &providers)
            .map(|(_, pair)| pair)
    }

    /// The `small_model` pair resolved against the provider entries. Its
    /// provider must be openai-compatible; an invalid pair never redirects a
    /// request to another entry.
    pub fn resolved_small_model(&self) -> Result<ResolvedSmallModel, String> {
        let providers = self.resolved_providers().providers;
        let (entry, pair) = resolve_pair(SMALL_MODEL_KEY, self.small_model.as_ref(), &providers)?;
        if entry.kind != RawProviderKind::OpenAiCompatible {
            return Err(format!(
                "{SMALL_MODEL_KEY}: provider {:?} must be openai-compatible — titles and \
                 approval judgments are disabled",
                pair.provider
            ));
        }
        Ok(ResolvedSmallModel {
            provider: entry,
            model: pair.model,
        })
    }
}

/// The top-level key naming the conversation sessions' default model.
pub const DEFAULT_MODEL_KEY: &str = "default_model";
/// The top-level key naming the model titles and approval judgments use.
pub const SMALL_MODEL_KEY: &str = "small_model";

/// Resolves one top-level `{ provider, model }` pair against `providers`.
/// The error names `key` and what runs instead.
fn resolve_pair(
    key: &str,
    pair: Option<&RawModelPair>,
    providers: &[ResolvedProviderConfig],
) -> Result<(ResolvedProviderConfig, ResolvedModelPair), String> {
    let consequence = if key == DEFAULT_MODEL_KEY {
        "sessions start without a model"
    } else {
        "titles and approval judgments are disabled"
    };
    let pair = pair.ok_or_else(|| {
        format!(
            "{key}: not set — {consequence} (set {key} = {{ provider = \"...\", model = \"...\" }})"
        )
    })?;
    if pair.model.is_empty() {
        return Err(format!("{key}: has no model id — {consequence}"));
    }
    let entry = providers
        .iter()
        .find(|entry| entry.name == pair.provider)
        .ok_or_else(|| {
            format!(
                "{key}: provider {:?} names no [[providers]] entry — {consequence}",
                pair.provider
            )
        })?;
    Ok((
        entry.clone(),
        ResolvedModelPair {
            provider: pair.provider.clone(),
            model: pair.model.clone(),
        },
    ))
}

/// Deserializes a top-level `{ provider, model }` pair leniently: a value
/// that is not such a table becomes `None` instead of failing the whole
/// file's parse (the per-value policy [`deserialize_lenient_f64`] applies
/// too). `warnings` names the wrong shape.
fn deserialize_lenient_model_pair<'de, D>(deserializer: D) -> Result<Option<RawModelPair>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<toml::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| match value {
        toml::Value::Table(_) => value.try_into().ok(),
        _ => None,
    }))
}

/// Value-level warnings for the provider sections, beside the name-walking
/// [`warnings::warn`] (same warn-and-continue policy, never fail startup).
/// Collected once per parse alongside key-name warnings.
pub fn provider_config_warnings(config: &RawConfig) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Err(error) = config.resolved_default_model() {
        warnings.push(error);
    }
    if let Err(error) = config.resolved_small_model() {
        warnings.push(error);
    }
    for (index, entry) in config.providers.iter().enumerate() {
        if entry.name.is_empty() {
            warnings.push(format!(
                "[[providers]]: entry {index} has no name, dropping it (name it so a model pair can select it)"
            ));
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    for entry in &config.providers {
        if entry.name.is_empty() {
            continue;
        }
        if seen.contains(&entry.name.as_str()) {
            warnings.push(format!(
                "[[providers]]: duplicate name {} — the later entry is shadowed",
                entry.name
            ));
        } else {
            seen.push(entry.name.as_str());
        }
    }
    warnings
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
///
/// [`Serialize`] is what lets the shell hand this section to a preview-pane
/// plugin (`src/preview/`) as JSON and have the guest deserialize it back
/// into the same struct the scheme resolver consumes. JSON rather than TOML:
/// `colors` is `#[serde(flatten)]`ed and `ansi` is a sub-table, an ordering
/// TOML's "values before tables" rule rejects.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
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

/// `[theme.ansi]`: the six normal-hue ANSI color slots, each an optional
/// `#rrggbb`/`#rgb` hex string -- the seed's own hue set
/// (`docs/theme-design.md`'s 2026-07-16 "config surface narrowed to the
/// seed" decision; the ten bright/black/white slots are derived-only and no
/// longer configurable). See `ui::theme::ansi` for the built-in defaults
/// each falls back to when unset here.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct RawThemeAnsiConfig {
    pub red: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub blue: Option<String>,
    pub magenta: Option<String>,
    pub cyan: Option<String>,
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

/// This process's `$HOME`, for `[grants]`' `~` expansion. Borrowed from
/// `horizon_sandbox` rather than re-read here so the expansion and the
/// "is this tree `$HOME` itself?" refusal always agree about what `$HOME`
/// is. Unlike [`resolve_config_path`] this is *not* `#[cfg(test)]`-gated:
/// it never resolves the developer's real config file, and the pure
/// `grants::resolve` (which every expansion test drives directly) takes
/// `home` as an argument anyway.
fn home_dir() -> Option<PathBuf> {
    horizon_sandbox::home_dir()
}

/// Every validated `[[grants.project]]` entry -- what `horizon-agentd`
/// consults at session spawn to decide which trees this session's project
/// may write to (`grants::trees_for_project`). Entries this crate refused
/// (over-broad, unexpandable) are already dropped, having warned on stderr
/// when the file was read.
pub fn project_grants(config: &RawConfig) -> Vec<ProjectGrant> {
    grants::resolve(&config.grants.project, home_dir().as_deref()).0
}

/// Every validated `trusted_projects` entry — what `horizon-agentd`
/// consults at session spawn to decide whether repository skills and
/// `AGENTS.md`/`CLAUDE.md` instructions may be loaded into the prompt. Each
/// entry is expanded the same way `[[grants.project]]` `root` is (a leading
/// `~/` against `$HOME`, then required to be absolute); an entry that
/// doesn't survive that expansion is warned about on stderr and dropped,
/// matching `grants::resolve`'s warn-and-ignore policy.
pub fn trusted_projects(config: &RawConfig) -> Vec<std::path::PathBuf> {
    let home = home_dir();
    let mut resolved = Vec::new();
    for entry in &config.trusted_projects {
        match grants::expand(entry, home.as_deref()) {
            Some(path) => {
                if !resolved.contains(&path) {
                    resolved.push(path);
                }
            }
            None => {
                eprintln!(
                    "horizon config: trusted_projects: entry {entry:?} is not an absolute path \
                     (and no $HOME is set to expand a leading \"~/\" against), ignoring it"
                );
            }
        }
    }
    resolved
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
            // Retired/unrecognized-key warnings (`[agent]`/`[[providers]]`/
            // `[terminal]`/`[ui]`/`[grants]` -- see `warnings`' module doc)
            // run here, once per successful parse, so both
            // `load_from_path` (startup) and `reload_from_path` (`Reload
            // Config`) get them through this one shared call site.
            warnings::warn(&contents);
            // `[grants]`' *value* validation (over-broad trees, unexpandable
            // `~`) can't be done by name-walking the raw table, so it runs
            // beside it, off the same successful parse.
            for warning in grants::resolve(&config.grants.project, home_dir().as_deref()).1 {
                eprintln!("horizon config: {warning}");
            }
            // Provider value warnings (invalid selections and nameless or
            // duplicate entries) are likewise
            // name-walk-invisible, off the same successful parse.
            for warning in provider_config_warnings(&config) {
                eprintln!("horizon config: {warning}");
            }
            // `[[moa]]`'s value warnings ride the same seam: which entries
            // resolve at all depends on the provider entries beside them,
            // which a name walk cannot see either.
            for warning in moa_config_warnings(&config) {
                eprintln!("horizon config: {warning}");
            }
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
/// like `reload_agent_runtime`, which spawns a real process, there is
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

/// Parse the current file contract. Legacy configuration is converted offline.
pub fn parse(contents: &str) -> Result<RawConfig, String> {
    let root = toml::from_str::<toml::Table>(contents).map_err(|error| error.to_string())?;
    if root.contains_key("provider") {
        return Err("[provider] was removed; convert it to [[providers]] using the procedure in docs/provider-configuration.md".into());
    }
    toml::Value::Table(root)
        .try_into()
        .map_err(|error: toml::de::Error| error.to_string())
}

#[cfg(test)]
mod tests;
