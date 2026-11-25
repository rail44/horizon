//! Board policy is supplied by skills on ordinary agent sessions.

use horizon_agent::packages::PackagesBuilder;
use horizon_agent::roles::{RoleDefinition, RoleWrites};

pub(super) const TASK: &str = "board-task";
pub(super) const REVIEWER: &str = "board-reviewer";

/// Adds the board's roles and skills.
pub(crate) fn add(packages: PackagesBuilder) -> PackagesBuilder {
    let definitions = [
        (TASK, "Board Task", "board-task"),
        (REVIEWER, "Board Reviewer", "board-reviewer"),
    ]
    .into_iter()
    .map(|(id, title, skill)| RoleDefinition {
        id,
        title,
        prompt_section: match skill {
            "board-task" => "Read the board-task skill before working on your associated task. Keep the same session through consultation and implementation.",
            _ => "Read the board-reviewer skill before reviewing the requested task changes.",
        },
        allowed_tool_ids: None,
        model: None,
        unbudgeted: false,
        // Writable once `board_session action=implement` (or a review
        // request) gives the session its own worktree; board #100.
        writes: RoleWrites::InOwnWorktree,
        include_repository_instructions: true,
        skill_ids: match skill {
            "board-task" => &["board-task", "board-integration"],
            _ => &["board-reviewer"],
        },
        // What the judge leaves to a human is refused instead of parked;
        // board #100.
        unattended: true,
    })
    .collect();
    packages.roles(definitions).skill_sources(vec![
        horizon_board::agents::TASK_SKILL,
        horizon_board::agents::REVIEWER_SKILL,
    ])
}
