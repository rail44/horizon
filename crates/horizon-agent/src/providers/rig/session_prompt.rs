//! System-prompt synthesis for a Rig session: building the
//! [`SessionEnvironment`] and the `extra_sections` a session's system
//! prompt is composed from (`prompt::system_prompt`). Extracted from
//! `session.rs` because it has no dependency on the session loop — these
//! run at session start and at the serialized environment activation boundary.

use crate::{
    config::RigAgentConfig, contract::StartSession, prompt::SessionEnvironment,
    roles::RoleDefinition,
};

/// Builds a session's [`SessionEnvironment`] from the `StartSession` request
/// that started it. Extracted as its own function (rather than inlined at
/// its one call site above) so the 2026-07-19 dogfooding fix -- an isolated
/// session's prompt must reflect its own workspace root, not the daemon
/// process's `cwd` -- is directly unit-testable without spinning up the
/// session's real thread.
pub(super) fn session_environment(request: &StartSession) -> SessionEnvironment {
    SessionEnvironment::for_workspace_root(request.workspace_root.as_deref())
}

/// Builds the `extra_sections` a session's system prompt is composed from
/// (`prompt::system_prompt`), in order: the delegation-routing block (only
/// for a session that actually has the `task` tool -- see
/// [`advertises_task_tool`]), the role's own prompt section (if any), then a
/// skills listing, then repository `AGENTS.md`/`CLAUDE.md` instructions --
/// but only when `role.include_repository_instructions` allows it.
///
/// The skills listing (`skills::SkillRegistry`, composed here from this
/// session's own cwd -- see `skills`' module doc for the v2 repository
/// layer) covers *every* skill for a role-less session
/// (`SkillRegistry::prompt_section_for_all`) and just that role's
/// `skill_ids` for a role-bearing one (`SkillRegistry::
/// prompt_section_for_ids`) -- so `role: None` no longer reproduces the
/// pre-v2 prompt byte-for-byte whenever this build has any skill to
/// disclose (it always does -- see `skills`' embedded builtins); it stays
/// byte-identical only in the hypothetical case of an empty registry,
/// exercised directly in `skills`' own tests.
pub(super) fn session_extra_sections(
    modules: &[std::sync::Arc<dyn crate::modules::SessionModule>],
    environment: &SessionEnvironment,
    config: &RigAgentConfig,
    role: Option<&'static RoleDefinition>,
    trusted_project: bool,
) -> Vec<String> {
    let cx = crate::modules::ResolveContext {
        environment,
        config,
        role,
        trusted_project,
    };
    crate::modules::sections(modules, &cx)
        .into_iter()
        .map(|section| section.text)
        .collect()
}
