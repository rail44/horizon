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
//! longer parses any of them into a field at all, and carries no separate
//! retired-key compatibility warning either (owner decision 2026-08-03): a
//! config file that still sets one of those names gets
//! [`warnings::warn`]'s ordinary "probable typo" treatment, the same as
//! any other unrecognized key.
//!
//!
//! **The surface below is deliberately re-extended, not frozen.** The
//! 2026-07-18 narrowing's single-`[provider]` shape assumed one rig-backed
//! provider; the owner-agreed multi-provider wave (2026-10, board task #1)
//! re-extends the declared surface with the `[[providers]]` array and
//! `default_provider` -- and keeps the legacy `[provider]` table as a
//! backward-compatible alias for exactly that one implicit provider (see
//! [`RawConfig::resolved_providers`]). Everything else the narrowing wave
//! retired stays retired: the re-extension adds provider entries, it does
//! not reopen any other knob.
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
//!   live. `[provider]` picks up on `Reload Agent Runtime` (a fresh
//!   `horizon-agentd` process re-reads the file, no full UI restart
//!   needed); `[terminal]`/`[ui]` need a full UI restart.
//! - **Secrets stay out.** Nothing under `[provider]` or `[[providers]]`
//!   accepts an API key — the config file records at most an environment
//!   variable **name** (`[provider]`'s `OPENAI_API_KEY`, `[[providers]]`'s
//!   `api_key_env`; any future provider secret likewise) and the key itself
//!   is environment-only.

pub mod grants;
mod warnings;

/// Deserializes a TOML table into its (key, value) pairs *in document
/// order* — the mechanism [`RawNamedProviderConfig`]'s `models` field uses
/// to keep the file's listing order (TOML 記載順, owner-agreed for the
/// picker). serde's own `Vec<(K, V)>` impl expects a sequence and a plain
/// table would sort keys; this visitor takes the map path instead, whose
/// `next_entry` order is the parsed table's own — document order under the
/// `preserve_order` toml feature.
pub mod ordered_pairs {
    use serde::de::{Deserializer, MapAccess, Visitor};
    use std::fmt;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<(String, String)>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OrderedPairsVisitor;

        impl<'de> Visitor<'de> for OrderedPairsVisitor {
            type Value = Vec<(String, String)>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a map of model alias -> model id")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut pairs = Vec::new();
                while let Some(pair) = map.next_entry::<String, String>()? {
                    pairs.push(pair);
                }
                Ok(pairs)
            }
        }

        deserializer.deserialize_map(OrderedPairsVisitor)
    }
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

pub use grants::{ProjectGrant, RawGrantsConfig, RawProjectGrant};

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
    /// `[[providers]]`: named rig-backed provider entries — the
    /// owner-agreed re-extension of the narrowing wave's single-`[provider]`
    /// surface (see the module doc). Empty unless the file sets it; the
    /// legacy `[provider]` table folds in through
    /// [`RawConfig::resolved_providers`] rather than here.
    pub providers: Vec<RawNamedProviderConfig>,
    /// Which `[[providers]]` `name` runs when nothing else selected it.
    /// `None` means the first effective entry's name
    /// ([`RawConfig::resolved_providers`] owns that rule). A stale name that
    /// matches no entry is warned about and falls back to the first entry.
    pub default_provider: Option<String>,
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

/// One `[[moa]]` entry as the file writes it. Model ids are written out;
/// `[[providers]]`' `models` aliases are not resolved here.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawMoaConfig {
    pub name: String,
    /// The member that writes the answer the pane shows.
    pub aggregator: RawMoaMember,
    /// One read-only `task`-shaped proposer session per entry, each on its
    /// own `{provider, model}`. Duplicates are kept: the same member listed
    /// twice runs twice.
    pub proposers: Vec<RawMoaMember>,
}

/// One `[[moa]]` member: a `[[providers]]` entry name plus a model id.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawMoaMember {
    pub provider: String,
    pub model: String,
}

/// One resolved `[[moa]]` entry — [`RawConfig::resolved_moa`]'s output, with
/// nameless/aggregator-less entries and members naming no `[[providers]]`
/// entry already dropped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMoaConfig {
    pub name: String,
    pub aggregator: ResolvedMoaMember,
    pub proposers: Vec<ResolvedMoaMember>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMoaMember {
    pub provider: String,
    pub model: String,
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

/// Which rig-backed provider *client* an entry builds: the first-scope kinds
/// (owner-agreed: `openai-compatible` + `anthropic`). `openai-compatible`
/// (the default) is any endpoint speaking OpenAI's chat-completions wire
/// (rig's `openai::CompletionsClient`); `anthropic` is rig's
/// `anthropic::Client`. rig-core 0.42 bundles both with no feature gates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RawProviderKind {
    #[default]
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

/// One `[[providers]]` entry as the file writes it. `models` is a map of
/// *alias* -> model id; it deserializes into pairs so the file's own listing
/// order survives — the picker's order is TOML 記載順 (owner-agreed), which
/// the `preserve_order` toml feature backs (the default table would silently
/// sort keys). TOML itself refuses duplicate keys in one table, so an alias
/// can't repeat.
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
    /// Deserialized through [`ordered_pairs`] so the file's listing order
    /// survives (serde's own `Vec<(K, V)>` impl expects a sequence, and a
    /// plain table would sort keys — either would break the order contract
    /// above).
    #[serde(default, with = "ordered_pairs")]
    pub models: Vec<(String, String)>,
}

/// One resolved provider entry — [`RawConfig::resolved_providers`]'s output:
/// every `Option` collapsed, the legacy `[provider]` table folded in, and
/// nameless entries dropped. This is the shape `horizon-agentd` hands to
/// `horizon_agent::config`, which owns the env-var precedence on top
/// (`HORIZON_RIG_MODEL`/`OPENAI_API_KEY`/`OPENAI_BASE_URL`/`ANTHROPIC_API_KEY`
/// /`ANTHROPIC_BASE_URL` keep winning over whatever is set here).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProviderConfig {
    pub name: String,
    pub kind: RawProviderKind,
    pub base_url: Option<String>,
    pub api_key_env: String,
    /// alias -> model id, in file listing order. The first entry is the
    /// provider's own default model — the same document order the picker
    /// shows, so "first" means the same thing to both.
    pub models: Vec<(String, String)>,
}

/// [`RawConfig::resolved_providers`]'s whole output: the effective entry
/// list (file order) and which name is the default.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvidersResolution {
    pub providers: Vec<ResolvedProviderConfig>,
    pub default_name: String,
}

/// The name the legacy `[provider]` table folds in as, when the file has no
/// named `[[providers]]` entries. Deliberately plain: for a single-provider
/// config the picker showing one provider is not improved by renaming it
/// from the file, and `[provider]`-only files must keep working
/// byte-for-byte.
pub const LEGACY_PROVIDER_NAME: &str = "default";

impl RawConfig {
    /// Folds the legacy `[provider]` table and the `[[providers]]` array
    /// into one effective provider list plus a default name. Pure (the
    /// value-level warnings live in [`provider_config_warnings`], run once
    /// per parse beside the name-walking [`warnings::warn`]):
    ///
    /// - `[[providers]]` entries set → those entries, in file order
    ///   (nameless ones dropped — warned). The legacy `[provider]` table is
    ///   IGNORED in this case: a file that names providers has already left
    ///   the single-provider surface, and merging an unnamed entry into a
    ///   named list would make the effective list unreadable from the file.
    /// - No named `[[providers]]` entries (the legacy case, including a file
    ///   with no provider config at all) → one implicit entry named
    ///   [`LEGACY_PROVIDER_NAME`] carrying `[provider]`'s `base_url` and,
    ///   when `[provider].model` is set, that model as its single
    ///   (model -> model) alias pair. This is what keeps
    ///   pre-`[[providers]]` behavior intact: same entry count, same knobs,
    ///   same env precedence (which `horizon_agent::config` resolves on
    ///   top).
    /// - Every entry's `kind`/`api_key_env` `Option`s collapse to their
    ///   defaults ([`RawProviderKind::default`]/[`RawProviderKind::
    ///   default_api_key_env`]).
    /// - Default name: `default_provider` when it names one of the effective
    ///   entries; a stale name falls back to the first entry's name (the
    ///   fallback keeps a renamed or dropped provider from breaking startup —
    ///   the same never-fail-on-a-typo policy the file's other sections
    ///   follow), else the first entry's name.
    pub fn resolved_providers(&self) -> ProvidersResolution {
        let mut providers: Vec<ResolvedProviderConfig> = Vec::new();
        if self.providers.is_empty() {
            let models = match &self.provider.model {
                Some(model) => vec![(model.clone(), model.clone())],
                None => Vec::new(),
            };
            providers.push(ResolvedProviderConfig {
                name: LEGACY_PROVIDER_NAME.to_string(),
                kind: RawProviderKind::OpenAiCompatible,
                base_url: self.provider.base_url.clone(),
                api_key_env: RawProviderKind::OpenAiCompatible
                    .default_api_key_env()
                    .to_string(),
                models,
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
                    models: entry.models.clone(),
                });
            }
        }
        let default_name = match &self.default_provider {
            Some(name) if providers.iter().any(|p| &p.name == name) => name.clone(),
            _ => providers
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| LEGACY_PROVIDER_NAME.to_string()),
        };
        ProvidersResolution {
            providers,
            default_name,
        }
    }
}

/// The picker group (and `set_session_model` `provider` argument) the
/// `[[moa]]` entries are offered under. A `[[providers]]` entry with this
/// name is warned about and loses the name to MoA — see
/// [`moa_config_warnings`].
pub const MOA_PROVIDER_NAME: &str = "moa";

impl RawConfig {
    /// Folds `[[moa]]` into the entries a session can be started on: an
    /// entry needs a name, an aggregator whose `provider` names one of
    /// [`Self::resolved_providers`]' entries, and a non-empty model id.
    /// A proposer failing the same test is dropped on its own, leaving the
    /// entry usable. Warnings live in [`moa_config_warnings`], run once per
    /// parse like the provider ones.
    pub fn resolved_moa(&self) -> Vec<ResolvedMoaConfig> {
        let providers = self.resolved_providers();
        let known = |member: &RawMoaMember| {
            !member.model.is_empty()
                && providers
                    .providers
                    .iter()
                    .any(|entry| entry.name == member.provider)
        };
        let resolve = |member: &RawMoaMember| ResolvedMoaMember {
            provider: member.provider.clone(),
            model: member.model.clone(),
        };
        self.moa
            .iter()
            .filter(|entry| !entry.name.is_empty() && known(&entry.aggregator))
            .map(|entry| ResolvedMoaConfig {
                name: entry.name.clone(),
                aggregator: resolve(&entry.aggregator),
                proposers: entry.proposers.iter().filter(|m| known(m)).map(resolve).collect(),
            })
            .collect()
    }
}

/// Value-level warnings for `[[moa]]`, beside [`provider_config_warnings`]
/// and with the same warn-and-continue policy: an entry this crate refused
/// (no name, an aggregator naming no `[[providers]]` entry, a member with no
/// model id) is named on stderr rather than silently missing from the
/// picker.
pub fn moa_config_warnings(config: &RawConfig) -> Vec<String> {
    let providers = config.resolved_providers();
    let mut warnings = Vec::new();
    if config.moa.is_empty() {
        return warnings;
    }
    if providers
        .providers
        .iter()
        .any(|entry| entry.name == MOA_PROVIDER_NAME)
    {
        warnings.push(format!(
            "[[providers]]: the name {MOA_PROVIDER_NAME:?} is reserved for the [[moa]] group in \
             model selection — rename that provider entry, it cannot be selected"
        ));
    }
    let describe = |member: &RawMoaMember| {
        if member.model.is_empty() {
            Some("has no model id".to_string())
        } else if !providers
            .providers
            .iter()
            .any(|entry| entry.name == member.provider)
        {
            Some(format!(
                "names no [[providers]] entry ({:?})",
                member.provider
            ))
        } else {
            None
        }
    };
    let mut seen: Vec<&str> = Vec::new();
    for (index, entry) in config.moa.iter().enumerate() {
        if entry.name.is_empty() {
            warnings.push(format!(
                "[[moa]]: entry {index} has no name, dropping it (name it so it can be selected)"
            ));
            continue;
        }
        if seen.contains(&entry.name.as_str()) {
            warnings.push(format!(
                "[[moa]]: duplicate name {} — the later entry is shadowed",
                entry.name
            ));
        } else {
            seen.push(entry.name.as_str());
        }
        if let Some(reason) = describe(&entry.aggregator) {
            warnings.push(format!(
                "[[moa]]: entry {:?} aggregator {reason}, dropping the whole entry",
                entry.name
            ));
            continue;
        }
        for (position, proposer) in entry.proposers.iter().enumerate() {
            if let Some(reason) = describe(proposer) {
                warnings.push(format!(
                    "[[moa]]: entry {:?} proposer {position} {reason}, dropping that proposer",
                    entry.name
                ));
            }
        }
        if entry.proposers.is_empty() {
            warnings.push(format!(
                "[[moa]]: entry {:?} lists no proposers — the aggregator will answer alone",
                entry.name
            ));
        }
    }
    warnings
}

/// Value-level warnings for the provider sections, beside the name-walking
/// [`warnings::warn`] (same warn-and-continue policy, never fail startup).
/// Pure: collected here, printed by [`read_config`] once per successful
/// parse. Covers what a name walk can't see:
/// - a file that sets BOTH `[[providers]]` and the legacy `[provider]`
///   table — `[[providers]]` wins and the legacy table is dead weight the
///   reader should drop;
/// - a `default_provider` naming no effective entry;
/// - a `[[providers]]` entry with no `name` (it would resolve as an unnamed
///   provider nothing can select);
/// - a duplicate `[[providers]]` `name` (the later entry is shadowed for
///   selection).
pub fn provider_config_warnings(config: &RawConfig) -> Vec<String> {
    let resolution = config.resolved_providers();
    let mut warnings = Vec::new();
    if !config.providers.is_empty()
        && (config.provider.model.is_some() || config.provider.base_url.is_some())
    {
        warnings.push(
            "[provider]: ignored because [[providers]] is set — [[providers]] wins; drop the legacy [provider] table"
                .to_string(),
        );
    }
    if let Some(name) = &config.default_provider {
        if !resolution.providers.iter().any(|p| &p.name == name) {
            warnings.push(format!(
                "default_provider: {name:?} names no [[providers]] entry — falling back to the first entry"
            ));
        }
    }
    for (index, entry) in config.providers.iter().enumerate() {
        if entry.name.is_empty() {
            warnings.push(format!(
                "[[providers]]: entry {index} has no name, dropping it (name it so default_provider can select it)"
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

/// `[theme.ansi]`: the six normal-hue ANSI color slots, each an optional
/// `#rrggbb`/`#rgb` hex string -- the seed's own hue set
/// (`docs/theme-design.md`'s 2026-07-16 "config surface narrowed to the
/// seed" decision; the ten bright/black/white slots are derived-only and no
/// longer configurable). See `ui::theme::ansi` for the built-in defaults
/// each falls back to when unset here.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
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
            // Retired/unrecognized-key warnings (`[agent]`/`[provider]`/
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
            // The provider sections' *value* warnings (the
            // `[provider]`/`[[providers]]` coexistence rule, a stale
            // `default_provider`, nameless/duplicate entries) are likewise
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

fn parse(contents: &str) -> Result<RawConfig, toml::de::Error> {
    toml::from_str(contents)
}

#[cfg(test)]
mod tests;
