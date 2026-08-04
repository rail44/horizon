//! System-prompt synthesis for a Rig session: building the
//! [`SessionEnvironment`] and the `extra_sections` a session's system
//! prompt is composed from (`prompt::system_prompt`). Extracted from
//! `session.rs` because it has no dependency on the session loop — these
//! run once at session start, before the loop begins.

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
/// but only when `role.include_repository_instructions` allows it
/// (`roles::CONFIG_ROLE` sets this `false`; see its own doc comment for
/// why).
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
    environment: &SessionEnvironment,
    config: &RigAgentConfig,
    role: Option<&'static RoleDefinition>,
    trusted_project: bool,
) -> Vec<String> {
    // Untrusted project: embedded skills only (no `.horizon/skills/`
    // discovery), and no `AGENTS.md`/`CLAUDE.md` injection — see the `skills`
    // module doc's trust note (owner decision 2026-08-05). Embedded skills
    // are ship-native, so they stay advertised; the repository layer is the
    // only thing the gate suppresses.
    let skills = if trusted_project {
        crate::skills::SkillRegistry::discover(&environment.cwd)
    } else {
        crate::skills::SkillRegistry::embedded()
    };
    let mut sections = Vec::new();
    if advertises_task_tool(config) {
        sections.push(crate::prompt::DELEGATION_ROUTING_SECTION.to_string());
    }
    let include_repository_instructions = match role {
        Some(role) => {
            sections.push(role.prompt_section.to_string());
            if let Some(skills_section) = skills.prompt_section_for_ids(role.skill_ids) {
                sections.push(skills_section);
            }
            role.include_repository_instructions
        }
        None => {
            if let Some(skills_section) = skills.prompt_section_for_all() {
                sections.push(skills_section);
            }
            true
        }
    };
    if trusted_project && include_repository_instructions {
        sections.extend(crate::instructions::extra_sections(
            &environment.cwd,
            config.repository_instructions_cap_chars,
        ));
    } else if !trusted_project {
        // One-line note so the model doesn't read the absence of AGENTS.md
        // as an anomaly — the repository's content was deliberately not
        // loaded because the project is untrusted.
        sections.push(
            "Repository content not loaded (untrusted project) — AGENTS.md/CLAUDE.md and \
             .horizon/skills/ were not injected into this prompt; this is expected, not an error."
                .to_string(),
        );
    }
    sections
}

/// Whether this session is actually offered the `task` tool, decided from
/// the same allowlist `completion::rig_tool_definitions` filters the
/// advertised catalog with -- `config` here is already role-adjusted
/// (`super::role_adjusted_config`, applied before `spawn_rig_session`), so
/// `None` means the unrestricted role-less toolset and `Some` is the role's
/// exact list.
///
/// This is the whole conditionality of `prompt::DELEGATION_ROUTING_SECTION`.
/// The probes measured its wording unhedged ("your FIRST action must be
/// task"), so the wording keeps no "when it is available" escape clause;
/// instead the block is simply absent from a prompt whose session has no
/// such tool. An exploration session is exactly that case: its role allows
/// `fs.read`/`fs.grep`/`fs.glob` only (`roles::EXPLORE_ROLE`, which
/// deliberately excludes this tool so explorations cannot recurse), and
/// instructing it to delegate first would be an instruction it could only
/// fail.
fn advertises_task_tool(config: &RigAgentConfig) -> bool {
    match &config.allowed_tool_ids {
        Some(allowed) => allowed.iter().any(|id| id == crate::tools::TASK_TOOL_ID),
        None => true,
    }
}
