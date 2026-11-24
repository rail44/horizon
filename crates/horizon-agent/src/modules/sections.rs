//! The built-in prompt sections, each a module of the resolve stage. An
//! untrusted project gets embedded skills only, no repository instructions
//! and no knowledge index (`skills`' module doc on trust).

use super::{ResolveContext, Section, SessionModule};

/// The delegation-routing block, for a session offered `task`.
pub(super) struct DelegationRouting;

impl SessionModule for DelegationRouting {
    fn sections(&self, cx: &ResolveContext<'_>) -> Vec<Section> {
        if !advertises_task_tool(cx) {
            return Vec::new();
        }
        vec![Section {
            name: "delegation",
            text: crate::prompt::DELEGATION_ROUTING_SECTION.to_string(),
        }]
    }
}

/// The session's role's own section.
pub(super) struct RoleSection;

impl SessionModule for RoleSection {
    /// A role that lists its tools is offered those alone, and `task_output`
    /// only with `task`.
    fn withholds_tool(&self, cx: &ResolveContext<'_>, tool_id: &str) -> bool {
        let Some(allowed) = cx.config.allowed_tool_ids.as_deref() else {
            return false;
        };
        let allows = |id: &str| allowed.iter().any(|allowed| allowed == id);
        !allows(tool_id)
            || (tool_id == crate::tools::TASK_OUTPUT_TOOL_ID && !allows(crate::tools::TASK_TOOL_ID))
    }

    fn sections(&self, cx: &ResolveContext<'_>) -> Vec<Section> {
        match cx.role {
            Some(role) if !role.prompt_section.is_empty() => vec![Section {
                name: "role",
                text: role.prompt_section.to_string(),
            }],
            _ => Vec::new(),
        }
    }
}

/// The skills listing: the role's skills, or every skill for a role-less
/// session.
pub(super) struct Skills;

impl SessionModule for Skills {
    fn sections(&self, cx: &ResolveContext<'_>) -> Vec<Section> {
        let packages = &cx.config.packages;
        let skills = if cx.trusted_project {
            crate::skills::SkillRegistry::discover(&cx.environment.cwd, packages)
        } else {
            crate::skills::SkillRegistry::embedded(packages)
        };
        let section = match cx.role {
            Some(role) => skills.prompt_section_for_ids(role.skill_ids),
            None => skills.prompt_section_for_all(),
        };
        section
            .map(|text| Section {
                name: "skills",
                text,
            })
            .into_iter()
            .collect()
    }
}

/// The repository's `AGENTS.md`/`CLAUDE.md`, or a note that an untrusted
/// project's were not loaded.
pub(super) struct RepositoryInstructions;

impl SessionModule for RepositoryInstructions {
    fn sections(&self, cx: &ResolveContext<'_>) -> Vec<Section> {
        if !cx.trusted_project {
            // The absence of AGENTS.md is deliberate here, not an anomaly
            // for the model to work around.
            return vec![Section {
                name: "repository",
                text: "Repository content not loaded (untrusted project) — AGENTS.md/CLAUDE.md \
                       and .horizon/skills/ were not injected into this prompt; this is \
                       expected, not an error."
                    .to_string(),
            }];
        }
        if !cx
            .role
            .is_none_or(|role| role.include_repository_instructions)
        {
            return Vec::new();
        }
        crate::instructions::extra_sections(
            &cx.environment.cwd,
            cx.config.repository_instructions_cap_chars,
        )
        .into_iter()
        .map(|text| Section {
            name: "repository",
            text,
        })
        .collect()
    }
}

/// The project knowledge index: `id: description` for every active entry
/// in the user-side store of this session's project.
pub(super) struct KnowledgeIndex;

impl SessionModule for KnowledgeIndex {
    /// The knowledge tools read and write the project's own records, so an
    /// untrusted project's session is not offered them.
    fn withholds_tool(&self, cx: &ResolveContext<'_>, tool_id: &str) -> bool {
        !cx.trusted_project && matches!(tool_id, "knowledge_read" | "knowledge_write")
    }

    fn sections(&self, cx: &ResolveContext<'_>) -> Vec<Section> {
        if !cx.trusted_project {
            return Vec::new();
        }
        crate::knowledge::prompt_section(&cx.environment.cwd)
            .map(|text| Section {
                name: "knowledge",
                text,
            })
            .into_iter()
            .collect()
    }
}

/// Whether this session is offered the `task` tool: the role-adjusted
/// allowlist names it, or there is none. The routing block's wording has no
/// "when it is available" clause, so it is left out of a prompt whose
/// session cannot delegate (an exploration session).
pub(crate) fn advertises_task_tool(cx: &ResolveContext<'_>) -> bool {
    match &cx.config.allowed_tool_ids {
        Some(allowed) => allowed.iter().any(|id| id == crate::tools::TASK_TOOL_ID),
        None => true,
    }
}
