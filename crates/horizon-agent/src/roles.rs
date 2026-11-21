//! A minimal role registry: a role id maps to an extra system-prompt
//! section, a tool allowlist, an optional model override, an optional
//! turn-cap override, whether repository instructions are ingested, and the
//! skills to advertise (see
//! `skills`) -- nothing more. The retired plan
//! `docs/plans/agent-foundation/03-roles-and-config-agent.md` (removed in a8839fc1)
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
//! [`RoleId`] is the wire/contract-level identifier (`hosting::SessionNew`,
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
/// JSON (`hosting::SessionNew`/`SessionSummary`) and the persisted event log
/// (`persistence::event_log::Record`) unchanged.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RoleId(pub String);

/// A single role: a static, compile-time-authored bundle of the handful of
/// things a role is allowed to vary per `docs/research/agent-prompting.md`
/// Part 2.5's extension points, plus which skills (`skills`) it advertises.
/// Deliberately *not* extensible beyond these fields -- see the module doc
/// on keeping the mechanism minimal until a second, differently-shaped role
/// exists to prove out what more it would need. [`EXPLORE_ROLE`] is that
/// second role; what it varies beyond the `config` role is its turn budget
/// and its read-only sandbox ([`Self::uncapped_turns`],
/// [`Self::read_only`]).
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
    /// Whether sessions with this role run with no turn-loop budget at all
    /// (`config::RigAgentConfig::iteration_cap` set to `None` by
    /// `providers::rig::role_adjusted_config`). `false` keeps the built-in
    /// [`crate::config::DEFAULT_ITERATION_CAP`]. Set for [`EXPLORE_ROLE`]:
    /// a delegated child has no budget (`docs/agent-subagent-design.md`
    /// decision 2) -- what stops it is what stops any session.
    pub uncapped_turns: bool,
    /// Whether `bash` in sessions with this role runs in a sandbox with no
    /// writable root, so the operating system keeps it from writing
    /// (`docs/agent-subagent-design.md` decision 7). Every write it attempts
    /// is refused as ungrantable rather than offered as a grant.
    pub read_only: bool,
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
    /// Whether this is a *standing* role -- a long-lived, context-carrying
    /// agent that maintains a memory document across turns
    /// (`docs/standing-agent-memory-design.md`). A standing role's session
    /// gets the `memory_update` tool, a turn-end checkpoint that forces it
    /// to update (or explicitly skip) its memory each turn, and a
    /// provider-view projection that prepends the current memory document
    /// in place of the old turn-by-turn history. The board keeper (removed
    /// in 8e057dd7) was the first standing role; today the board flow's roles
    /// (`horizon-agentd`'s `board_flow::roles`) set it, and coding sessions
    /// are not standing. Added for the keeper, which needed exactly this
    /// one field the two prior roles didn't.
    pub standing: bool,
    /// Whether nobody watches a session with this role: no pane is attached
    /// to it, so a tool call that would ask for approval is refused instead
    /// of parked (`ToolSessionState::is_unattended`). Such a role is started
    /// by the harness, never by the user, so [`user_launchable`] leaves it
    /// out. The exploration role and the wiki's thread role set it.
    pub unattended: bool,
}

/// Horizon's configuration assistant: the first role -- the concrete
/// second use case `docs/research/
/// agent-prompting.md` Part 2's audit said was needed before committing to
/// a role design. Helps the user adjust `[theme]`/`[theme.ansi]`/
/// `[keybindings]` in Horizon's config file conversationally, via the
/// `horizon-config` skill and the `config.*`/`skill_read` tools -- see
/// `tools::config` and the skill's own `SKILL.md` for the mechanics.
///
/// `include_repository_instructions: false`: this role can write
/// `~/.config/horizon/config.toml` (or wherever `HORIZON_CONFIG` points),
/// a single host-owned file the `config_write` tool deliberately reaches
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
    allowed_tool_ids: Some(&["skill_read", "config_read", "config_write"]),
    model: None,
    uncapped_turns: false,
    read_only: false,
    include_repository_instructions: false,
    skill_ids: &["horizon-config"],
    standing: false,
    unattended: false,
};

const CONFIG_ROLE_PROMPT_SECTION: &str = "You are Horizon's configuration assistant: you help \
     the user adjust Horizon's color theme and keybindings by editing its config file \
     conversationally.\n\
     \n\
     Before proposing any change, read the `horizon-config` skill (via `skill_read`) and the \
     user's current config file (via `config_read`) -- do not guess at the file's format, \
     valid names, or current contents.\n\
     \n\
     Apply a change by writing the complete file with `config_write`, preserving every existing \
     entry the user didn't ask you to change -- never write a partial file.\n\
     \n\
     Theme and keybinding changes apply automatically once the user approves a `config_write`; \
     no restart is needed for those two sections, though other sections of the file still \
     require one. You have no filesystem or shell access beyond `skill_read`/`config_read`/\
     `config_write` -- do not suggest running commands or reading other files.";

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

/// A parallel exploration session (`docs/agent-explore-design.md`): spawned
/// by another session's `task` call to answer one question about the shared
/// workspace, and terminated as soon as it has.
///
/// The read mode of `task` (`docs/agent-subagent-design.md`): read-only
/// filesystem, history, and project-knowledge tools, all
/// `ToolPermission::AutoAllowRead`, plus `bash` in a sandbox with no
/// writable root ([`RoleDefinition::read_only`]), so it can read Git
/// history and run other read-only commands while the operating system
/// keeps it from writing. It runs with no turn budget
/// ([`RoleDefinition::uncapped_turns`]). The allowlist does not
/// keep approvals out of the session -- a read whose path escapes the
/// workspace root is still routed to the approval gate -- and such a
/// prompt is shown to the human through the requesting session
/// (`ExplorationHost::forward_approval`,
/// `docs/agent-subagent-design.md` decision 4). `task`
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
    allowed_tool_ids: Some(&[
        "fs_read",
        "fs_grep",
        "fs_glob",
        "recall_search",
        "recall_read",
        "recall_sessions",
        "knowledge_read",
        "bash",
        "bash_output",
        "bash_cancel",
    ]),
    model: None,
    uncapped_turns: true,
    read_only: true,
    include_repository_instructions: true,
    skill_ids: &[],
    standing: false,
    unattended: false,
};

const EXPLORE_ROLE_PROMPT_SECTION: &str = "You are an exploration session: another agent asked \
     you one question about this workspace and will read only your final message, never your \
     tool calls. There is no human to ask; take the most plausible reading of an ambiguous \
     question. Answer with concrete paths, line numbers, and findings, complete enough that the \
     requester need not repeat your reading.\n\
     \n\
     Your toolset is read-only: fs_read, fs_grep, fs_glob, recall_search, recall_read, \
     recall_sessions, knowledge_read, and bash (with bash_output and bash_cancel for its \
     background jobs). bash runs in a sandbox where nothing is writable -- not the workspace, \
     not a temporary directory -- so use it for commands that only read, such as `git log`, \
     `git show`, or `git blame`. You cannot edit or create files, and a command that writes, \
     builds, or runs tests is refused. When the answer genuinely needs that, say so in your \
     final message and hand the step back to the requester instead of attempting it.";

/// [`WRITE_TASK_ROLE`]'s id: a persistence and cleanup identity like
/// [`EXPLORE_ROLE_ID`], recognized by [`is_exploration`] alongside it.
pub const WRITE_TASK_ROLE_ID: &str = "task-write";

/// The write mode of `task` (`docs/agent-subagent-design.md` decisions 1,
/// 3 and 10): a child that does a self-contained change in a worktree made
/// for it, branched from its requester's `HEAD`, and commits it on its own
/// branch for the requester to review and integrate. The same mechanism as
/// [`EXPLORE_ROLE`] with write tools: no budget, approvals shown through the
/// requester, and no `task` of its own, so it cannot delegate further.
pub(crate) const WRITE_TASK_ROLE: RoleDefinition = RoleDefinition {
    id: WRITE_TASK_ROLE_ID,
    title: "Task Session",
    prompt_section: WRITE_TASK_ROLE_PROMPT_SECTION,
    allowed_tool_ids: Some(&[
        "fs_read",
        "fs_grep",
        "fs_glob",
        "fs_write",
        "fs_edit",
        "bash",
        "bash_output",
        "bash_cancel",
        "recall_search",
        "recall_read",
        "recall_sessions",
        "knowledge_read",
        "skill_read",
        "web_search",
        "web_fetch",
        "request_sandbox_area",
    ]),
    model: None,
    uncapped_turns: true,
    read_only: false,
    include_repository_instructions: true,
    skill_ids: &[],
    standing: false,
    unattended: false,
};

const WRITE_TASK_ROLE_PROMPT_SECTION: &str = "You are a task session: another agent handed you \
     one self-contained change and will read only your final message, never your tool calls. \
     You work in a git worktree made for you, on a branch of your own, branched from where the \
     requester was; its other sessions may be working in other worktrees of the same \
     repository at the same time. There is no human to ask; take the most plausible reading of \
     an ambiguous instruction, and when an approval is needed it is shown to the human through \
     the requester.\n\
     \n\
     Stay in your worktree: do not change branches, do not touch the requester's checkout or \
     any other worktree, and do not push. Run the checks the repository asks for. When the \
     change is done, commit it on your branch -- the requester reviews and merges that branch. \
     End with what you changed, the commits you made, and what you checked and with what \
     result; say plainly what you could not finish.";

/// Whether `role_id` names an exploration session -- [`EXPLORE_ROLE`], a
/// `task` child. This is the one predicate `horizon-agentd` needs to tell
/// such sessions apart from every other session it hosts, without a wire
/// field of its own (`docs/agent-explore-design.md` decision 8: the role id
/// alone identifies them, so nothing additive had to be added to the
/// session wire). It
/// decides three things: no `task` host of its own,
/// absence from the client-visible session list, and termination rather
/// than resumption at daemon startup.
pub fn is_exploration(role_id: &RoleId) -> bool {
    is_exploration_id(&role_id.0)
}

fn is_exploration_id(id: &str) -> bool {
    id == EXPLORE_ROLE_ID || id == WRITE_TASK_ROLE_ID
}

/// Every role this build knows about. A `Vec`-free static slice since the
/// set is fixed at compile time -- see the module doc on keeping this
/// minimal rather than data-driven.
static ROLES: &[&RoleDefinition] = &[&CONFIG_ROLE, &EXPLORE_ROLE, &WRITE_TASK_ROLE];

/// Roles registered at runtime by the composition root (`horizon-agentd`)
/// from external crates (the board's roles, the wiki's thread role) -- see
/// [`register_external`]. Each package registers its own, so registrations
/// add up; a registered definition is leaked to `'static`, which
/// [`resolve`]'s callers hold on to.
static EXTERNAL_ROLES: std::sync::RwLock<Vec<&'static RoleDefinition>> =
    std::sync::RwLock::new(Vec::new());

fn external_roles() -> Vec<&'static RoleDefinition> {
    EXTERNAL_ROLES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Registers externally-provided roles (from crates `horizon-agent` does not
/// depend on) so [`resolve`] can find them. Called once at `horizon-agentd`
/// startup, before any session is spawned. Each `RoleDefinition`'s `&'static`
/// fields come from `pub const` data in the providing crate (e.g.
/// `horizon_board::agents`' skill texts, registered by `horizon-agentd`'s
/// `board_flow::roles`), so the registration is a by-value copy of thin
/// references -- no allocation, no lifetime extension. See
/// `docs/board-keeper-design.md` §1 for why this indirection exists instead
/// of a Cargo edge. Adds to earlier registrations; a role whose id is
/// already known (built in or registered before) is ignored, so a package
/// may register again.
pub fn register_external(roles: Vec<RoleDefinition>) {
    let mut external = EXTERNAL_ROLES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for role in roles {
        let known = ROLES.iter().any(|known| known.id == role.id)
            || external.iter().any(|known| known.id == role.id);
        if !known {
            external.push(Box::leak(Box::new(role)));
        }
    }
}

/// Every role this build knows about that a user can launch directly —
/// the built-in [`ROLES`] plus any registered via [`register_external`],
/// minus exploration sessions ([`EXPLORE_ROLE`], spawned by the harness,
/// never user-creatable). Called by
/// the shell's view chooser (to populate the list of launchable roles) and
/// its control-plane dispatch (to validate `--role` ids). Each process that
/// links `horizon-agent` and wants to see externally-provided roles must
/// call [`register_external`] at startup — `horizon-agentd` does this for
/// the board keeper role, and the shell does the same so both processes see
/// an identical set, sourced from the same `pub const` data in each
/// providing crate.
pub fn user_launchable() -> Vec<&'static RoleDefinition> {
    let mut roles: Vec<&'static RoleDefinition> = ROLES.to_vec();
    roles.extend(external_roles());
    roles
        .into_iter()
        .filter(|role| !is_exploration_id(role.id) && !role.unattended)
        .collect()
}

/// Resolves `role_id` to its static definition, or `None` if this build
/// doesn't know it. Checks the compile-time [`ROLES`] slice first, then the
/// runtime-registered [`EXTERNAL_ROLES`]. See the module doc: a `None` here
/// must never be silently treated as "no role" by a session-starting caller.
pub fn resolve(role_id: &RoleId) -> Option<&'static RoleDefinition> {
    if let Some(role) = ROLES.iter().copied().find(|role| role.id == role_id.0) {
        return Some(role);
    }
    external_roles()
        .into_iter()
        .find(|role| role.id == role_id.0)
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
        assert_eq!(allowed, &["skill_read", "config_read", "config_write"]);
        assert!(!allowed.contains(&"bash"), "config role must exclude bash");
        assert!(
            !allowed.contains(&"fs_read"),
            "config role must exclude filesystem tools"
        );
        assert!(
            !allowed.contains(&"fs_write"),
            "config role must exclude filesystem tools"
        );
    }

    #[test]
    fn config_role_does_not_ingest_repository_instructions() {
        let role = resolve(&RoleId("config".to_string())).expect("config role must resolve");
        assert!(!role.include_repository_instructions);
    }

    #[test]
    fn config_role_uses_the_session_default_model_and_budget() {
        let role = resolve(&RoleId("config".to_string())).expect("config role must resolve");
        assert_eq!(role.model, None);
        assert!(!role.uncapped_turns);
        assert!(!role.read_only);
    }

    /// `docs/agent-explore-design.md` decision 4: the exploration session's
    /// toolset is exactly the read-only tools, and `task`
    /// itself is absent so recursion cannot be expressed at all.
    #[test]
    fn explore_role_allowlist_is_exactly_the_read_only_tools() {
        let role =
            resolve(&RoleId(EXPLORE_ROLE_ID.to_string())).expect("the explore role must resolve");
        let allowed = role
            .allowed_tool_ids
            .expect("the explore role must restrict its tools");
        assert_eq!(
            allowed,
            &[
                "fs_read",
                "fs_grep",
                "fs_glob",
                "recall_search",
                "recall_read",
                "recall_sessions",
                "knowledge_read",
                "bash",
                "bash_output",
                "bash_cancel",
            ]
        );
        // The prompt counts the same tools out in prose. It exists because a
        // child that does not know its own limits spends its budget asking
        // for a shell it does not have, so a tool added to one list and not
        // the other reintroduces exactly that failure.
        for tool in allowed.iter() {
            assert!(
                role.prompt_section.contains(tool),
                "the explore prompt must name {tool} so the child does not attempt it blindly"
            );
        }
        for forbidden in [
            "task",
            "fs_write",
            "fs_edit",
            "web_fetch",
            "web_search",
            "config_write",
        ] {
            assert!(
                !allowed.contains(&forbidden),
                "the explore role must exclude `{forbidden}`"
            );
        }
    }

    /// Every advertised exploration tool but the shell must be
    /// auto-allowed: an in-workspace call then runs with no approval round
    /// trip at all (decision 4). Out-of-workspace reads are the exception the
    /// prompt the requester shows covers -- see [`EXPLORE_ROLE`]'s doc comment. The
    /// shell is kept from writing by its sandbox instead
    /// ([`RoleDefinition::read_only`]).
    #[test]
    fn every_explore_tool_but_the_shell_is_auto_allowed() {
        for tool_id in EXPLORE_ROLE
            .allowed_tool_ids
            .expect("the explore role must restrict its tools")
            .iter()
            .filter(|tool_id| !tool_id.starts_with("bash"))
        {
            assert_eq!(
                crate::tools::permission_for_tool(tool_id),
                Some(crate::contract::ToolPermission::AutoAllowRead),
                "`{tool_id}` must be auto-allowed for an exploration session"
            );
        }
    }

    #[test]
    fn explore_role_runs_unbudgeted_read_only_and_with_repository_instructions() {
        let role =
            resolve(&RoleId(EXPLORE_ROLE_ID.to_string())).expect("the explore role must resolve");
        assert!(role.uncapped_turns, "a delegated child has no budget");
        assert!(role.read_only, "the read mode's shell must not write");
        assert!(
            role.include_repository_instructions,
            "an exploration reads the requester's own repository and wants its instructions"
        );
        assert_eq!(role.model, None, "v1 has no cheap-model override");
    }

    #[test]
    fn is_exploration_matches_the_explore_role_only() {
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
            allowed_tool_ids: Some(&["fs_read"]),
            model: None,
            uncapped_turns: false,
            read_only: false,
            include_repository_instructions: false,
            skill_ids: &[],
            standing: false,
            unattended: false,
        }]);
        let role = resolve(&RoleId("test-external-role".to_string()))
            .expect("external role must resolve after registration");
        assert_eq!(role.id, "test-external-role");
        assert_eq!(
            role.allowed_tool_ids,
            Some(&["fs_read"][..]),
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
            uncapped_turns: false,
            read_only: false,
            include_repository_instructions: false,
            skill_ids: &[],
            standing: false,
            unattended: false,
        }]);
        // Built-in roles must still be found.
        assert!(resolve(&RoleId("config".to_string())).is_some());
        assert!(resolve(&RoleId(EXPLORE_ROLE_ID.to_string())).is_some());
        // The external one too.
        assert!(resolve(&RoleId("test-additive".to_string())).is_some());
        // And a genuinely unknown id is still None.
        assert!(resolve(&RoleId("no-such-role".to_string())).is_none());
    }

    /// Each package registers its own roles: a later registration adds to
    /// the earlier ones, and a known id keeps its first definition.
    #[test]
    fn registrations_add_up_and_a_known_id_is_kept() {
        let role = |id: &'static str, title: &'static str| RoleDefinition {
            id,
            title,
            prompt_section: "",
            allowed_tool_ids: Some(&[]),
            model: None,
            uncapped_turns: false,
            read_only: false,
            include_repository_instructions: false,
            skill_ids: &[],
            standing: false,
            unattended: false,
        };
        register_external(vec![role("test-first", "First")]);
        register_external(vec![
            role("test-second", "Second"),
            role("test-first", "Replacement"),
            role("config", "Replacement"),
        ]);
        let title = |id: &str| resolve(&RoleId(id.to_string())).map(|role| role.title);
        assert_eq!(title("test-first"), Some("First"));
        assert_eq!(title("test-second"), Some("Second"));
        assert_eq!(title("config"), Some("Configuration Agent"));
        let launchable = user_launchable();
        assert_eq!(
            launchable
                .iter()
                .filter(|role| role.id == "test-first")
                .count(),
            1
        );
    }

    /// `user_launchable` must include the config role and any externally
    /// registered roles, but never the explore role (a delegated sub-session
    /// that is never user-creatable).
    #[test]
    fn user_launchable_includes_config_excludes_explore() {
        let roles = user_launchable();
        let ids: Vec<&str> = roles.iter().map(|r| r.id).collect();
        assert!(ids.contains(&"config"), "config must be user-launchable");
        assert!(
            !ids.contains(&EXPLORE_ROLE_ID),
            "explore must never be user-launchable"
        );
    }
}
