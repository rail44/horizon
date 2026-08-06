//! A minimal role registry: a role id maps to an extra system-prompt
//! section, a tool allowlist, an optional model override, an optional
//! turn-cap override, whether repository instructions are ingested, and the
//! skills to advertise (see
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
//! callers that start sessions (currently `registry::ProviderRegistry::
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
/// exists to prove out what more it would need. [`EXPLORE_ROLE`] is that
/// second role, and it needed exactly one field the `config` role didn't
/// ([`Self::iteration_cap`]); everything else it varies was already here.
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
    /// Overrides `config::RigAgentConfig::iteration_cap` -- the turn-loop
    /// guard's consecutive-tool-turn budget -- for sessions with this role.
    /// `None` means "use the built-in [`crate::config::DEFAULT_ITERATION_CAP`]
    /// unchanged". Added for [`EXPLORE_ROLE`], which answers exactly one
    /// question and therefore needs a much tighter budget than an ordinary
    /// coding session (`docs/agent-explore-design.md` decision 7); it is
    /// applied in `providers::rig::role_adjusted_config` alongside the two
    /// overrides above.
    pub iteration_cap: Option<u32>,
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
    /// Whether hitting the iteration cap runs one forced, tools-disabled
    /// completion (an injected "stop and summarize" instruction) before the
    /// turn loop halts, instead of halting straight away
    /// (`providers::rig::session::halt_turn_loop`). `false` preserves the
    /// original "stash the real result, wait for Continue" behavior.
    /// Deliberately role-scoped rather than a config knob or a prompt-only
    /// convention (`docs/research/agent-context-reduction-prior-
    /// art-2026-07-26.md` §4's OpenCode/Hermes precedent: neither leaves it
    /// to the model) -- set for [`EXPLORE_ROLE`], whose whole job is a
    /// single delegated report, so a capped run should still return
    /// whatever it found rather than a bare error
    /// (`docs/agent-explore-design.md`'s 2026-07-27 addendum).
    pub summarize_on_cap: bool,
}

/// Horizon's configuration assistant: the first role -- the concrete
/// second use case `docs/research/
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
    iteration_cap: None,
    include_repository_instructions: false,
    skill_ids: &["horizon-config"],
    summarize_on_cap: false,
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

/// [`EXPLORE_ROLE`]'s id, named as a constant because two things outside
/// this module have to recognize it without constructing a
/// [`RoleDefinition`]: `tools::explore` (which asks the daemon to spawn a
/// session with it) and `horizon-agentd` (which refuses to resume one at
/// startup and keeps it out of the client-visible session list -- see
/// [`is_exploration`]).
///
/// Unchanged by the 2026-07-27 rename of the model-visible tool id to
/// `task` (`tools::explore::TOOL_ID`): this string is a persistence and
/// cleanup identity, written into the event log and matched at daemon
/// startup, and nothing the model ever sees.
pub const EXPLORE_ROLE_ID: &str = "explore";

/// The turn budget an exploration session runs with
/// (`docs/agent-explore-design.md` decision 7): far tighter than
/// [`crate::config::DEFAULT_ITERATION_CAP`], because an exploration answers
/// exactly one question. Hitting it is not a failure of the mechanism --
/// the tool returns whatever report exists and the requester asks something
/// narrower.
const EXPLORE_ITERATION_CAP: u32 = 25;

/// A parallel exploration session (`docs/agent-explore-design.md`): spawned
/// by another session's `task` call to answer one open-ended
/// question about the shared workspace, and terminated as soon as it has.
///
/// The allowlist is the whole restriction mechanism: three read-only tools,
/// every one of them `ToolPermission::AutoAllowRead`, so an exploration can
/// never reach an approval prompt no human is watching for. `task`
/// itself is absent, which is what makes recursion structurally impossible
/// rather than merely discouraged -- and, since 2026-07-27, is also what
/// keeps the prompt's delegation-routing block out of an exploration's own
/// system prompt (`providers::rig::session::advertises_task_tool`).
///
/// `include_repository_instructions: true`, unlike [`CONFIG_ROLE`]: an
/// exploration runs against the *requester's own* workspace root and its
/// entire job is to understand that repository, so it wants exactly the
/// `AGENTS.md`/`CLAUDE.md` context the requester itself has. There is no
/// tier crossing here -- the repository is the subject, and the exploration
/// can only read it.
pub(crate) const EXPLORE_ROLE: RoleDefinition = RoleDefinition {
    id: EXPLORE_ROLE_ID,
    title: "Exploration Session",
    prompt_section: EXPLORE_ROLE_PROMPT_SECTION,
    allowed_tool_ids: Some(&["fs.read", "fs.grep", "fs.glob"]),
    model: None,
    iteration_cap: Some(EXPLORE_ITERATION_CAP),
    include_repository_instructions: true,
    skill_ids: &[],
    summarize_on_cap: true,
};

const EXPLORE_ROLE_PROMPT_SECTION: &str = "You are an exploration session: another agent asked \
     you one open-ended question about the code in this workspace, and you run in parallel with \
     it on the same files.\n\
     \n\
     Answer that question and nothing else. Your access is read-only -- `fs.read`, `fs.grep`, \
     `fs.glob` -- so you cannot edit files, run commands, or delegate further, and there is no \
     human to ask: answer an ambiguous question with your best reading of it and say plainly \
     what stayed uncertain.\n\
     \n\
     Your final message is the entire deliverable. Nothing else you produce survives: the \
     requester never sees your tool calls or their output, only that last message. Write it as \
     a self-contained report -- concrete paths, line numbers, and findings, enough that the \
     requester never has to repeat the reading you just did. Do not narrate your process and do \
     not promise follow-up work.\n\
     \n\
     Finish in one turn. Your turn budget is deliberately tight; if you run out of room to keep \
     searching, report what you found and name exactly what is still unknown rather than \
     continuing to look.";

/// Whether `role_id` names [`EXPLORE_ROLE`] -- the one predicate
/// `horizon-agentd` needs to tell an exploration session apart from every
/// other session it hosts, without a wire field of its own
/// (`docs/agent-explore-design.md` decision 8: the role id alone identifies
/// them, so nothing additive had to be added to the session wire).
pub fn is_exploration(role_id: &RoleId) -> bool {
    role_id.0 == EXPLORE_ROLE_ID
}

/// Every role this build knows about. A `Vec`-free static slice since the
/// set is fixed at compile time -- see the module doc on keeping this
/// minimal rather than data-driven.
static ROLES: &[&RoleDefinition] = &[&CONFIG_ROLE, &EXPLORE_ROLE];

/// Roles registered at runtime by the composition root (`horizon-agentd`)
/// from external crates (e.g. `horizon-board`'s keeper role) -- see
/// [`register_external`]. Populated once at daemon startup, before any
/// session is spawned; a `OnceLock` because `resolve` is called from every
/// session-start path and must be lock-free after init.
static EXTERNAL_ROLES: std::sync::OnceLock<Vec<RoleDefinition>> = std::sync::OnceLock::new();

/// Registers externally-provided roles (from crates `horizon-agent` does not
/// depend on) so [`resolve`] can find them. Called once at `horizon-agentd`
/// startup, before any session is spawned. Each `RoleDefinition`'s `&'static`
/// fields come from `pub const` data in the providing crate (e.g.
/// `horizon_board::keeper`), so the registration is a by-value copy of thin
/// references -- no allocation, no lifetime extension. See
/// `docs/board-keeper-design.md` §1 for why this indirection exists instead
/// of a Cargo edge.
pub fn register_external(roles: Vec<RoleDefinition>) {
    let _ = EXTERNAL_ROLES.set(roles);
}

/// Resolves `role_id` to its static definition, or `None` if this build
/// doesn't know it. Checks the compile-time [`ROLES`] slice first, then the
/// runtime-registered [`EXTERNAL_ROLES`]. See the module doc: a `None` here
/// must never be silently treated as "no role" by a session-starting caller.
pub fn resolve(role_id: &RoleId) -> Option<&'static RoleDefinition> {
    if let Some(role) = ROLES.iter().copied().find(|role| role.id == role_id.0) {
        return Some(role);
    }
    // `OnceLock::get` returns `&Vec<RoleDefinition>` with `'static` lifetime
    // (the OnceLock itself is static), and `Vec::iter` borrows from it -- so
    // the returned `&RoleDefinition` is `'static` too.
    EXTERNAL_ROLES
        .get()
        .and_then(|roles| roles.iter().find(|role| role.id == role_id.0))
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
        assert_eq!(CONFIG_ROLE.iteration_cap, None);
    }

    #[test]
    fn config_role_does_not_summarize_on_cap() {
        let role = resolve(&RoleId("config".to_string())).expect("config role must resolve");
        assert!(
            !role.summarize_on_cap,
            "only a role whose whole job is one delegated report opts into the forced wrap-up"
        );
    }

    /// `docs/agent-explore-design.md` decision 4: the exploration session's
    /// toolset is exactly the three read-only tools, and `task`
    /// itself is absent so recursion cannot be expressed at all.
    #[test]
    fn explore_role_allowlist_is_exactly_the_three_read_only_tools() {
        let role =
            resolve(&RoleId(EXPLORE_ROLE_ID.to_string())).expect("the explore role must resolve");
        let allowed = role
            .allowed_tool_ids
            .expect("the explore role must restrict its tools");
        assert_eq!(allowed, &["fs.read", "fs.grep", "fs.glob"]);
        for forbidden in [
            "task",
            "bash",
            "fs.write",
            "fs.edit",
            "web_fetch",
            "web_search",
            "config.write",
        ] {
            assert!(
                !allowed.contains(&forbidden),
                "the explore role must exclude `{forbidden}`"
            );
        }
    }

    /// Every advertised exploration tool must be auto-allowed, or an
    /// exploration could park on an approval prompt no human is watching
    /// (decision 4).
    #[test]
    fn every_explore_tool_is_auto_allowed() {
        for tool_id in EXPLORE_ROLE
            .allowed_tool_ids
            .expect("the explore role must restrict its tools")
        {
            assert_eq!(
                crate::tools::permission_for_tool(tool_id),
                Some(crate::contract::ToolPermission::AutoAllowRead),
                "`{tool_id}` must be auto-allowed for an exploration session"
            );
        }
    }

    #[test]
    fn explore_role_runs_with_a_tighter_turn_cap_and_repository_instructions() {
        let role =
            resolve(&RoleId(EXPLORE_ROLE_ID.to_string())).expect("the explore role must resolve");
        assert_eq!(role.iteration_cap, Some(EXPLORE_ITERATION_CAP));
        assert!(
            role.iteration_cap
                .is_some_and(|cap| cap < crate::config::DEFAULT_ITERATION_CAP),
            "an exploration answers one question; its budget must be the tighter one"
        );
        assert!(
            role.include_repository_instructions,
            "an exploration reads the requester's own repository and wants its instructions"
        );
        assert_eq!(role.model, None, "v1 has no cheap-model override");
    }

    /// `docs/agent-explore-design.md`'s 2026-07-27 addendum: a capped
    /// exploration must still return whatever it found instead of a bare
    /// error, so the explore role opts into the forced wrap-up completion.
    #[test]
    fn explore_role_summarizes_on_cap() {
        let role =
            resolve(&RoleId(EXPLORE_ROLE_ID.to_string())).expect("the explore role must resolve");
        assert!(role.summarize_on_cap);
    }

    #[test]
    fn is_exploration_only_matches_the_explore_role() {
        assert!(is_exploration(&RoleId(EXPLORE_ROLE_ID.to_string())));
        assert!(!is_exploration(&RoleId("config".to_string())));
        assert!(!is_exploration(&RoleId("exploration".to_string())));
    }

    // -- external role registration (docs/board-keeper-design.md §1) ---------

    /// A role registered via [`register_external`] must be findable by
    /// [`resolve`], exactly like the compile-time [`ROLES`] entries. This is
    /// the mechanism `horizon-agentd` uses to contribute `horizon-board`'s
    /// keeper role without a Cargo edge between the two crates.
    #[test]
    fn register_external_makes_resolve_find_the_role() {
        register_external(vec![RoleDefinition {
            id: "test-external-role",
            title: "Test External Role",
            prompt_section: "You are a test role.",
            allowed_tool_ids: Some(&["fs.read"]),
            model: None,
            iteration_cap: None,
            include_repository_instructions: false,
            skill_ids: &[],
            summarize_on_cap: false,
        }]);
        let role = resolve(&RoleId("test-external-role".to_string()))
            .expect("external role must resolve after registration");
        assert_eq!(role.id, "test-external-role");
        assert_eq!(
            role.allowed_tool_ids,
            Some(&["fs.read"][..]),
            "external role's allowlist must be preserved"
        );
    }

    /// Built-in roles must still resolve after an external registration —
    /// the external registry is additive, not a replacement.
    #[test]
    fn builtin_roles_still_resolve_after_external_registration() {
        register_external(vec![RoleDefinition {
            id: "test-additive",
            title: "Test Additive",
            prompt_section: "",
            allowed_tool_ids: None,
            model: None,
            iteration_cap: None,
            include_repository_instructions: false,
            skill_ids: &[],
            summarize_on_cap: false,
        }]);
        // Built-in roles must still be found.
        assert!(resolve(&RoleId("config".to_string())).is_some());
        assert!(resolve(&RoleId(EXPLORE_ROLE_ID.to_string())).is_some());
        // The external one too.
        assert!(resolve(&RoleId("test-additive".to_string())).is_some());
        // And a genuinely unknown id is still None.
        assert!(resolve(&RoleId("no-such-role".to_string())).is_none());
    }
}
