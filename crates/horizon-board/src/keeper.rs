//! The board keeper agent: the first instance of a board "package" — a
//! feature (the board store) bundled with the agent definition that operates
//! it and the skill that guides that agent — as an early, concrete example
//! of the extension/plugin direction `docs/board-keeper-design.md` records.
//!
//! This module exports the keeper role's fields as `pub const` data and the
//! keeper skill's source text, so `horizon-agentd` (the composition root that
//! depends on both `horizon-agent` and `horizon-board`) can assemble a
//! `RoleDefinition` and register the skill at startup — without either crate
//! depending on the other.
//!
//! **Dependency direction** (`docs/board-keeper-design.md` §1): `horizon-board`
//! does not depend on `horizon-agent` (it never has, and that boundary is an
//! owner requirement). `horizon-agent` does not depend on `horizon-board`
//! (owner decision). `horizon-agentd` depends on both and wires them together.
//! Board exports plain `&'static` data here; agentd constructs the
//! `RoleDefinition` from it and registers the skill source. No new shared
//! crate is needed because the fields are all `&'static str` / `&'static [&str]`
//! — agentd copies them by value into the `RoleDefinition` struct it already
//! owns. This is the "small type-location" option the owner offered, except
//! the type location is the agent crate's own `RoleDefinition` (reached
//! through agentd, not through a Cargo edge), not a separate crate — chosen
//! because it avoids a new crate for one role, and because a board→agent
//! dependency would pull DuckDB/rig-core into this lightweight data crate.

/// The keeper role's identifier — matches `RoleDefinition::id` after agentd
/// assembles it. Used as a `RoleId` on the wire and in the persisted event log.
pub const ROLE_ID: &str = "keeper";

/// Human-readable title shown in Horizon's view chooser when creating a
/// session with this role.
pub const ROLE_TITLE: &str = "Board Keeper";

/// Appended to the keeper session's system prompt as its own section.
pub const ROLE_PROMPT_SECTION: &str = "\
You are the board keeper: you read the task board's items and their comments, \
reconstruct missing context from the codebase, conversation logs, and docs, and \
write that context back as comments on the items that need it.\n\
\n\
Your only write capability is `board.comment` — you add comments to items. You \
do not change status, rank, assignee, or add items. You read the board with \
`board.read`, and you read code, docs, and history with `fs.read`/`fs.grep`/\
`fs.glob`/`recall.search`/`recall.read`/`knowledge.read`.\n\
\n\
Read the `board-keeper` skill (via `skill.read`) before writing your first \
comment — it covers the discipline of restoring context honestly.";

/// The tool ids the keeper role may call. Read-only tools plus `board.read` \
/// (read the board) and `board.comment` (write a comment). `bash`, `fs.write`, \
/// `fs.edit`, and all board state-mutation tools are absent — the keeper \
/// cannot change item status/rank/assignee or add items.
pub const ROLE_ALLOWED_TOOL_IDS: Option<&[&str]> = Some(&[
    "fs.read",
    "fs.grep",
    "fs.glob",
    "board.read",
    "board.comment",
    "skill.read",
    "knowledge.read",
    "recall.search",
    "recall.read",
    "memory.update",
]);

/// No model override — the keeper uses the provider's configured model.
pub const ROLE_MODEL: Option<&str> = None;

/// No turn-cap override — the keeper uses the default iteration cap.
pub const ROLE_ITERATION_CAP: Option<u32> = None;

/// The keeper reads the repository's `AGENTS.md`/`CLAUDE.md` because it needs \
/// to understand the project's conventions to reconstruct context accurately.
pub const ROLE_INCLUDE_REPOSITORY_INSTRUCTIONS: bool = true;

/// The keeper's skill — loaded as an embedded skill at agentd startup.
pub const ROLE_SKILL_IDS: &[&str] = &["board-keeper"];

/// No forced wrap-up on cap — the keeper is interactive, not a one-shot \
/// delegated report like the explore role.
pub const ROLE_SUMMARIZE_ON_CAP: bool = false;

/// The keeper is a *standing* role -- a long-lived, context-carrying agent
/// that maintains a memory document across turns
/// (`docs/standing-agent-memory-design.md`). This is what makes the keeper's
/// sessions carry project context across the `board.read`/`board.comment`
/// interactions they serve, rather than starting from zero each spawn.
pub const ROLE_STANDING: bool = true;

/// The keeper skill's `SKILL.md` source, embedded at compile time so \
/// `horizon-agentd` can register it as an embedded skill without a Cargo \
/// dependency on this crate's `skills/` directory layout.
pub const SKILL_SOURCE: &str = include_str!("../skills/board-keeper/SKILL.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_id_is_keeper() {
        assert_eq!(ROLE_ID, "keeper");
    }

    #[test]
    fn role_title_is_nonempty() {
        assert!(!ROLE_TITLE.is_empty());
    }

    #[test]
    fn allowlist_includes_board_read_and_comment() {
        let allowed = ROLE_ALLOWED_TOOL_IDS.expect("keeper must restrict its tools");
        assert!(
            allowed.contains(&"board.read"),
            "keeper must be able to read the board"
        );
        assert!(
            allowed.contains(&"board.comment"),
            "keeper must be able to comment on items"
        );
    }

    #[test]
    fn allowlist_includes_memory_update() {
        let allowed = ROLE_ALLOWED_TOOL_IDS.expect("keeper must restrict its tools");
        assert!(
            allowed.contains(&"memory.update"),
            "keeper is a standing role and must update its memory document"
        );
    }

    #[test]
    fn allowlist_includes_read_tools_for_context_reconstruction() {
        let allowed = ROLE_ALLOWED_TOOL_IDS.expect("keeper must restrict its tools");
        // The keeper reconstructs context from code, docs, and history —
        // it needs the same read tools an ordinary session has.
        assert!(allowed.contains(&"fs.read"));
        assert!(allowed.contains(&"fs.grep"));
        assert!(allowed.contains(&"fs.glob"));
        assert!(allowed.contains(&"recall.search"));
        assert!(allowed.contains(&"recall.read"));
        assert!(allowed.contains(&"knowledge.read"));
        assert!(allowed.contains(&"skill.read"));
    }

    #[test]
    fn allowlist_excludes_bash_and_file_writes() {
        let allowed = ROLE_ALLOWED_TOOL_IDS.expect("keeper must restrict its tools");
        // The keeper's only write is board.comment — no bash, no file edits.
        for forbidden in [
            "bash",
            "fs.write",
            "fs.edit",
            "web_search",
            "web_fetch",
            "config.write",
            "knowledge.write",
            "task",
        ] {
            assert!(
                !allowed.contains(&forbidden),
                "keeper must not have `{forbidden}`"
            );
        }
    }

    #[test]
    fn allowlist_has_no_board_state_mutation_tools() {
        let allowed = ROLE_ALLOWED_TOOL_IDS.expect("keeper must restrict its tools");
        // The keeper may comment but must not change item status, rank,
        // assignee, or add items — those are the owner's and integrator's
        // decisions. These tool ids don't exist in the catalog today, but
        // the allowlist is the structural gate: listing them here would grant
        // them if they were ever added.
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
    }

    #[test]
    fn skill_source_has_correct_frontmatter() {
        // The agent's skill loader (skills::parse_skill_md) expects `---\n`
        // delimiters with `name:` and `description:` keys. A malformed source
        // would crash the daemon at startup (embedded_skills .expect()s it),
        // so this is a build-time invariant worth checking here too.
        assert!(
            SKILL_SOURCE.starts_with("---\n"),
            "SKILL.md must start with frontmatter"
        );
        assert!(
            SKILL_SOURCE.contains("name: board-keeper"),
            "SKILL.md frontmatter must name the skill `board-keeper`"
        );
        assert!(
            SKILL_SOURCE.contains("description:"),
            "SKILL.md frontmatter must have a description"
        );
    }

    #[test]
    fn skill_source_body_covers_keeper_discipline() {
        // The skill body must cover the four keeper disciplines the owner
        // specified: read comments in context, restore context from logs/code,
        // write for a zero-context reader, and don't present speculation as fact.
        assert!(
            SKILL_SOURCE.contains("context"),
            "skill must address reading comments in their item's context"
        );
        assert!(
            SKILL_SOURCE.contains("speculation") || SKILL_SOURCE.contains("guess"),
            "skill must address not presenting speculation as fact"
        );
    }
}
