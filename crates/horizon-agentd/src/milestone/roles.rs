//! The daemon composes board capabilities with agent roles.

use horizon_agent::roles::RoleDefinition;

pub(super) const PLANNER: &str = "milestone-planner";
pub(super) const WORKER: &str = "milestone-worker";

pub(crate) fn definitions() -> Vec<RoleDefinition> {
    vec![
        RoleDefinition {
            id: PLANNER, title: "Milestone Planner",
            prompt_section: "You plan the assigned milestone. Read the repository, the board item, its discussion, previous plan, results, and owner answers. Save a concrete plan using board.report with kind=plan. Use the owner's language. Keep the summary short. Describe observable acceptance criteria and implementation tasks with stable keys, instructions, acceptance criteria, and dependency keys. Order tasks by priority. Preserve completed tasks exactly; add follow-up tasks for changes. Surface only unresolved decisions that materially change the goal or implementation, each with one clear question, short factual context, a recommendation and its consequence. Do not ask the owner to classify decisions or read raw logs. Resolve routine implementation choices yourself. An empty decisions array authorizes the eligible tasks to execute automatically. Do not add a blanket approval decision. Do not implement code or write comments. Use kind=blocked with a concrete reason if you cannot produce a reliable plan. After saving a report, finish the turn.",
            allowed_tool_ids: Some(&["fs.read", "fs.grep", "fs.glob", "board.read", "board.report", "skill.read", "recall.search", "recall.read", "knowledge.read"]),
            model: None, iteration_cap: None, include_repository_instructions: true,
            skill_ids: &[], summarize_on_cap: false, standing: false,
        },
        RoleDefinition {
            id: WORKER, title: "Milestone Implementer",
            prompt_section: "Implement only the currently assigned milestone task in your isolated worktree. Read the current plan and owner answers using board.read. Prior tasks share this worktree: inspect existing changes and preserve them. Follow repository instructions, run the relevant verification and required quality gate, and inspect the final diff. Save a board.report with kind=task, a concise summary, and checks naming the actual commands, outcomes, and any verification limits. Do not claim checks that were not run. If a required check fails, authority is missing, or scope needs an owner decision, report kind=blocked with a concrete reason. Never integrate into main, push, deploy, or send external messages as part of an assignment. Do not change board state through shell commands. After saving the report, stop changing files and finish your turn. A task report is implementation evidence for review, not owner acceptance or a merge.",
            allowed_tool_ids: Some(&["bash", "fs.read", "fs.grep", "fs.glob", "fs.write", "fs.edit", "board.read", "board.report", "skill.read"]),
            model: None, iteration_cap: None, include_repository_instructions: true,
            skill_ids: &[], summarize_on_cap: false, standing: false,
        },
    ]
}
