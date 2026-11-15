//! Pre-execution Git metadata grants for isolated worktrees.
//!
//! A linked worktree keeps its index/HEAD under the main repository's
//! `.git/worktrees/<name>` directory and its objects/refs under the shared
//! common `.git` directory. Waiting for Git to discover those paths through
//! containment denials is both noisy and unsafe for commands with remote side
//! effects. This module recognizes ordinary direct Git invocations and
//! resolves the two metadata roots before the command runs. The approval path
//! still owns the decision; this module only supplies a validated proposal.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use horizon_sandbox::{FilesystemGrant, FilesystemGrantAccess, FilesystemGrantScope};

const MAX_GIT_POINTER_BYTES: u64 = 16 * 1024;

/// Whether a bash tool input contains a directly-recognizable Git invocation
/// that may write repository metadata.
pub(crate) fn approved_metadata_roots(output: &Value) -> Option<Vec<PathBuf>> {
    let evidence: crate::tools::output::Evidence = serde_json::from_value(output.clone()).ok()?;
    if evidence.git_operation_approved != Some(true) {
        return None;
    }
    let roots: Vec<_> = evidence
        .approved_git_metadata_roots?
        .into_iter()
        .map(PathBuf::from)
        .collect();
    (!roots.is_empty()).then_some(roots)
}

/// Resolves the metadata directories a Git-writing command needs.
///
/// For a linked worktree, both the worktree-specific gitdir and shared common
/// gitdir are returned. The `.git` pointer, backlink, `commondir`, and expected
/// `common/worktrees/*` layout are all checked before any path can become an
/// approval proposal.
pub(crate) fn metadata_writable_roots(workspace_root: &Path) -> Result<Vec<PathBuf>, String> {
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|error| format!("could not canonicalize workspace root: {error}"))?;
    let dot_git = workspace_root.join(".git");
    let metadata = fs::symlink_metadata(&dot_git)
        .map_err(|error| format!("could not inspect {}: {error}", dot_git.display()))?;

    if metadata.is_dir() {
        let git_dir = dot_git
            .canonicalize()
            .map_err(|error| format!("could not canonicalize {}: {error}", dot_git.display()))?;
        validate_common_git_dir(&git_dir)?;
        return Ok(vec![git_dir]);
    }
    if !metadata.is_file() {
        return Err(format!(
            "{} is not a Git directory or pointer file",
            dot_git.display()
        ));
    }

    let pointer = read_small_text(&dot_git)?;
    let git_dir_raw = pointer
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:").map(str::trim))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "{} does not contain a valid gitdir pointer",
                dot_git.display()
            )
        })?;
    let git_dir = resolve_relative(&workspace_root, git_dir_raw)
        .canonicalize()
        .map_err(|error| {
            format!(
                "could not resolve gitdir from {}: {error}",
                dot_git.display()
            )
        })?;
    if !git_dir.is_dir() {
        return Err(format!(
            "resolved gitdir {} is not a directory",
            git_dir.display()
        ));
    }

    let backlink_path = git_dir.join("gitdir");
    let backlink_raw = read_small_text(&backlink_path)?;
    let backlink = resolve_relative(&git_dir, backlink_raw.trim())
        .canonicalize()
        .map_err(|error| format!("could not resolve {}: {error}", backlink_path.display()))?;
    let canonical_dot_git = dot_git
        .canonicalize()
        .map_err(|error| format!("could not canonicalize {}: {error}", dot_git.display()))?;
    if backlink != canonical_dot_git {
        return Err(format!(
            "gitdir backlink {} does not point to this workspace",
            backlink_path.display()
        ));
    }

    let commondir_path = git_dir.join("commondir");
    let common_raw = read_small_text(&commondir_path)?;
    let common_dir = resolve_relative(&git_dir, common_raw.trim())
        .canonicalize()
        .map_err(|error| format!("could not resolve {}: {error}", commondir_path.display()))?;
    validate_common_git_dir(&common_dir)?;
    let worktrees_root = common_dir.join("worktrees");
    if !git_dir.starts_with(&worktrees_root) || git_dir == worktrees_root {
        return Err(format!(
            "linked-worktree gitdir {} is outside {}",
            git_dir.display(),
            worktrees_root.display()
        ));
    }

    Ok(vec![git_dir, common_dir])
}

/// Subpaths within a resolved gitdir that a GitOperation extended grant
/// excludes from writing, even though the parent tree is ReadWrite. These are
/// the host-escalation vectors: `hooks/` executes arbitrary code on the next
/// host-side git invocation, and `config` can set `core.fsmonitor`,
///`core.pager`, credential helpers, etc. — all of which run outside the
/// sandbox. Board #38; see
/// `docs/agent-approval-design.md` and
/// `docs/research/shell-approval-evidence-2026-08-17.md`.
const GIT_METADATA_EXCLUDED_SUBPATHS: &[&str] = &["hooks", "config"];

pub(crate) fn validated_metadata_grants(
    workspace_root: &Path,
    expected_roots: &[PathBuf],
) -> Result<Vec<FilesystemGrant>, String> {
    if expected_roots.is_empty() {
        return Err("Git metadata approval did not name any writable roots".to_string());
    }
    let current = metadata_writable_roots(workspace_root)?;
    if current != expected_roots {
        return Err(
            "Git metadata roots changed after approval; refusing the stale grant".to_string(),
        );
    }
    Ok(metadata_grants(&current))
}

/// The grant a GitOperation approval applies to each metadata root: the
/// whole tree read-write except [`GIT_METADATA_EXCLUDED_SUBPATHS`]. Both the
/// sandbox spawn and the approval judge's input are built from this, so the
/// judge is asked about exactly the grant the sandbox enforces.
pub(crate) fn metadata_grants(roots: &[PathBuf]) -> Vec<FilesystemGrant> {
    let excluded: Vec<PathBuf> = GIT_METADATA_EXCLUDED_SUBPATHS
        .iter()
        .map(PathBuf::from)
        .collect();
    roots
        .iter()
        .cloned()
        .map(|path| FilesystemGrant {
            path,
            access: FilesystemGrantAccess::ReadWrite,
            scope: FilesystemGrantScope::DirectoryTree,
            excluded_subpaths: excluded.clone(),
        })
        .collect()
}

fn validate_common_git_dir(path: &Path) -> Result<(), String> {
    if path.join("HEAD").is_file() && path.join("objects").is_dir() && path.join("refs").is_dir() {
        Ok(())
    } else {
        Err(format!(
            "{} is not a complete Git common directory",
            path.display()
        ))
    }
}

fn read_small_text(path: &Path) -> Result<String, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_GIT_POINTER_BYTES {
        return Err(format!("{} is not a small regular file", path.display()));
    }
    fs::read_to_string(path).map_err(|error| format!("could not read {}: {error}", path.display()))
}

fn resolve_relative(base: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn validated_metadata_grants_exclude_hooks_and_config() {
        // Board #38: the GitOperation extended
        // grant excludes hooks/ and config from writing — the
        // host-escalation vectors. The exclusion is based on the resolved
        // gitdir, not a literal `.git` path name.
        let fixture = linked_worktree_fixture("exclusion");
        let roots = metadata_writable_roots(&fixture.workspace).unwrap();
        let grants = validated_metadata_grants(&fixture.workspace, &roots).unwrap();
        assert_eq!(grants.len(), 2);
        for grant in &grants {
            assert_eq!(grant.access, FilesystemGrantAccess::ReadWrite);
            assert_eq!(grant.scope, FilesystemGrantScope::DirectoryTree);
            assert_eq!(
                grant.excluded_subpaths,
                vec![PathBuf::from("hooks"), PathBuf::from("config")],
                "each metadata grant must exclude hooks and config"
            );
        }
        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn metadata_grants_map_each_root_to_a_tree_grant_excluding_hooks_and_config() {
        let roots = vec![
            PathBuf::from("/repo/.git/worktrees/w"),
            PathBuf::from("/repo/.git"),
        ];
        let grants = metadata_grants(&roots);
        assert_eq!(
            grants.iter().map(|grant| &grant.path).collect::<Vec<_>>(),
            roots.iter().collect::<Vec<_>>()
        );
        for grant in &grants {
            assert_eq!(grant.access, FilesystemGrantAccess::ReadWrite);
            assert_eq!(grant.scope, FilesystemGrantScope::DirectoryTree);
            assert_eq!(
                grant.excluded_subpaths,
                vec![PathBuf::from("hooks"), PathBuf::from("config")]
            );
        }
        assert!(metadata_grants(&[]).is_empty());
    }

    #[test]
    fn resolves_and_validates_linked_worktree_metadata_roots() {
        let fixture = linked_worktree_fixture("valid");
        assert_eq!(
            metadata_writable_roots(&fixture.workspace).unwrap(),
            vec![
                fixture.worktree_git_dir.canonicalize().unwrap(),
                fixture.common_git_dir.canonicalize().unwrap(),
            ]
        );
        fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn rejects_a_gitdir_pointer_without_the_matching_backlink() {
        let fixture = linked_worktree_fixture("forged");
        let foreign_dot_git = fixture.root.join("somewhere-else/.git");
        fs::create_dir_all(foreign_dot_git.parent().unwrap()).unwrap();
        fs::write(&foreign_dot_git, "gitdir: nowhere\n").unwrap();
        fs::write(
            fixture.worktree_git_dir.join("gitdir"),
            foreign_dot_git.display().to_string(),
        )
        .unwrap();
        assert!(metadata_writable_roots(&fixture.workspace)
            .unwrap_err()
            .contains("does not point to this workspace"));
        fs::remove_dir_all(fixture.root).unwrap();
    }

    struct WorktreeFixture {
        root: PathBuf,
        workspace: PathBuf,
        common_git_dir: PathBuf,
        worktree_git_dir: PathBuf,
    }

    fn linked_worktree_fixture(label: &str) -> WorktreeFixture {
        let root = std::env::temp_dir().join(format!(
            "horizon-agent-git-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let workspace = root.join("worktree");
        let common_git_dir = root.join("main/.git");
        let worktree_git_dir = common_git_dir.join("worktrees/agent");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(common_git_dir.join("objects")).unwrap();
        fs::create_dir_all(common_git_dir.join("refs")).unwrap();
        fs::create_dir_all(&worktree_git_dir).unwrap();
        fs::write(common_git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", worktree_git_dir.display()),
        )
        .unwrap();
        fs::write(
            worktree_git_dir.join("gitdir"),
            workspace.join(".git").display().to_string(),
        )
        .unwrap();
        fs::write(worktree_git_dir.join("commondir"), "../..\n").unwrap();
        WorktreeFixture {
            root,
            workspace,
            common_git_dir,
            worktree_git_dir,
        }
    }
}
