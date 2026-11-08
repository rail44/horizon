//! The `project-wiki` skill: the procedure for refreshing the wiki, written
//! so it does not depend on which agent runs it. Embedded here so
//! `horizon-agentd` can register it as a skill source; Claude Code reads the
//! same file through `.claude/skills/project-wiki`.

pub const SKILL_SOURCE: &str = include_str!("../skills/project-wiki/SKILL.md");
