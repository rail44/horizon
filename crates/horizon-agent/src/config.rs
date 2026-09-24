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
//! The caller translates `[[providers]]` entries into [`NamedProviderConfig`]
//! and passes them to [`AgentConfig::from_env_and_providers`]. Auxiliary AI
//! configuration is resolved separately, independent of the conversation
//! default. Every other former
//! `[agent]`/`[provider]` file knob (tool caps, turn-loop guard
//! thresholds, stream-flush cadence, history/instructions budgets,
//! `max_tokens`) is now a fixed built-in constant -- see each `DEFAULT_*`
//! constant below. `max_tokens` is [`DEFAULT_AGENT_MAX_OUTPUT_TOKENS`],
//! sent explicitly on every agent completion request as of a 2026-07-27
//! audit (`docs/research/agent-ceiling-death-autopsy-2026-07-26.md`):
//! before that audit this sentence's claim was false for `max_tokens` --
//! nothing was sent at all, not even a fixed value, and the retired
//! `[provider] max_tokens` config key's removal had silently become "no
//! `max_tokens` control of any kind" rather than "a fixed built-in one".
//! `temperature`/`top_p` remain that way today: this crate still sends
//! neither on agent completions (both stay unset on the request the same
//! way `max_tokens` used to), so the provider's own default applies --
//! there is no Horizon-side constant standing in for the retired
//! `[provider] temperature` key, and the same audit found no vendor
//! guidance strong enough to justify adding one. `event_log_path`/
//! `state_db_path` similarly lost their file keys; `HORIZON_AGENT_EVENT_LOG`/
//! `HORIZON_AGENT_STATE_DB` plus the XDG-based built-in default remain the
//! only override path.

mod selection;
pub use selection::{resolve_model_selection, ResolvedModelSelection};

use std::path::PathBuf;

use rig_core::providers::openai;

/// Presence gates the rig completion path (see
/// [`RigAgentConfig::api_key_present`]). The **value** of this variable is
/// read directly by `providers::rig::completion` when it builds the provider
/// client for a turn (never from the config file — see the module doc).
/// Horizon only checks whether it is set here, so the session can decide up
/// front whether to attempt the provider path at all or fall back to a
/// deterministic in-process responder (useful offline and in tests).
///
/// With `[[providers]]` this is the *default* variable name for the
/// openai-compatible kind: an entry's `api_key_env` (the variable's name,
/// never a value) overrides it, and the same check runs against that name
/// — the multi-provider generalization of the same rule.
pub(crate) const OPENAI_API_KEY_VAR: &str = "OPENAI_API_KEY";
/// The anthropic kind's base-URL environment variable — the
/// `OPENAI_BASE_URL` counterpart, honored by [`NamedProviderConfig::resolved`]
/// for `kind = anthropic` entries.
pub(crate) const ANTHROPIC_BASE_URL_VAR: &str = "ANTHROPIC_BASE_URL";
pub(crate) const EXA_API_KEY_VAR: &str = "EXA_API_KEY";

/// Overrides the rig completion model id. Falls back to the config file's
/// `[[providers]].default_model`, then [`openai::GPT_4O_MINI`].
const RIG_MODEL_VAR: &str = "HORIZON_RIG_MODEL";

/// Rig/OpenAI's own base-URL env var (already honored implicitly by
/// `openai::CompletionsClient::from_env()`); kept authoritative here too so
/// it wins over `[[providers]].base_url` in the config file, per Horizon's
/// "existing env vars keep working and win" precedence rule.
pub(crate) const OPENAI_BASE_URL_VAR: &str = "OPENAI_BASE_URL";

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

/// Overrides the Tier 1 history-clearing trigger percentage (see
/// [`DEFAULT_CLEARING_TRIGGER_PCT`]), clamped to `1..=100`. Env-only, the
/// same no-file-key treatment `HORIZON_AGENT_EVENT_LOG`/
/// `HORIZON_AGENT_STATE_DB` get: it exists to force clearing to fire in a
/// measurement run (`docs/agent-compaction-design.md`'s "計測スイッチ"),
/// not as a user-facing knob. A value that isn't a number in range is
/// ignored, leaving the built-in default in place.
const CLEARING_THRESHOLD_PCT_VAR: &str = "HORIZON_AGENT_CLEARING_THRESHOLD_PCT";

/// Overrides the enforcing judge's model id (`crate::judge`,
/// `docs/agent-approval-design.md`'s "Judge design"). Falls back to
/// [`DEFAULT_JUDGE_MODEL`]. Env-only, mirroring `HORIZON_AGENT_EVENT_LOG`/
/// `HORIZON_AGENT_STATE_DB`'s no-file-key treatment above -- the config
/// surface is frozen (2026-07-18 config-narrowing wave), so promoting this
/// to a config-file key is a deliberate later decision, not implied by this
/// override existing.
pub(crate) const JUDGE_MODEL_VAR: &str = "HORIZON_AGENT_JUDGE_MODEL";

/// The judge's default model id: a synthetic.new provider-maintained
/// small-model *alias* (`docs/agent-approval-design.md`'s "Judge model"
/// bullet) -- concretely backed by GLM-4.7-Flash
/// today, updated by the provider as better small models ship. Chosen over
/// a raw vendor id (`hf:zai-org/GLM-4.7-Flash`) so Horizon tracks the
/// provider's own small-model choice rather than committing to one
/// vendor's model/governance direction. Never hardcode a raw vendor id in
/// its place -- this constant, or [`JUDGE_MODEL_VAR`]'s override, is the
/// only sanctioned source of the judge's model id.
pub(crate) const DEFAULT_JUDGE_MODEL: &str = "syn:small:text";

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
pub(crate) const DEFAULT_BASH_TIMEOUT_DEFAULT_SECS: u64 = 300;
pub(crate) const DEFAULT_BASH_TIMEOUT_MAX_SECS: u64 = 1_800;
pub(crate) const DEFAULT_BASH_OUTPUT_CAP_CHARS: usize = 30_000;
pub(crate) const DEFAULT_BASH_DRAIN_GRACE_SECS: u64 = 2;
/// Default `fs.read` window when the caller does not choose one. The tool
/// accepts an explicit window up to 2,000 lines, but the smaller default keeps
/// an exploratory read from turning into a whole-file context dump.
pub(crate) const DEFAULT_FS_READ_LINE_CAP: usize = 500;
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
/// Character cap on the composed "Repository instructions" system-prompt
/// section built by `instructions::extra_sections` from `AGENTS.md`/
/// `CLAUDE.md` files found while walking from the session's working
/// directory up to the repository root. 24,000 characters is roughly
/// 4x the size of this repository's own `AGENTS.md` (~6KB at the time this
/// default was chosen), generous enough for a normal single-file repo
/// instruction set while still bounding a worst case (a deep monorepo with
/// an instruction file at every level) -- at a roughly 4-characters-per-token
/// rule of thumb this is ~6,000 tokens. The section is part of every
/// request's fixed preamble, so this cap bounds a cost paid once per turn
/// for the whole session.
pub(crate) const DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS: usize = 24_000;
/// Explicit `max_tokens` sent on every agent completion request (`rig_core`'s
/// `CompletionRequestBuilder::max_tokens`, wired in
/// `providers::rig::completion::rig_openai_turn_streaming`). Before the
/// 2026-07-27 audit this was left unset entirely, which on an
/// OpenAI-compatible backend means "let the backend pick" rather than
/// "unbounded" -- and the backend Horizon has been dogfooding against
/// (synthetic.new) turned out to pick expensively. Its own
/// `GET /openai/v1/models` response declares `context_length: 262144` and
/// `max_output_length: 65536` for both models exercised in the campaign
/// (`hf:moonshotai/Kimi-K2.7-Code` and `hf:MiniMaxAI/MiniMax-M3`, verified
/// live 2026-07-27), and five independent agent-session deaths in that
/// campaign all hit a provider 400 within about 1% of exactly
/// `262144 - 65536 = 196608` input tokens -- consistent with (not proven
/// to be) the backend reserving the model's full declared max output
/// whenever `max_tokens` is omitted. See
/// `docs/research/agent-ceiling-death-autopsy-2026-07-26.md`'s 2026-07-27
/// section for the full audit, including why `temperature`/`top_p` are
/// deliberately NOT given the same treatment.
///
/// 32,768 is chosen from the same campaign's own shape: per-round output
/// there was <=~3k tokens except for rare larger file-writes, so this
/// leaves ample headroom for any single turn while reclaiming roughly half
/// of the previously-reserved-but-unused 65,536-token budget (~33k tokens)
/// as input runway. It also happens to match Moonshot's own hosted API's
/// documented default for this exact model ("Default to be 32k aka
/// 32768", `platform.kimi.ai`'s Kimi K2.7 Code quickstart, 2026-07-27) --
/// notable corroboration, though Horizon talks to synthetic.new (a
/// third-party vLLM/SGLang-style host), not Moonshot's own endpoint, so
/// that match isn't taken as a guarantee of synthetic.new's actual
/// default-selection behavior.
pub(crate) const DEFAULT_AGENT_MAX_OUTPUT_TOKENS: u64 = 32_768;

// --- Tier 1 compaction (reversible tool-result clearing) -------------------
//
// `docs/agent-compaction-design.md`, "閾値と数値" — every one of these is an
// initial value the design doc itself flags as unmeasured, to be adjusted
// once a real session's clearing behavior has been observed. They live here
// beside the other agent defaults rather than in `providers::rig::clearing`
// so the whole tunable surface of this crate stays in one file.

/// Tier 1 clearing fires once the provider's own most recently reported
/// input token count reaches this percentage of the effective window
/// (`context_length − max_output_tokens`). 60% per the design doc's
/// threshold table. Overridable for measurement only, via
/// [`CLEARING_THRESHOLD_PCT_VAR`].
pub(crate) const DEFAULT_CLEARING_TRIGGER_PCT: u32 = 60;

/// A clearing pass only runs when it would recover at least this many
/// tokens' worth of tool-result text. The design doc's recovery floor
/// (16k), borrowed from OpenCode's `PRUNE_MINIMUM`: clearing invalidates
/// the provider's prompt cache from the first cleared message onward, so
/// the loss is worth paying rarely and in bulk, never in a trickle.
pub(crate) const CLEARING_RECOVERY_FLOOR_TOKENS: u64 = 16_384;

/// The most recent tool results are kept verbatim until their combined
/// size reaches this budget; a pass walks oldest-first and stops there.
/// Measured in rounds' worth of text rather than a turn count, per the
/// design doc ("往復単位で遡る（turn 非依存）").
pub(crate) const CLEARING_TAIL_BUDGET_TOKENS: u64 = 16_384;

/// The characters-per-token estimate both the recovery floor and the tail
/// budget are measured with. Deliberately the crude 4:1 rule of thumb: the
/// exact tokenizer is provider- and model-specific, and both budgets are
/// coarse thresholds where a ±30% estimate changes nothing that matters.
pub(crate) const CLEARING_CHARS_PER_TOKEN: u64 = 4;

// Production defaults for fs tool caps; unused under cfg(test), where the
// cfg(test) accessors below select the shrunk *_TEST_DEFAULT values instead.
#[cfg_attr(test, allow(dead_code))]
pub(crate) const FS_GREP_MAX_BYTES_PRODUCTION_DEFAULT: u64 = 64 * 1024 * 1024;
#[cfg_attr(test, allow(dead_code))]
pub(crate) const FS_TRAVERSAL_MAX_FILES_PRODUCTION_DEFAULT: usize = 20_000;
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
    pub auxiliary: Option<crate::auxiliary::AuxiliaryConfig>,
    pub rig: RigAgentConfig,
    /// The whole effective provider surface ([`ProvidersTable`]). The
    /// default entry's resolved view is what `rig` carries, so everything
    /// that reads `rig` keeps reading the default provider unchanged.
    pub providers: ProvidersTable,
    /// Every configured Mixture-of-Agents entry ([`MoaTable`],
    /// `docs/agent-moa-design.md`). Empty for a config file with no
    /// `[[moa]]` section, which is every pre-MoA config.
    pub moa: MoaTable,
    pub persistence: AgentPersistenceConfig,
    pub tools: AgentToolsConfig,
}

impl AgentConfig {
    /// Build provider sessions from named entries already resolved by the file
    /// loader, applying environment precedence once for the accepted catalog.
    pub fn from_env_and_providers(
        entries: Vec<NamedProviderConfig>,
        default_name: String,
        moa: Vec<MoaEntry>,
    ) -> Self {
        // Availability resolves once, here — the one env-read point for the
        // whole surface (callers construct entries from file values only;
        // a mid-session switch re-reads its own target, which is the "a key
        // appearing in the environment is honored at the switch" rule).
        let entries = entries
            .into_iter()
            .map(|mut entry| {
                entry.api_key_present = std::env::var_os(&entry.api_key_env).is_some();
                entry
            })
            .collect();
        let providers = ProvidersTable {
            entries,
            default_name,
        };
        let rig = providers
            .default_entry()
            .map(NamedProviderConfig::resolved)
            .unwrap_or_default();
        // Every MoA member resolves against the same entries, so a member's
        // availability is that entry's availability and is decided here too
        // rather than re-read per pass.
        let resolve_member = |mut member: MoaMember| {
            if let Some(entry) = providers.entry(&member.provider) {
                member.api_key_present = entry.api_key_present;
                member.api_key_env = entry.api_key_env.clone();
            }
            member
        };
        let moa = moa
            .into_iter()
            .map(|entry| MoaEntry {
                name: entry.name,
                aggregator: resolve_member(entry.aggregator),
                proposers: entry.proposers.into_iter().map(resolve_member).collect(),
            })
            .collect();
        Self {
            auxiliary: None,
            rig,
            providers,
            moa: MoaTable { entries: moa },
            persistence: AgentPersistenceConfig::from_env(),
            tools: AgentToolsConfig::default(),
        }
    }
}

/// One `[[moa]]` member: which `[[providers]]` entry runs it, and the model
/// id to run, written out.
///
/// The two resolved fields come from that entry, filled in once by
/// [`AgentConfig::from_env_and_providers`] — the surface's one env-read
/// point. A member is a plain `{provider, model}` pair as the file writes
/// it; callers that construct one directly (tests) state the availability
/// they mean.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoaMember {
    pub provider: String,
    pub model: String,
    /// Whether the named entry's API-key variable was set when the surface
    /// was built. `false` means a session started on it would answer from
    /// the deterministic fallback responder instead of the model, so the
    /// pass skips the member rather than treating that text as an answer.
    pub api_key_present: bool,
    /// The environment variable **name** that entry's key is read from
    /// (never a value), for the message a skipped member is reported with.
    pub api_key_env: String,
}

impl MoaMember {
    /// Why a member on an unavailable entry was not asked, naming the entry and
    /// the variable its key is read from (never a value).
    pub(crate) fn unavailable_reason(&self) -> String {
        if self.api_key_env.is_empty() {
            format!("the `{}` provider is not configured", self.provider)
        } else {
            format!(
                "the `{}` provider's key variable {} is not set",
                self.provider, self.api_key_env
            )
        }
    }

    /// The file-level pair, before [`AgentConfig::from_env_and_providers`]
    /// resolves the entry behind it.
    pub fn new(provider: String, model: String) -> Self {
        Self {
            provider,
            model,
            api_key_present: false,
            api_key_env: String::new(),
        }
    }
}

/// One resolved `[[moa]]` entry: the aggregator that writes the answer, plus
/// the proposers the harness launches as read-only `task`-shaped sessions
/// (`docs/agent-moa-design.md`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoaEntry {
    pub name: String,
    pub aggregator: MoaMember,
    pub proposers: Vec<MoaMember>,
}

/// Every configured MoA entry, in file order — the counterpart of
/// [`ProvidersTable`] for the `moa` selection group. Mirrors
/// `horizon_config`'s own resolution shape without depending on that crate.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MoaTable {
    pub entries: Vec<MoaEntry>,
}

impl MoaTable {
    /// Finds an entry by name — what a selection (`Command::SetSessionModel`
    /// with `provider = "moa"`) resolves against.
    pub fn entry(&self, name: &str) -> Option<&MoaEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

/// The reserved selection-group name MoA entries are offered (and selected)
/// under — `list_providers` reports one [`crate::wire::ProviderSummary`]
/// with this name whose "models" are the `[[moa]]` entry names, and
/// `set_session_model` resolves this `provider` against [`MoaTable`] rather
/// than [`ProvidersTable`]. Mirrors `horizon_config::MOA_PROVIDER_NAME`
/// (this crate has no dependency on `horizon-config` — the two literals must
/// agree); a `[[providers]]` entry that claims the name is warned about
/// there and cannot be selected.
pub const MOA_PROVIDER_NAME: &str = "moa";

/// What a MoA-selected session carries on its [`RigAgentConfig`]: which
/// entry is selected and the proposers to launch for each owner message.
/// The aggregator's own `{provider, model}` is not repeated here — it is
/// already the session's resolved provider bits and `model`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoaPass {
    pub name: String,
    pub proposers: Vec<MoaMember>,
}

/// Which rig-backed provider *client* a turn builds — the owner-agreed
/// first scope (`openai-compatible` + `anthropic`). rig-core 0.42 bundles
/// both clients with no feature gates. Mirrors
/// `horizon_config::RawProviderKind` (this crate has no dependency on
/// `horizon-config` — the caller translates).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProviderKind {
    /// Any endpoint speaking OpenAI's chat-completions wire (rig's
    /// `openai::CompletionsClient`) — the default, and the kind every
    /// pre-`[[providers]]` config resolves to.
    #[default]
    OpenAiCompatible,
    /// rig's `anthropic::Client`.
    Anthropic,
}

impl ProviderKind {
    /// The base-URL environment variable this kind's client honors:
    /// existing env vars keep working and win over the config file, per the
    /// module doc's precedence rule — one variable per kind.
    pub(crate) fn base_url_env(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => OPENAI_BASE_URL_VAR,
            ProviderKind::Anthropic => ANTHROPIC_BASE_URL_VAR,
        }
    }

    /// The endpoint this kind's rig client uses when neither the entry's
    /// `base_url` nor the kind's base-URL env var is set — rig's own
    /// per-kind default (`completion_client` leaves the builder's default in
    /// place for `None`). Named here so callers outside the client (the
    /// picker's `/models` discovery) resolve the same endpoint the client
    /// actually talks to.
    pub(crate) fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => DEFAULT_OPENAI_BASE_URL,
            ProviderKind::Anthropic => DEFAULT_ANTHROPIC_BASE_URL,
        }
    }
}

/// rig's own default endpoint for an OpenAI-compatible client — the value
/// `completion_client` leaves in place when `config.base_url` is `None`.
pub(crate) const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
/// rig's own default endpoint for an Anthropic client.
pub(crate) const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

/// One resolved `[[providers]]` entry as the caller (`horizon-agentd`'s
/// `main` / `AgentdState::reload_provider_config`) translates it from
/// `horizon_config`'s own resolution. This crate owns the env-var
/// precedence on top of it ([`Self::resolved`]). Secrets stay out: the
/// config file records at most the environment variable's **name**
/// ([`Self::api_key_env`]) — never a value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    /// The file's `base_url`, if any — env resolution (the kind's own
    /// base-URL variable) happens in [`Self::resolved`], which is why the
    /// file value is carried unresolved here.
    pub base_url: Option<String>,
    /// The environment variable **name** the entry's API key is read from
    /// (never a value — see [`NamedProviderConfig`]'s doc).
    pub api_key_env: String,
    /// Resolved once by [`AgentConfig::from_env_and_providers`] — the one
    /// env-read point for the whole surface: whether this entry's key
    /// variable was set at build time. `false` means the entry registers
    /// but is unavailable (deterministic fallback turns, "grayed out, not
    /// hidden" to `list_providers`); a mid-session environment change is
    /// honored by a *switch* (`Command::SetSessionModel` re-reads its
    /// target), not by this value.
    pub api_key_present: bool,
    /// The model this entry runs when nothing else selected one — the
    /// file's `default_model`. `None`
    /// leaves the kind's own built-in default in place.
    pub default_model: Option<String>,
}

impl Default for NamedProviderConfig {
    fn default() -> Self {
        Self {
            name: DEFAULT_PROVIDER_NAME.into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: None,
            api_key_env: OPENAI_API_KEY_VAR.into(),
            api_key_present: false,
            default_model: None,
        }
    }
}

impl NamedProviderConfig {
    /// The provider's own live model listing (`GET {base_url}/models`), for
    /// the picker's discovery (`SessionHub::list_provider_models`).
    /// Resolves this entry the same way [`Self::resolved`] resolves its
    /// base URL and key name — the kind's env var, then the entry's
    /// `base_url`, then the kind's own default endpoint — so an Anthropic
    /// entry with no `base_url` asks `api.anthropic.com`, not rig's OpenAI
    /// default. An unavailable entry (no key variable set) or a provider
    /// that answers no listing yields an empty list. Never an error: this
    /// augments the picker, it never blocks a pick.
    pub async fn list_model_ids(&self) -> Vec<String> {
        if !self.api_key_present {
            return Vec::new();
        }
        let api_key = std::env::var(&self.api_key_env).unwrap_or_default();
        crate::providers::rig::list_model_ids(self.kind, &self.discovery_base_url(), &api_key).await
    }

    /// Where this entry's `/models` discovery request goes: the kind's
    /// base-URL env var > the entry's `base_url` > the kind's own default
    /// endpoint (the same endpoint `completion_client` would talk to for
    /// this entry).
    fn discovery_base_url(&self) -> String {
        resolve_base_url(
            std::env::var(self.kind.base_url_env()).ok(),
            self.base_url.clone(),
        )
        .unwrap_or_else(|| self.kind.default_base_url().to_string())
    }
}

impl NamedProviderConfig {
    /// The resolved, per-turn rig view of this entry: Horizon's env
    /// precedence applied on top of the file values (existing env vars keep
    /// working and win) — the entry's own API-key variable (by *name*,
    /// [`Self::api_key_env`]) decides [`RigAgentConfig::api_key_present`],
    /// the kind's own base-URL variable decides the URL, and
    /// `HORIZON_RIG_MODEL` overrides this entry's default model. Env reads
    /// happen at the call site, never stored.
    pub(crate) fn resolved(&self) -> RigAgentConfig {
        RigAgentConfig {
            api_key_present: self.api_key_present,
            model: self.default_model(),
            base_url: resolve_base_url(
                std::env::var(self.kind.base_url_env()).ok(),
                self.base_url.clone(),
            ),
            kind: self.kind,
            api_key_env: self.api_key_env.clone(),
            clearing_threshold_pct: resolve_clearing_threshold_pct(
                std::env::var(CLEARING_THRESHOLD_PCT_VAR).ok(),
            ),
            ..RigAgentConfig::default()
        }
    }

    /// This entry's default model id: `HORIZON_RIG_MODEL` (the standing
    /// env override) > the entry's `default_model` > for openai-compatible
    /// entries the standing [`openai::GPT_4O_MINI`] built-in. An anthropic
    /// entry with no `default_model` has no Horizon-side default model at
    /// all (`resolved`'s view carries the empty string, which
    /// `resolved_model` reports as nothing): claiming a model is in play
    /// when none is would be the same dishonesty the fallback-mode `None`
    /// already refuses.
    fn default_model(&self) -> String {
        let env = std::env::var(RIG_MODEL_VAR).ok();
        match self.kind {
            // The standing precedence, unchanged: env > the entry's
            // default_model > the built-in GPT_4O_MINI default.
            ProviderKind::OpenAiCompatible => resolve_model(env, self.default_model.clone()),
            // An anthropic entry with no default_model has no Horizon-side
            // default model at all (the empty string `resolved_model`
            // reports as nothing): claiming a model is in play when none
            // is would be the same dishonesty the fallback-mode `None`
            // already refuses.
            ProviderKind::Anthropic => env
                .or_else(|| self.default_model.clone())
                .unwrap_or_default(),
        }
    }
}

/// The whole effective provider surface: one entry per resolved
/// `[[providers]]` entry (or the built-in default), plus which entry is the
/// default. Mirrors `horizon_config`'s `ProvidersResolution` shape without
/// depending on that crate.
#[derive(Clone, Debug, PartialEq)]
pub struct ProvidersTable {
    pub entries: Vec<NamedProviderConfig>,
    pub default_name: String,
}

impl ProvidersTable {
    /// The default entry, falling back to the first — the same
    /// never-fail-on-a-stale-name rule `horizon_config`'s own resolution
    /// follows; a default name matching nothing still yields a working
    /// (first-entry) default rather than a panic.
    pub(crate) fn default_entry(&self) -> Option<&NamedProviderConfig> {
        self.entries
            .iter()
            .find(|entry| entry.name == self.default_name)
            .or_else(|| self.entries.first())
    }

    /// Finds an entry by name — a mid-session switch target's lookup
    /// (`Command::SetSessionModel`). An absent name is a validation error.
    pub fn entry(&self, name: &str) -> Option<&NamedProviderConfig> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

/// The no-file default provider name. Mirrors
/// `horizon_config::DEFAULT_PROVIDER_NAME` (this crate has no dependency on
/// `horizon-config` — the two literals must agree).
pub(crate) const DEFAULT_PROVIDER_NAME: &str = "default";

/// Rig provider configuration: model/base-URL selection (`[[providers]]`, plus
/// the env vars above), the turn-loop guard's fixed thresholds
/// (`iteration_cap`/`doom_loop_window`, always [`DEFAULT_ITERATION_CAP`]/
/// [`DEFAULT_DOOM_LOOP_WINDOW`] -- see `providers::rig::session`'s
/// `TurnLoopGuard`, which this is threaded into unchanged) — and the
/// streamed-delta coalescing cadence (always [`DEFAULT_STREAM_FLUSH_INTERVAL_MS`]/
/// [`DEFAULT_STREAM_FLUSH_CHARS`]) used by `providers::rig::stream`.
#[derive(Clone, Debug, PartialEq)]
pub struct RigAgentConfig {
    /// Whether this entry's API-key variable (see [`Self::api_key_env`])
    /// is set. When `false`, the rig provider answers with a deterministic
    /// fallback responder instead of calling the provider (see
    /// `providers::rig::completion::complete_rig_turn`). Renamed from
    /// `api_key_present` with `[[providers]]`: which *variable* decides it
    /// is now per-entry, so the name had to stop claiming OpenAI.
    pub api_key_present: bool,
    /// Which provider client this config builds (`[[providers]]`'s `kind`,
    /// [`ProviderKind`]). `openai-compatible` (the default) is every
    /// pre-`[[providers]]` config.
    pub kind: ProviderKind,
    /// The environment variable **name** this config's API key is read
    /// from (never a value — the module doc's secrets-stay-out rule). The
    /// default variable is [`OPENAI_API_KEY_VAR`]; a `[[providers]]`
    /// entry's is its own `api_key_env`.
    pub api_key_env: String,
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
    /// Character cap applied to the composed "Repository instructions"
    /// system-prompt section -- see
    /// [`DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS`] for why 24,000 was
    /// chosen. Always that constant. Read by `providers::rig::session::
    /// spawn_rig_session` when it builds that section via
    /// `instructions::extra_sections`.
    pub repository_instructions_cap_chars: usize,
    /// Explicit `max_tokens` sent on every agent completion request via
    /// `rig_core`'s `CompletionRequestBuilder::max_tokens` -- see
    /// [`DEFAULT_AGENT_MAX_OUTPUT_TOKENS`] for why this exists and how
    /// 32,768 was chosen. Always that constant.
    pub max_output_tokens: u64,
    /// Percentage of the effective context window at which Tier 1 history
    /// clearing fires (`providers::rig::clearing`). Always
    /// [`DEFAULT_CLEARING_TRIGGER_PCT`] unless
    /// [`CLEARING_THRESHOLD_PCT_VAR`] overrides it for a measurement run --
    /// kept as a field (rather than reading the constant at the use site)
    /// for the same reason `iteration_cap` is: so a test can construct a
    /// config that trips the threshold without simulating a real window.
    pub clearing_threshold_pct: u32,
    /// Restricts which tool ids `providers::rig::completion::
    /// rig_tool_definitions` advertises to the provider. `None` (the only
    /// value [`AgentConfig::from_env_and_providers`] itself ever produces -- this
    /// field is process-wide config, not per-session) means "no
    /// restriction, every tool in `tools::definitions()`" -- current
    /// behavior, unchanged. This back-compatible extension point
    /// (`docs/research/agent-prompting.md` Part 2.5) now has its first
    /// consumer: `providers::rig::Provider::start_session` derives a
    /// per-session `RigAgentConfig` with `Some(..)` here when the session
    /// has a role that restricts tools (`roles::RoleDefinition::
    /// allowed_tool_ids`).
    pub allowed_tool_ids: Option<Vec<String>>,
    /// Whether this session's project root is in the user's
    /// `trusted_projects` list — threaded in from
    /// `StartSession::trusted_project` by `spawn_rig_session` so
    /// `rig_tool_definitions` can filter the knowledge tools
    /// (`knowledge.read`/`knowledge.write`) out of the advertised
    /// catalog for untrusted sessions. Defaults to `false`
    /// (fail-closed), matching `StartSession::trusted_project`'s own
    /// `#[serde(default)]`.
    pub trusted_project: bool,
    /// `Some` when this session runs as a Mixture-of-Agents aggregator
    /// (`docs/agent-moa-design.md`): the provider bits and `model` above are
    /// the aggregator's, and every owner message opens a MoA pass that first
    /// launches these proposers. `None` — the value every non-MoA selection
    /// produces — is ordinary single-model behavior, unchanged.
    pub moa: Option<MoaPass>,
}

impl Default for RigAgentConfig {
    fn default() -> Self {
        Self {
            api_key_present: false,
            kind: ProviderKind::OpenAiCompatible,
            api_key_env: OPENAI_API_KEY_VAR.to_string(),
            model: openai::GPT_4O_MINI.to_string(),
            base_url: None,
            iteration_cap: DEFAULT_ITERATION_CAP,
            doom_loop_window: DEFAULT_DOOM_LOOP_WINDOW,
            stream_flush_interval_ms: DEFAULT_STREAM_FLUSH_INTERVAL_MS,
            stream_flush_chars: DEFAULT_STREAM_FLUSH_CHARS,
            repository_instructions_cap_chars: DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS,
            max_output_tokens: DEFAULT_AGENT_MAX_OUTPUT_TOKENS,
            clearing_threshold_pct: DEFAULT_CLEARING_TRIGGER_PCT,
            allowed_tool_ids: None,
            trusted_project: false,
            moa: None,
        }
    }
}

/// The per-turn rig view a session selected onto `moa_entry` runs with: the
/// aggregator's `[[providers]]` entry resolved as usual, its model pinned to
/// the aggregator's written-out model id, and the pass itself carried on
/// [`RigAgentConfig::moa`]. `None` when the aggregator names no entry of
/// `table` — the one thing that makes a MoA entry unselectable.
///
/// The registry's build-time construction and a mid-session switch both go
/// through here, so the two cannot resolve an entry differently.
pub(crate) fn moa_session_config(
    table: &ProvidersTable,
    moa_entry: &MoaEntry,
) -> Option<RigAgentConfig> {
    let entry = table.entry(&moa_entry.aggregator.provider)?;
    let mut config = entry.resolved();
    apply_moa_selection(&mut config, table, moa_entry);
    Some(config)
}

/// Swaps `config`'s provider bits, model, and pass to `moa_entry` in place —
/// the mid-session-switch half of [`moa_session_config`], which keeps the
/// role's non-model overrides (tool restrictions, iteration cap) exactly as
/// an ordinary provider switch does.
pub(crate) fn apply_moa_selection(
    config: &mut RigAgentConfig,
    table: &ProvidersTable,
    moa_entry: &MoaEntry,
) -> bool {
    let Some(entry) = table.entry(&moa_entry.aggregator.provider) else {
        return false;
    };
    apply_provider_entry(config, entry, &moa_entry.aggregator.model);
    config.moa = Some(MoaPass {
        name: moa_entry.name.clone(),
        proposers: moa_entry.proposers.clone(),
    });
    true
}

/// Points `config` at one `[[providers]]` entry running `model`: the
/// provider bits (kind, key variable, base URL, key presence) plus the model
/// id. Shared by the plain provider switch and the MoA selection above so
/// the two resolve an entry identically. Presence and base URL are re-read
/// from the environment here, the same rule a switch has always followed.
pub(crate) fn apply_provider_entry(
    config: &mut RigAgentConfig,
    entry: &NamedProviderConfig,
    model: &str,
) {
    config.kind = entry.kind;
    config.api_key_env = entry.api_key_env.clone();
    config.base_url = resolve_base_url(
        std::env::var(entry.kind.base_url_env()).ok(),
        entry.base_url.clone(),
    );
    config.api_key_present = std::env::var_os(&entry.api_key_env).is_some();
    config.model = model.to_string();
}

/// Pure resolution for the Tier 1 clearing trigger percentage: a parseable
/// env value clamped into `1..=100` wins, anything else (absent, empty,
/// non-numeric, zero) leaves [`DEFAULT_CLEARING_TRIGGER_PCT`] in place.
/// Kept free of I/O for the same testability reason as [`resolve_model`].
///
/// Zero is deliberately rejected rather than clamped up: "0%" would read as
/// "clear on every request", which is not a measurement mode this design
/// supports (a pass is meant to be rare and bulk -- see
/// [`CLEARING_RECOVERY_FLOOR_TOKENS`]).
pub(crate) fn resolve_clearing_threshold_pct(env_value: Option<String>) -> u32 {
    env_value
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|pct| (1..=100).contains(pct))
        .unwrap_or(DEFAULT_CLEARING_TRIGGER_PCT)
}

/// Pure precedence resolution for the rig model id: env var wins, then the
/// config file's `[[providers]].default_model` (already resolved by the caller — see
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
pub(crate) fn resolve_base_url(
    env_value: Option<String>,
    provider_value: Option<String>,
) -> Option<String> {
    env_value.or(provider_value)
}

/// Pure precedence resolution for the judge's model id -- [`JUDGE_MODEL_VAR`]
/// wins, else [`DEFAULT_JUDGE_MODEL`]. No config-file/provider-value input
/// (unlike [`resolve_model`]): the judge model is env-only by design (see
/// [`JUDGE_MODEL_VAR`]'s own doc comment).
pub(crate) fn resolve_judge_model(env_value: Option<String>) -> String {
    env_value.unwrap_or_else(|| DEFAULT_JUDGE_MODEL.to_string())
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
    pub(crate) fn from_env() -> Self {
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
pub(crate) fn agent_data_home_from(xdg_data_home: Option<String>, home: Option<String>) -> PathBuf {
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    match non_empty(xdg_data_home) {
        Some(dir) => PathBuf::from(dir),
        None => match non_empty(home) {
            Some(home) => PathBuf::from(home).join(".local").join("share"),
            None => std::env::temp_dir(),
        },
    }
}

/// Convenience wrapper around [`agent_data_home_from`] that reads the
/// env vars itself, for callers that don't need the parameterized form
/// (testability). The knowledge store (`crate::knowledge`) uses this to
/// resolve its base directory the same way the event log and DuckDB
/// projection do.
pub(crate) fn agent_data_home() -> PathBuf {
    agent_data_home_from(
        std::env::var(XDG_DATA_HOME_VAR).ok(),
        std::env::var(HOME_VAR).ok(),
    )
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
/// built on it (e.g. `horizon-agentd`'s startup rebuild) -- doesn't need
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
        let config = AgentConfig::from_env_and_providers(
            vec![NamedProviderConfig::default()],
            DEFAULT_PROVIDER_NAME.into(),
            Vec::new(),
        );

        assert_eq!(config.rig.iteration_cap, DEFAULT_ITERATION_CAP);
        assert_eq!(config.rig.doom_loop_window, DEFAULT_DOOM_LOOP_WINDOW);
        assert_eq!(config.rig.base_url, None);
        assert_eq!(
            config.rig.stream_flush_interval_ms,
            DEFAULT_STREAM_FLUSH_INTERVAL_MS
        );
        assert_eq!(config.rig.stream_flush_chars, DEFAULT_STREAM_FLUSH_CHARS);
        assert_eq!(
            config.rig.repository_instructions_cap_chars,
            DEFAULT_REPOSITORY_INSTRUCTIONS_CAP_CHARS
        );
        assert_eq!(
            config.rig.max_output_tokens,
            DEFAULT_AGENT_MAX_OUTPUT_TOKENS
        );
        assert_eq!(config.rig.allowed_tool_ids, None);
        // The file loader supplies one built-in entry for an empty file.
        assert_eq!(config.providers.entries.len(), 1);
    }

    #[test]
    fn rig_agent_config_reads_model_and_base_url_from_the_resolved_provider_values() {
        let config = AgentConfig::from_env_and_providers(
            vec![NamedProviderConfig {
                default_model: Some("provider-model".into()),
                base_url: Some("https://provider.invalid".into()),
                ..Default::default()
            }],
            DEFAULT_PROVIDER_NAME.into(),
            Vec::new(),
        );

        assert_eq!(config.rig.model, "provider-model");
        assert_eq!(
            config.rig.base_url,
            Some("https://provider.invalid".to_string())
        );
    }

    /// Every MoA member resolves its availability and key-variable name
    /// from the `[[providers]]` entry it names, at the one point the whole
    /// surface reads the environment.
    #[test]
    fn moa_members_resolve_availability_from_the_entry_they_name() {
        let entries = vec![
            NamedProviderConfig {
                name: "present".to_string(),
                kind: ProviderKind::OpenAiCompatible,
                base_url: None,
                // Always set: this process's own argv[0] path variable is
                // not something a test may mutate, so the fixture uses a
                // variable that is certain to exist instead.
                api_key_env: "PATH".to_string(),
                api_key_present: false,
                default_model: None,
            },
            NamedProviderConfig {
                name: "absent".to_string(),
                kind: ProviderKind::OpenAiCompatible,
                base_url: None,
                api_key_env: "HORIZON_TEST_KEY_NEVER_SET".to_string(),
                api_key_present: false,
                default_model: None,
            },
        ];
        let moa = vec![MoaEntry {
            name: "mix".to_string(),
            aggregator: MoaMember::new("present".to_string(), "m-a".to_string()),
            proposers: vec![
                MoaMember::new("present".to_string(), "m-b".to_string()),
                MoaMember::new("absent".to_string(), "m-c".to_string()),
                MoaMember::new("not-an-entry".to_string(), "m-d".to_string()),
            ],
        }];

        let config = AgentConfig::from_env_and_providers(entries, "present".to_string(), moa);
        let entry = config.moa.entry("mix").expect("the entry resolves");
        assert!(entry.aggregator.api_key_present);
        assert_eq!(entry.aggregator.api_key_env, "PATH");
        assert!(entry.proposers[0].api_key_present);
        assert!(!entry.proposers[1].api_key_present);
        assert_eq!(entry.proposers[1].api_key_env, "HORIZON_TEST_KEY_NEVER_SET");
        // A member naming no entry has nothing to resolve against and stays
        // unavailable, so the pass skips it too.
        assert!(!entry.proposers[2].api_key_present);
        assert!(entry.proposers[2].api_key_env.is_empty());
    }

    #[test]
    fn judge_model_prefers_env_over_default() {
        assert_eq!(
            resolve_judge_model(Some("hf:some/other-model".to_string())),
            "hf:some/other-model"
        );
        assert_eq!(resolve_judge_model(None), DEFAULT_JUDGE_MODEL);
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

    /// Each kind names the endpoint its rig client would actually talk to
    /// when no base URL is configured — an Anthropic entry must not fall
    /// through to rig's OpenAI default on the discovery path.
    #[test]
    fn provider_kinds_name_their_own_default_endpoints() {
        assert_eq!(
            ProviderKind::OpenAiCompatible.default_base_url(),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            ProviderKind::Anthropic.default_base_url(),
            "https://api.anthropic.com"
        );
    }

    /// An Anthropic entry with no `base_url` discovers against the kind's own
    /// default (or its env-var override), never rig's OpenAI endpoint. Env
    /// compared against the same read so the assertion holds whatever the
    /// environment carries.
    #[test]
    fn discovery_base_url_falls_back_to_the_kinds_own_default() {
        let entry = NamedProviderConfig {
            name: "claude".to_string(),
            kind: ProviderKind::Anthropic,
            base_url: None,
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            api_key_present: true,
            default_model: None,
        };
        let expected = resolve_base_url(std::env::var(ANTHROPIC_BASE_URL_VAR).ok(), None)
            .unwrap_or_else(|| "https://api.anthropic.com".to_string());
        assert_eq!(entry.discovery_base_url(), expected);
    }
}
