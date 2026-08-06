//! Integration test: the board keeper role and skill, assembled from
//! `horizon-board`'s `pub const` data by `horizon-agentd`'s composition root,
//! resolve correctly and carry the intended permission set.
//!
//! These tests exercise the full `docs/board-keeper-design.md` §1 wiring:
//! `horizon-agentd` (which depends on both `horizon-agent` and `horizon-board`)
//! assembles a `RoleDefinition` from board's constants, registers it via
//! `roles::register_external`, and registers the skill source via
//! `skills::register_external_skill_sources` — then `roles::resolve` finds it.
//!
//! The skill-in-prompt verification (that `SkillRegistry::prompt_section_for_ids`
//! lists the keeper skill) is covered in `horizon-agent`'s own `skills` tests,
//! since `prompt_section_for_ids` is `pub(crate)` and not reachable from here.
//!
//! Each test runs in its own process (nextest), so the one-shot `OnceLock`
//! behind `register_external`/`register_external_skill_sources` is fresh per
//! test.

use horizon_agent::roles::{register_external, resolve, RoleDefinition, RoleId};
use horizon_agent::skills::register_external_skill_sources;

/// Assembles the keeper `RoleDefinition` from `horizon-board`'s `pub const`
/// data, exactly as `horizon-agentd`'s `main` does at startup.
fn keeper_role() -> RoleDefinition {
    RoleDefinition {
        id: horizon_board::keeper::ROLE_ID,
        title: horizon_board::keeper::ROLE_TITLE,
        prompt_section: horizon_board::keeper::ROLE_PROMPT_SECTION,
        allowed_tool_ids: horizon_board::keeper::ROLE_ALLOWED_TOOL_IDS,
        model: horizon_board::keeper::ROLE_MODEL,
        iteration_cap: horizon_board::keeper::ROLE_ITERATION_CAP,
        include_repository_instructions:
            horizon_board::keeper::ROLE_INCLUDE_REPOSITORY_INSTRUCTIONS,
        skill_ids: horizon_board::keeper::ROLE_SKILL_IDS,
        summarize_on_cap: horizon_board::keeper::ROLE_SUMMARIZE_ON_CAP,
    }
}

#[test]
fn keeper_role_resolves_after_registration_from_board() {
    register_external(vec![keeper_role()]);
    let role = resolve(&RoleId("keeper".to_string()))
        .expect("keeper role must resolve after registration from horizon-board");
    assert_eq!(role.id, "keeper");
    assert_eq!(role.title, "Board Keeper");
}

#[test]
fn keeper_role_may_comment_but_not_mutate_board_state() {
    register_external(vec![keeper_role()]);
    let role = resolve(&RoleId("keeper".to_string())).expect("keeper role must resolve");
    let allowed = role
        .allowed_tool_ids
        .expect("keeper role must restrict its tools");

    // The keeper may read the board and write comments.
    assert!(allowed.contains(&"board.read"), "keeper may read the board");
    assert!(
        allowed.contains(&"board.comment"),
        "keeper may write board comments"
    );

    // The keeper may NOT change item status, rank, assignee, or add items.
    // These tool ids don't exist in the catalog today, but the allowlist is
    // the structural gate: if they were ever added, the keeper would not
    // have them because they're absent here.
    for forbidden in [
        "board.set_status",
        "board.assign",
        "board.move_item",
        "board.add_item",
        "board.claim",
    ] {
        assert!(
            !allowed.contains(&forbidden),
            "keeper must not have board state-mutation tool `{forbidden}`"
        );
    }

    // The keeper also has no bash, no file writes — only reads and comments.
    for forbidden in [
        "bash",
        "fs.write",
        "fs.edit",
        "config.write",
        "knowledge.write",
    ] {
        assert!(
            !allowed.contains(&forbidden),
            "keeper must not have `{forbidden}`"
        );
    }
}

#[test]
fn keeper_skill_source_registers_and_role_lists_it() {
    // Register the keeper skill source from horizon-board, exactly as
    // agentd's main does at startup. The skill-in-prompt-section verification
    // is covered in horizon-agent's own skills tests (the method is
    // pub(crate) there); here we verify the role's skill_ids include the
    // keeper skill id, which is what drives the prompt section.
    register_external_skill_sources(vec![horizon_board::keeper::SKILL_SOURCE]);

    register_external(vec![keeper_role()]);
    let role = resolve(&RoleId("keeper".to_string())).expect("keeper role must resolve");
    assert!(
        role.skill_ids.contains(&"board-keeper"),
        "keeper role's skill_ids must include `board-keeper`"
    );
}
