//! A minimal role registry: a role id maps to an extra system-prompt
//! section, a tool allowlist, an optional model override, whether
//! repository instructions are ingested, and the skills to advertise (see
//! `skills`) -- nothing more. `docs/plans/agent-foundation/03-roles-and-config-agent.md`
//! deliberately keeps this a static mapping rather than a "role framework":
//! whether a domain agent should be an agent-defined role or a
//! skill-specialized generic coder is an open question the owner has not
//! settled, and this implementation (the `config` role below) is the
//! evidence-gathering exercise for that decision, not a bet on either
//! answer. `docs/research/agent-prompting.md` Part 2.5/2.6 is the audit
//! that identified `prompt::system_prompt`'s `extra_sections` and
//! `RigAgentConfig::allowed_tool_ids` as the two back-compatible extension
//! points a role would need; this module is the first thing that actually
//! populates them.
//!
//! [`RoleId`] is the wire/contract-level identifier (`wire::SessionNew`,
//! `contract::StartSession`/`Initialization`, `persistence::event_log::
//! Record`); [`resolve`] maps it to the static [`RoleDefinition`] a
//! provider builds its per-session config and prompt from
//! (`providers::rig::Provider::start_session`). An unresolvable `RoleId` at
//! session start must never silently degrade to a role-less session --
//! callers that start sessions (currently `contract::ProviderRegistry::
//! start_session`) are responsible for treating `resolve` returning `None`
//! as a hard failure (a session error event), not a fallback.

use serde::{Deserialize, Serialize};

/// The wire/contract-level role identifier -- a `String` newtype in the
/// same style as [`crate::contract::ProviderId`], so it round-trips through
/// JSON (`wire::SessionNew`/`SessionSummary`) and the persisted event log
/// (`persistence::event_log::Record`) unchanged.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RoleId(pub String);

/// A single role: a static, compile-time-authored bundle of the handful of
/// things a role is allowed to vary per `docs/research/agent-prompting.md`
/// Part 2.5's extension points, plus which skills (`skills`) it advertises.
/// Deliberately *not* extensible beyond these fields -- see the module doc
/// on keeping the mechanism minimal until a second, differently-shaped role
/// exists to prove out what more it would need.
pub struct RoleDefinition {
    /// Matches [`RoleId`]'s inner string exactly -- see [`resolve`].
    pub id: &'static str,
    /// Human-readable name shown in Horizon's second-stage view chooser
    /// (`docs/roadmap.md`'s "Placement-first session creation": the
    /// palette's registry-driven list of kinds + roles a new session can be
    /// created as) -- the only place a role is user-facing outside its own
    /// system prompt.
    pub title: &'static str,
    /// Appended as its own `extra_sections` entry (`prompt::system_prompt`),
    /// right after the base prompt and before the skills listing -- see
    /// `providers::rig::session::spawn_rig_session`'s ordering.
    pub prompt_section: &'static str,
    /// Restricts `rig_tool_definitions`'s advertised tool set
    /// (`config::RigAgentConfig::allowed_tool_ids`). `None` would mean "no
    /// restriction" (today's role-less behavior); every role defined here
    /// is expected to set `Some(..)` -- a role that needed every tool
    /// wouldn't need a role at all.
    pub allowed_tool_ids: Option<&'static [&'static str]>,
    /// Overrides `config::RigAgentConfig::model` for sessions with this
    /// role. `None` means "use the provider's configured model unchanged".
    pub model: Option<&'static str>,
    /// Whether repository `AGENTS.md`/`CLAUDE.md` instructions
    /// (`instructions::extra_sections`) are ingested for a session with
    /// this role. See [`CONFIG_ROLE`]'s doc comment for why the `config`
    /// role sets this to `false`.
    pub include_repository_instructions: bool,
    /// Skill ids (`skills::SkillRegistry::get`) advertised to a session with
    /// this role -- see `skills::SkillRegistry::prompt_section_for_ids`.
    /// Resolved against the session's own composed registry (embedded +
    /// any repository skills discovered from its cwd -- see the `skills`
    /// module doc's v2 update), so a repository skill can override an
    /// embedded one even for a role.
    pub skill_ids: &'static [&'static str],
}

/// Horizon's configuration assistant: the first (and, as of this writing,
/// only) role -- the concrete second use case `docs/research/
/// agent-prompting.md` Part 2's audit said was needed before committing to
/// a role design. Helps the user adjust `[theme]`/`[theme.ansi]`/
/// `[keybindings]` in Horizon's config file conversationally, via the
/// `horizon-config` skill and the `config.*`/`skill.read` tools -- see
/// `tools::config` and the skill's own `SKILL.md` for the mechanics.
///
/// `include_repository_instructions: false`: this role can write
/// `~/.config/horizon/config.toml` (or wherever `HORIZON_CONFIG` points),
/// a single host-owned file the `config.write` tool deliberately reaches
/// outside the usual `workspace_root` confinement to edit (see
/// `tools::config`'s own doc comment). A host-config-writing agent must
/// not also ingest arbitrary repository `AGENTS.md`/`CLAUDE.md` content --
/// unlike a role-less coding session, where the repository *is* the trust
/// boundary, this role's trust boundary is Horizon's own config file, and
/// pulling in instructions from whatever repository happens to be the
/// process's cwd would cross tiers for no benefit to the task at hand. See
/// `docs/trust-boundaries.md`'s tier reasoning.
pub const CONFIG_ROLE: RoleDefinition = RoleDefinition {
    id: "config",
    title: "Configuration Agent",
    prompt_section: CONFIG_ROLE_PROMPT_SECTION,
    allowed_tool_ids: Some(&["skill.read", "config.read", "config.write"]),
    model: None,
    include_repository_instructions: false,
    skill_ids: &["horizon-config"],
};

const CONFIG_ROLE_PROMPT_SECTION: &str = "You are Horizon's configuration assistant: you help \
     the user adjust Horizon's color theme and keybindings by editing its config file \
     conversationally.\n\
     \n\
     Before proposing any change, read the `horizon-config` skill (via `skill.read`) and the \
     user's current config file (via `config.read`) -- do not guess at the file's format, \
     valid names, or current contents.\n\
     \n\
     Apply a change by writing the complete file with `config.write`, preserving every existing \
     entry the user didn't ask you to change -- never write a partial file.\n\
     \n\
     Theme and keybinding changes apply automatically once the user approves a `config.write`; \
     no restart is needed for those two sections, though other sections of the file still \
     require one. You have no filesystem or shell access beyond `skill.read`/`config.read`/\
     `config.write` -- do not suggest running commands or reading other files.";

/// Every role this build knows about. A `Vec`-free static slice since the
/// set is fixed at compile time -- see the module doc on keeping this
/// minimal rather than data-driven.
static ROLES: &[&RoleDefinition] = &[&CONFIG_ROLE];

/// Resolves `role_id` to its static definition, or `None` if this build
/// doesn't know it. See the module doc: a `None` here must never be
/// silently treated as "no role" by a session-starting caller.
pub fn resolve(role_id: &RoleId) -> Option<&'static RoleDefinition> {
    ROLES.iter().copied().find(|role| role.id == role_id.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_finds_the_config_role() {
        let role = resolve(&RoleId("config".to_string())).expect("config role must resolve");
        assert_eq!(role.id, "config");
        assert_eq!(role.skill_ids, &["horizon-config"]);
    }

    #[test]
    fn resolve_returns_none_for_an_unknown_role() {
        assert!(resolve(&RoleId("does-not-exist".to_string())).is_none());
    }

    #[test]
    fn config_role_allowlist_is_exactly_the_three_config_tools() {
        let allowed = CONFIG_ROLE
            .allowed_tool_ids
            .expect("config role must restrict its tools");
        assert_eq!(allowed, &["skill.read", "config.read", "config.write"]);
        assert!(!allowed.contains(&"bash"), "config role must exclude bash");
        assert!(
            !allowed.contains(&"fs.read"),
            "config role must exclude filesystem tools"
        );
        assert!(
            !allowed.contains(&"fs.write"),
            "config role must exclude filesystem tools"
        );
    }

    #[test]
    fn config_role_does_not_ingest_repository_instructions() {
        let role = resolve(&RoleId("config".to_string())).expect("config role must resolve");
        assert!(!role.include_repository_instructions);
    }

    #[test]
    fn config_role_uses_the_provider_default_model() {
        assert_eq!(CONFIG_ROLE.model, None);
    }
}
