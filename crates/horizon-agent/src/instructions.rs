//! Repository instruction file loading (`AGENTS.md`/`CLAUDE.md`) — the first
//! consumer of `prompt::system_prompt`'s "extra sections" injection point.
//!
//! `docs/research/agent-prompting.md` Part 1.5 surveys the current
//! generation of coding agents (Codex CLI, Claude Code, GitHub Copilot,
//! Cline, ...) and finds that concatenating repository instruction files
//! found while walking from the working directory up to the repository
//! root is the de facto standard; Horizon having no such path at all was
//! flagged as "a clear outlier". This module closes that gap.
//!
//! Gathered once, when a session starts (same "applied at startup only"
//! philosophy as `config`'s file-based knobs — see `AGENTS.md`'s
//! "Configuration" section) and never refreshed for the session's
//! lifetime, mirroring how `prompt::SessionEnvironment` is gathered once
//! and reused for every turn.

use std::path::{Path, PathBuf};

/// Preferred repository-instruction filename at each directory level.
const AGENTS_MD: &str = "AGENTS.md";
/// Fallback, used only at a directory level where [`AGENTS_MD`] isn't
/// present. This crate's own repo is an example of why both are checked:
/// its root `CLAUDE.md` is a one-line `@AGENTS.md` include meant for tools
/// that only ever look for `CLAUDE.md`, with `AGENTS.md` holding the real
/// content.
const CLAUDE_MD: &str = "CLAUDE.md";

/// Label the composed section is introduced with in the system prompt.
const SECTION_LABEL: &str = "Repository instructions (AGENTS.md):";

/// Builds the `extra_sections` entries (see `prompt::system_prompt`) that
/// surface repository instruction files to the model: zero entries if none
/// were found anywhere in the walk described by [`ancestor_dirs_from_git_root`],
/// one labelled section otherwise. `cwd` is the session's working directory
/// (`prompt::SessionEnvironment::cwd`); `cap_chars` bounds the section's
/// total size (`config::RigAgentConfig::repository_instructions_cap_chars`)
/// so a large or deeply nested set of instruction files can't unboundedly
/// inflate every turn's prompt.
pub fn extra_sections(cwd: &Path, cap_chars: usize) -> Vec<String> {
    match repository_instructions_section(cwd, cap_chars) {
        Some(section) => vec![section],
        None => Vec::new(),
    }
}

fn repository_instructions_section(cwd: &Path, cap_chars: usize) -> Option<String> {
    let files: Vec<(PathBuf, String)> = ancestor_dirs_from_git_root(cwd)
        .into_iter()
        .filter_map(|dir| read_instruction_file(&dir))
        .collect();
    if files.is_empty() {
        return None;
    }

    let mut body = String::new();
    for (path, content) in &files {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str("# ");
        body.push_str(&path.display().to_string());
        body.push('\n');
        body.push_str(content.trim_end());
    }

    let (body, truncated) = cap_to_chars(body, cap_chars);
    let mut section = String::from(SECTION_LABEL);
    section.push_str("\n\n");
    section.push_str(&body);
    if truncated {
        section.push_str(
            "\n\n[repository instructions truncated: exceeded the configured character cap]",
        );
    }
    Some(section)
}

/// Directories to check for repository-scoped, per-directory content, in
/// composition/discovery order (broad to specific: the repository root
/// first, `cwd` last) — so a deeper directory's content reads as refining
/// the root's, matching the reading order a human would use. Inside a git
/// repository this is every ancestor of `cwd`, from the repository root
/// (the nearest ancestor holding a `.git` entry) down to `cwd` itself;
/// outside one it's just `cwd` — walking indefinitely toward the filesystem
/// root with no repository boundary to stop at would pull in unrelated
/// ancestor directories (e.g. a stray `~/AGENTS.md`).
///
/// Shared with `skills`' repository-skill discovery (`.horizon/skills/`) --
/// both want the exact same "walk from cwd up to the git root" discipline,
/// just applied to a different filename/directory convention at each level.
pub(crate) fn ancestor_dirs_from_git_root(cwd: &Path) -> Vec<PathBuf> {
    match git_root(cwd) {
        Some(root) => {
            let mut dirs: Vec<PathBuf> = Vec::new();
            for dir in cwd.ancestors() {
                dirs.push(dir.to_path_buf());
                if dir == root {
                    break;
                }
            }
            dirs.reverse();
            dirs
        }
        None => vec![cwd.to_path_buf()],
    }
}

/// The nearest ancestor of `cwd` (inclusive) holding a `.git` entry — a
/// directory in a normal checkout, a file in a worktree — or `None` if
/// `cwd` isn't inside a git repository at all. A near-duplicate of
/// `prompt::is_git_repository`'s walk (which only needs a yes/no answer);
/// this one needs the actual matching directory to bound
/// [`ancestor_dirs_from_git_root`]'s walk.
fn git_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Reads `dir`'s instruction file, preferring [`AGENTS_MD`] and falling
/// back to [`CLAUDE_MD`] only when [`AGENTS_MD`] isn't present at this
/// directory level. Returns `None` if neither exists, or if the one found
/// can't be read.
fn read_instruction_file(dir: &Path) -> Option<(PathBuf, String)> {
    let agents_md = dir.join(AGENTS_MD);
    if let Some(content) = read_to_string_or_warn(&agents_md) {
        return Some((agents_md, content));
    }
    let claude_md = dir.join(CLAUDE_MD);
    read_to_string_or_warn(&claude_md).map(|content| (claude_md, content))
}

/// A missing file is the overwhelmingly common case (most directory levels
/// have neither file) and isn't logged; any other read failure (permission
/// denied, a directory named `AGENTS.md`, ...) is unusual enough to warn
/// about — mirrors `config::load_file_config_from_path`'s treatment of its
/// own config file ("no file written yet is the common case, not a
/// warning").
fn read_to_string_or_warn(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "failed to read repository instructions file; skipping"
            );
            None
        }
    }
}

/// Truncates `body` to at most `cap_chars` *characters* (not bytes, so a
/// cap can never split a multi-byte UTF-8 sequence), returning whether it
/// truncated anything. `pub(crate)` so `skills` can apply the same
/// truncation discipline to a skill's body (`skill.read`'s size cap).
pub(crate) fn cap_to_chars(body: String, cap_chars: usize) -> (String, bool) {
    if body.chars().count() <= cap_chars {
        (body, false)
    } else {
        (body.chars().take(cap_chars).collect(), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "horizon-agent-instructions-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// No `AGENTS.md`/`CLAUDE.md` anywhere in the walk must produce zero
    /// extra sections — the "no consumer" backward-compatible case.
    #[test]
    fn extra_sections_is_empty_when_no_instruction_files_exist() {
        let root = temp_dir("empty");
        std::fs::create_dir_all(root.join(".git")).unwrap();

        assert_eq!(extra_sections(&root, 24_000), Vec::<String>::new());
    }

    /// A single `AGENTS.md` at the git root (the common case: `cwd` *is*
    /// the root) surfaces as one labelled section containing its content.
    #[test]
    fn extra_sections_surfaces_a_single_agents_md_at_the_repo_root() {
        let root = temp_dir("single");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write(&root, "AGENTS.md", "Run `cargo test` before committing.");

        let sections = extra_sections(&root, 24_000);

        assert_eq!(sections.len(), 1);
        assert!(sections[0].starts_with("Repository instructions (AGENTS.md):"));
        assert!(sections[0].contains("Run `cargo test` before committing."));
    }

    /// Ancestors are composed root-first, cwd-last — a deeper directory's
    /// guidance should read as refining the root's, not the other way
    /// around.
    #[test]
    fn extra_sections_composes_ancestors_root_first() {
        let root = temp_dir("nested-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let nested = root.join("crates").join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        write(&root, "AGENTS.md", "ROOT_MARKER");
        write(&nested, "AGENTS.md", "NESTED_MARKER");

        let sections = extra_sections(&nested, 24_000);

        assert_eq!(sections.len(), 1);
        let root_pos = sections[0]
            .find("ROOT_MARKER")
            .expect("root marker present");
        let nested_pos = sections[0]
            .find("NESTED_MARKER")
            .expect("nested marker present");
        assert!(
            root_pos < nested_pos,
            "expected root-level content before nested-level content"
        );
    }

    /// At a directory level with no `AGENTS.md`, `CLAUDE.md` is read
    /// instead; at a level with both, `AGENTS.md` wins.
    #[test]
    fn extra_sections_prefers_agents_md_and_falls_back_to_claude_md() {
        let root = temp_dir("fallback");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        // Root has only CLAUDE.md.
        write(&root, "CLAUDE.md", "CLAUDE_ONLY_MARKER");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        // Nested has both -- AGENTS.md must win.
        write(&nested, "AGENTS.md", "AGENTS_WINS_MARKER");
        write(&nested, "CLAUDE.md", "SHOULD_NOT_APPEAR_MARKER");

        let sections = extra_sections(&nested, 24_000);

        assert_eq!(sections.len(), 1);
        assert!(sections[0].contains("CLAUDE_ONLY_MARKER"));
        assert!(sections[0].contains("AGENTS_WINS_MARKER"));
        assert!(!sections[0].contains("SHOULD_NOT_APPEAR_MARKER"));
    }

    /// Outside a git repository, only `cwd` itself is checked -- not its
    /// ancestors.
    #[test]
    fn extra_sections_checks_only_cwd_outside_a_git_repository() {
        let root = temp_dir("non-git-root");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        write(&root, "AGENTS.md", "PARENT_MARKER_SHOULD_NOT_APPEAR");
        write(&nested, "AGENTS.md", "CWD_MARKER");

        let sections = extra_sections(&nested, 24_000);

        assert_eq!(sections.len(), 1);
        assert!(sections[0].contains("CWD_MARKER"));
        assert!(!sections[0].contains("PARENT_MARKER_SHOULD_NOT_APPEAR"));
    }

    /// A cap smaller than the composed body truncates and appends a note,
    /// rather than silently dropping content or exceeding the cap.
    #[test]
    fn extra_sections_truncates_and_notes_when_over_the_cap() {
        let root = temp_dir("truncate");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write(&root, "AGENTS.md", &"x".repeat(1000));

        let sections = extra_sections(&root, 50);

        assert_eq!(sections.len(), 1);
        assert!(sections[0].contains("truncated"));
        // The full 1000-character run can't have survived a 50-char cap.
        assert!(!sections[0].contains(&"x".repeat(1000)));
    }

    /// A read failure other than "not found" (a directory named
    /// `AGENTS.md`, here) is skipped rather than propagated as an error.
    #[test]
    fn extra_sections_skips_an_unreadable_instruction_file() {
        let root = temp_dir("unreadable");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        // A directory named AGENTS.md can't be read as a file -- read_to_string
        // fails with something other than NotFound.
        std::fs::create_dir_all(root.join("AGENTS.md")).unwrap();

        assert_eq!(extra_sections(&root, 24_000), Vec::<String>::new());
    }
}
