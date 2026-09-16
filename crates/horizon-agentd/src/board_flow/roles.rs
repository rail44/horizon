//! Board policy is supplied by skills on ordinary agent sessions.

use horizon_agent::roles::RoleDefinition;

pub(super) const ORGANIZER: &str = "board-organizer";
pub(super) const TASK: &str = "board-task";
pub(super) const REVIEWER: &str = "board-reviewer";

pub(crate) fn register() {
    let definitions = [
        (ORGANIZER, "Board Organizer", "board-organizer", true),
        (TASK, "Board Task", "board-task", false),
        (REVIEWER, "Board Reviewer", "board-reviewer", false),
    ]
    .into_iter()
    .map(|(id, title, skill, standing)| RoleDefinition {
        id,
        title,
        prompt_section: match skill {
            "board-organizer" => "Read the board-organizer skill before organizing tasks. Follow the project's instructions and integration policy.",
            "board-task" => "Read the board-task skill before working on your associated task. Keep the same session through consultation and implementation.",
            _ => "Read the board-reviewer skill before reviewing the requested task changes.",
        },
        allowed_tool_ids: None,
        model: None,
        iteration_cap: None,
        include_repository_instructions: true,
        skill_ids: match skill {
            "board-organizer" => &["board-organizer"],
            "board-task" => &["board-task", "board-integration"],
            _ => &["board-reviewer"],
        },
        summarize_on_cap: false,
        standing,
    })
    .collect();
    horizon_agent::roles::register_external(definitions);
    horizon_agent::skills::register_external_skill_sources(vec![
        horizon_board::agents::ORGANIZER_SKILL,
        horizon_board::agents::TASK_SKILL,
        horizon_board::agents::REVIEWER_SKILL,
    ]);
}
