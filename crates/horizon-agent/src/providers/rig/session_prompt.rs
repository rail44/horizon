//! System-prompt synthesis for a Rig session: building the
//! [`SessionEnvironment`] and the `extra_sections` a session's system
//! prompt is composed from (`prompt::system_prompt`). Extracted from
//! `session.rs` because it has no dependency on the session loop — these
//! run at session start and at the serialized environment activation boundary.

use crate::{
    config::RigAgentConfig,
    contract::{Event, PromptPrefix, StartSession},
    prompt::SessionEnvironment,
    roles::RoleDefinition,
};

/// Builds a session's [`SessionEnvironment`] from the `StartSession` request
/// that started it. Extracted as its own function (rather than inlined at
/// its one call site above) so the 2026-07-19 dogfooding fix -- an isolated
/// session's prompt must reflect its own workspace root, not the `horizon-agent-acp`
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
#[cfg(test)]
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

/// What the resolve stage gives a session's requests: the system prompt's
/// sections and the tools offered.
#[derive(Clone, Debug, Default)]
pub(super) struct Resolved {
    pub(super) sections: Vec<String>,
    pub(super) tools: Vec<rig_core::completion::ToolDefinition>,
}

/// The resolve stage: the sections `modules` add, and the core's tools less
/// those a module withholds.
pub(super) fn resolve(
    modules: &[std::sync::Arc<dyn crate::modules::SessionModule>],
    environment: &SessionEnvironment,
    config: &RigAgentConfig,
    role: Option<&'static RoleDefinition>,
    trusted_project: bool,
) -> Resolved {
    let cx = crate::modules::ResolveContext {
        environment,
        config,
        role,
        trusted_project,
    };
    let sections = crate::modules::sections(modules, &cx)
        .into_iter()
        .map(|section| section.text)
        .collect();
    let tools = super::completion::rig_tool_definitions(&config.packages, trusted_project)
        .into_iter()
        .filter(|tool| {
            !modules
                .iter()
                .any(|module| module.withholds_tool(&cx, tool.name.as_str()))
        })
        .collect();
    Resolved { sections, tools }
}

/// What the model was last told applies to a session: the sections, and
/// the tools by name. The next change is told from it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Told {
    pub(super) sections: Vec<String>,
    pub(super) tools: Vec<String>,
}

impl Resolved {
    pub(super) fn told(&self) -> Told {
        Told {
            sections: self.sections.clone(),
            tools: (self.tools.iter())
                .map(|tool| tool.name.as_str().to_string())
                .collect(),
        }
    }

    /// The record of a session's prefix: this, under `environment`.
    fn fixed(&self, environment: &SessionEnvironment) -> PromptPrefix {
        PromptPrefix {
            cwd: environment.cwd.display().to_string(),
            os: environment.os.clone(),
            git_repo: environment.git_repo,
            date: environment.date.clone(),
            git_branch: environment.git_branch.clone(),
            sections: self.sections.clone(),
            tools: serde_json::to_value(&self.tools).unwrap_or_default().into(),
        }
    }

    /// The prefix `record` fixed, and the environment it names. `None` for
    /// a record whose tools this build cannot read.
    fn restored(record: &PromptPrefix) -> Option<(SessionEnvironment, Self)> {
        let tools = serde_json::from_value(record.tools.0.clone()).ok()?;
        let environment = SessionEnvironment {
            cwd: record.cwd.clone().into(),
            os: record.os.clone(),
            git_repo: record.git_repo,
            date: record.date.clone(),
            git_branch: record.git_branch.clone(),
        };
        let sections = record.sections.clone();
        Some((environment, Self { sections, tools }))
    }

    /// What a session with `config` and no role would be offered, with
    /// `sections` for its prompt.
    #[cfg(test)]
    pub(super) fn from_config(sections: Vec<String>, config: &RigAgentConfig) -> Self {
        let environment = SessionEnvironment::for_workspace_root(None);
        let modules = crate::modules::builtin(config, None);
        Self {
            sections,
            ..resolve(&modules, &environment, config, None, config.trusted_project)
        }
    }
}

/// What a session starting sends and tells (board #106).
#[derive(Debug)]
pub(super) struct Settled {
    /// The environment its system prompt names.
    pub(super) environment: SessionEnvironment,
    /// Its prefix: the sections and tools its requests begin with.
    pub(super) prefix: Resolved,
    /// What the model has been told applies to it.
    pub(super) told: Told,
    /// What changed since it was last told, to be told now.
    pub(super) changes: Option<String>,
    /// What to record of it.
    pub(super) record: Option<Event>,
}

/// The prefix a session starting sends: the one recorded when it first
/// started (`recorded`), unchanged, so a provider's prompt cache still holds
/// for it; and what applies to it now (`now`, under `environment`) told from
/// what it was last told (`last_told`), as a session that moved to its
/// worktree is told. A session with no recorded prefix, or one this build
/// cannot read, fixes `now` as its prefix.
pub(super) fn settle(
    recorded: Option<&PromptPrefix>,
    last_told: Option<Told>,
    environment: SessionEnvironment,
    now: Resolved,
) -> Settled {
    let Some((fixed_environment, prefix)) = recorded.and_then(Resolved::restored) else {
        let record = Event::PromptPrefixFixed(now.fixed(&environment));
        return Settled {
            told: now.told(),
            environment,
            prefix: now,
            changes: None,
            record: Some(record),
        };
    };
    let told = last_told.unwrap_or_else(|| prefix.told());
    let Some(changes) = prefix_changes(&told, &now) else {
        return Settled {
            environment: fixed_environment,
            prefix,
            told,
            changes: None,
            record: None,
        };
    };
    let told = now.told();
    let record = Event::PromptChangesTold {
        sections: told.sections.clone(),
        tools: told.tools.clone(),
    };
    Settled {
        environment: fixed_environment,
        prefix,
        told,
        changes: Some(changes),
        record: Some(record),
    }
}

/// What `now` changes from what the model was last told (`sent`), told in
/// words: the sections that now apply in full, the ones that no longer
/// apply by their first line, and the tools no longer to be used. `None`
/// when nothing changed.
pub(super) fn prefix_changes(sent: &Told, now: &Resolved) -> Option<String> {
    let added: Vec<&String> = (now.sections.iter())
        .filter(|section| !sent.sections.contains(section))
        .collect();
    let gone: Vec<&str> = (sent.sections.iter())
        .filter(|section| !now.sections.contains(section))
        .map(|section| section.lines().next().unwrap_or_default())
        .collect();
    let withdrawn: Vec<String> = (sent.tools.iter())
        .filter(|tool| {
            !now.tools
                .iter()
                .any(|now| now.name.as_str() == tool.as_str())
        })
        .map(|tool| format!("`{tool}`"))
        .collect();
    if added.is_empty() && gone.is_empty() && withdrawn.is_empty() {
        return None;
    }
    let mut text = String::from(
        "What applies to this session changed with its environment; the system prompt still \
         shows what applied at its start.",
    );
    for section in added {
        text.push_str("\n\n");
        text.push_str(section);
    }
    if !gone.is_empty() {
        text.push_str("\n\nNo longer applies here: ");
        text.push_str(&gone.join("; "));
    }
    if !withdrawn.is_empty() {
        text.push_str("\n\nNo longer to be used here: ");
        text.push_str(&withdrawn.join(", "));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(date: &str, cwd: &str) -> SessionEnvironment {
        SessionEnvironment {
            cwd: cwd.into(),
            os: "linux".into(),
            git_repo: true,
            date: date.into(),
            git_branch: Some("main".into()),
        }
    }

    fn resolved(sections: &[&str]) -> Resolved {
        let config = RigAgentConfig::default();
        Resolved::from_config(
            sections.iter().map(|section| section.to_string()).collect(),
            &config,
        )
    }

    fn fixed(date: &str, cwd: &str, sections: &[&str]) -> PromptPrefix {
        match settle(None, None, environment(date, cwd), resolved(sections)).record {
            Some(Event::PromptPrefixFixed(prefix)) => prefix,
            other => panic!("{other:?}"),
        }
    }

    /// A session with no prefix recorded fixes the one it resolved now.
    #[test]
    fn a_first_start_fixes_the_prefix_it_resolved() {
        let settled = settle(
            None,
            None,
            environment("2026-11-25", "/repo"),
            resolved(&["A"]),
        );
        assert_eq!(settled.prefix.sections, ["A"]);
        assert_eq!(settled.environment.date, "2026-11-25");
        assert!(settled.changes.is_none());
        let Some(Event::PromptPrefixFixed(prefix)) = settled.record else {
            panic!("{:?}", settled.record);
        };
        assert_eq!(prefix.sections, ["A"]);
        assert_eq!(prefix.cwd, "/repo");
    }

    /// A session started again sends the prefix it fixed, as it was, though
    /// the day, the directory and the sections differ now, and is told what
    /// changed, as a session that moved to its worktree is.
    #[test]
    fn a_session_started_again_sends_its_prefix_and_is_told_what_changed() {
        let recorded = fixed("2026-11-25", "/repo", &["A", "B\nsecond line"]);
        let settled = settle(
            Some(&recorded),
            None,
            environment("2026-11-27", "/repo/.horizon/worktrees/x"),
            resolved(&["A", "C"]),
        );
        assert_eq!(settled.prefix.sections, ["A", "B\nsecond line"]);
        assert_eq!(settled.environment.date, "2026-11-25");
        assert_eq!(settled.environment.cwd, std::path::Path::new("/repo"));
        let full = Resolved::from_config(Vec::new(), &RigAgentConfig::default());
        assert_eq!(
            settled.prefix.tools, full.tools,
            "the tools are sent as fixed"
        );
        let changes = settled.changes.expect("what changed is told");
        assert!(changes.contains("\n\nC"), "{changes}");
        assert!(changes.contains("No longer applies here: B"), "{changes}");
        assert_eq!(settled.told.sections, ["A", "C"]);
        assert!(matches!(
            settled.record,
            Some(Event::PromptChangesTold { ref sections, .. }) if sections == &["A", "C"]
        ));
    }

    /// What the model was told once is not told again at the next start.
    #[test]
    fn what_was_told_is_not_told_again() {
        let recorded = fixed("2026-11-25", "/repo", &["A", "B"]);
        let told = resolved(&["A", "C"]).told();
        let settled = settle(
            Some(&recorded),
            Some(told.clone()),
            environment("2026-11-27", "/repo"),
            resolved(&["A", "C"]),
        );
        assert_eq!(settled.prefix.sections, ["A", "B"]);
        assert!(settled.changes.is_none());
        assert!(settled.record.is_none());
        assert_eq!(settled.told, told);
    }

    /// The record goes through the event log's own encoding.
    #[test]
    fn a_fixed_prefix_reads_back_from_its_record() {
        let event = Event::PromptPrefixFixed(fixed("2026-11-25", "/repo", &["A"]));
        let read: Event = serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(read, event);
        let Event::PromptPrefixFixed(prefix) = read else {
            unreachable!()
        };
        assert!(Resolved::restored(&prefix).is_some());
    }
}
