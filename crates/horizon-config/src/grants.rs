//! `[grants]`: per-project filesystem trees a session may write to from
//! the start (`docs/containment-denial-narrow-grants-design.md`'s
//! 2026-07-26 project-scoped-tree-grants decision).
//!
//! ```toml
//! [[grants.project]]
//! root  = "/home/satoshi/src/github.com/rail44/horizon"
//! trees = ["~/.cargo"]
//! ```
//!
//! Three properties of this section are deliberate, not incidental:
//!
//! - **It lives in the user's own config file, never in the repository.**
//!   A tracked project file would let a cloned repository widen its own
//!   authority the moment an agent session opened it -- the classic
//!   confused deputy. This is direnv's allow model (the human, outside the
//!   repository, says which project may reach where), not VS Code's
//!   tracked-settings model.
//! - **It is keyed by project, not by machine or by spawn.** Needing
//!   `~/.cargo` is a property of developing *this* project.
//! - **Nothing here is command- or language-specific.** `trees` are plain
//!   paths; no rule in Horizon maps a tool name to the directories it
//!   wants.
//!
//! Validation is warn-and-ignore, like every other section: a refused
//! entry names itself on stderr and drops out, and a file full of bad
//! grants still starts Horizon with no grants rather than failing.
//!
//! This module keeps `horizon_sandbox` out of its own public surface --
//! [`ProjectGrant`] carries plain `PathBuf`s. The one thing it borrows
//! from that crate is the over-broad-tree *rule*
//! ([`horizon_sandbox::is_overbroad_tree`]), so the path a config file is
//! allowed to name and the path the sandbox will actually accept can never
//! drift apart.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `[grants]`: the whole section. Only `project` exists today; the section
/// is a table (rather than `project` being a top-level key) so a future
/// grant flavor can join it without reshaping anything.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawGrantsConfig {
    /// `[[grants.project]]`, in file order.
    pub project: Vec<RawProjectGrant>,
}

/// One `[[grants.project]]` entry, exactly as written in the file --
/// unexpanded and unvalidated. [`resolve`] turns a list of these into
/// [`ProjectGrant`]s plus the warnings for whatever it refused.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawProjectGrant {
    /// The project's main-repository toplevel. A session working in a
    /// derived worktree of this repository resolves back to this root --
    /// see `horizon-agentd`'s `worktree::project_root`.
    pub root: String,
    /// Directories granted read-write, as whole trees, to every session of
    /// this project.
    pub trees: Vec<String>,
}

/// A validated `[[grants.project]]` entry: absolute paths, `~` already
/// expanded, every tree checked against the same over-broad rule the
/// sandbox enforces at spawn time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectGrant {
    pub root: PathBuf,
    pub trees: Vec<PathBuf>,
}

/// Expands and validates every `[[grants.project]]` entry, returning the
/// usable ones plus one warning string per refusal.
///
/// Pure in `home` so it can be tested without touching the process
/// environment (and so validating a config file that names another
/// account's paths stays predictable). `None` means no `$HOME` is
/// available: a `~` path cannot be expanded and is refused, but everything
/// absolute still validates.
pub fn resolve(
    entries: &[RawProjectGrant],
    home: Option<&Path>,
) -> (Vec<ProjectGrant>, Vec<String>) {
    let mut resolved = Vec::new();
    let mut warnings = Vec::new();

    for entry in entries {
        let Some(root) = expand(&entry.root, home) else {
            warnings.push(format!(
                "[[grants.project]]: root {:?} is not an absolute path (and no $HOME is set to \
                 expand a leading \"~/\" against), ignoring this entry",
                entry.root
            ));
            continue;
        };
        let mut trees = Vec::new();
        for tree in &entry.trees {
            let Some(tree_path) = expand(tree, home) else {
                warnings.push(format!(
                    "[[grants.project]] root {:?}: tree {tree:?} is not an absolute path (and no \
                     $HOME is set to expand a leading \"~/\" against), ignoring it",
                    entry.root
                ));
                continue;
            };
            if horizon_sandbox::is_overbroad_tree(&tree_path, home) {
                warnings.push(format!(
                    "[[grants.project]] root {:?}: tree {tree:?} resolves to {}, which is the \
                     filesystem root, your home directory, or a system directory -- refusing to \
                     grant it, ignoring it",
                    entry.root,
                    tree_path.display()
                ));
                continue;
            }
            if !trees.contains(&tree_path) {
                trees.push(tree_path);
            }
        }
        resolved.push(ProjectGrant { root, trees });
    }

    (resolved, warnings)
}

/// The trees granted to a session whose project root is `project_root`.
/// Entries are matched by exact root path (both sides already canonical in
/// production: the config's own expansion here, and the git-resolved
/// repository toplevel on the session side). Several entries naming the
/// same root contribute all of their trees.
pub fn trees_for_project(entries: &[ProjectGrant], project_root: &Path) -> Vec<PathBuf> {
    let mut trees = Vec::new();
    for entry in entries {
        if entry.root != project_root {
            continue;
        }
        for tree in &entry.trees {
            if !trees.contains(tree) {
                trees.push(tree.clone());
            }
        }
    }
    trees
}

/// Expands a leading `~`/`~/` against `home` and requires the result to be
/// absolute -- the same rule the persistence-path overrides
/// (`HORIZON_AGENT_EVENT_LOG`/`HORIZON_AGENT_STATE_DB`) already apply. A
/// `~user` form is deliberately *not* supported: it would need passwd
/// lookups to mean anything, and this file only ever describes the account
/// Horizon runs as.
fn expand(value: &str, home: Option<&Path>) -> Option<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let expanded = if trimmed == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        home?.join(rest)
    } else {
        PathBuf::from(trimmed)
    };
    expanded.is_absolute().then_some(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: &str, trees: &[&str]) -> RawProjectGrant {
        RawProjectGrant {
            root: root.to_string(),
            trees: trees.iter().map(|tree| tree.to_string()).collect(),
        }
    }

    #[test]
    fn a_tilde_tree_expands_against_home() {
        let home = PathBuf::from("/home/someone");
        let (resolved, warnings) = resolve(&[entry("/src/project", &["~/.cargo"])], Some(&home));

        assert!(warnings.is_empty(), "warnings = {warnings:?}");
        assert_eq!(
            resolved,
            vec![ProjectGrant {
                root: PathBuf::from("/src/project"),
                trees: vec![PathBuf::from("/home/someone/.cargo")],
            }]
        );
    }

    #[test]
    fn a_tilde_root_expands_against_home_too() {
        let home = PathBuf::from("/home/someone");
        let (resolved, _) = resolve(
            &[entry("~/src/project", &["~/.cache/project"])],
            Some(&home),
        );

        assert_eq!(resolved[0].root, PathBuf::from("/home/someone/src/project"));
        assert_eq!(
            resolved[0].trees,
            vec![PathBuf::from("/home/someone/.cache/project")]
        );
    }

    #[test]
    fn home_itself_is_refused_with_a_named_warning() {
        let home = PathBuf::from("/home/someone");
        let (resolved, warnings) = resolve(&[entry("/src/project", &["~"])], Some(&home));

        assert!(resolved[0].trees.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("/home/someone"));
        assert!(warnings[0].contains("refusing to grant it"));
    }

    #[test]
    fn the_filesystem_root_and_system_directories_are_refused() {
        let home = PathBuf::from("/home/someone");
        let (resolved, warnings) = resolve(
            &[entry("/src/project", &["/", "/usr", "/etc", "~/.cargo"])],
            Some(&home),
        );

        assert_eq!(
            resolved[0].trees,
            vec![PathBuf::from("/home/someone/.cargo")],
            "only the sound tree survives"
        );
        assert_eq!(warnings.len(), 3, "warnings = {warnings:?}");
        assert!(warnings.iter().all(|w| w.contains("refusing to grant it")));
    }

    #[test]
    fn a_relative_tree_is_refused() {
        let home = PathBuf::from("/home/someone");
        let (resolved, warnings) =
            resolve(&[entry("/src/project", &["relative/dir"])], Some(&home));

        assert!(resolved[0].trees.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("not an absolute path"));
    }

    #[test]
    fn a_relative_root_drops_the_whole_entry() {
        let home = PathBuf::from("/home/someone");
        let (resolved, warnings) = resolve(&[entry("project", &["~/.cargo"])], Some(&home));

        assert!(resolved.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
        assert!(warnings[0].contains("ignoring this entry"));
    }

    #[test]
    fn a_tilde_path_without_home_is_refused_rather_than_guessed() {
        let (resolved, warnings) = resolve(&[entry("/src/project", &["~/.cargo"])], None);

        assert!(resolved[0].trees.is_empty());
        assert_eq!(warnings.len(), 1, "warnings = {warnings:?}");
    }

    #[test]
    fn duplicate_trees_collapse() {
        let home = PathBuf::from("/home/someone");
        let (resolved, _) = resolve(
            &[entry("/src/project", &["~/.cargo", "/home/someone/.cargo"])],
            Some(&home),
        );

        assert_eq!(resolved[0].trees.len(), 1);
    }

    #[test]
    fn trees_are_looked_up_by_exact_project_root() {
        let home = PathBuf::from("/home/someone");
        let (resolved, _) = resolve(
            &[
                entry("/src/project", &["~/.cargo"]),
                entry("/src/other", &["~/.other-cache"]),
                entry("/src/project", &["~/.extra"]),
            ],
            Some(&home),
        );

        assert_eq!(
            trees_for_project(&resolved, Path::new("/src/project")),
            vec![
                PathBuf::from("/home/someone/.cargo"),
                PathBuf::from("/home/someone/.extra"),
            ],
            "every entry naming this root contributes"
        );
        assert_eq!(
            trees_for_project(&resolved, Path::new("/src/other")),
            vec![PathBuf::from("/home/someone/.other-cache")]
        );
        assert!(trees_for_project(&resolved, Path::new("/src/unlisted")).is_empty());
    }

    #[test]
    fn a_subdirectory_of_a_listed_root_is_not_a_match() {
        let home = PathBuf::from("/home/someone");
        let (resolved, _) = resolve(&[entry("/src/project", &["~/.cargo"])], Some(&home));

        assert!(
            trees_for_project(&resolved, Path::new("/src/project/crates/inner")).is_empty(),
            "a grant is keyed by the project root itself, not by containment"
        );
    }
}
