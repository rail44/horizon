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

use super::shell::{any_command_segment, executable_index};

const MAX_GIT_POINTER_BYTES: u64 = 16 * 1024;

/// Git subcommands that do not intentionally mutate repository metadata.
///
/// Unknown commands deliberately require approval: aliases and newly-added
/// Git commands can perform writes. A false negative in the shell recognizer
/// remains contained by the generic structured-denial path.
const READ_ONLY_SUBCOMMANDS: &[&str] = &[
    "annotate",
    "blame",
    "cat-file",
    "describe",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "for-each-ref",
    "grep",
    "help",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "merge-base",
    "name-rev",
    "rev-list",
    "rev-parse",
    "shortlog",
    "show",
    "show-ref",
    "status",
    "version",
    "whatchanged",
];

/// Whether `subcommand` with `args` only reads repository metadata.
///
/// Beyond [`READ_ONLY_SUBCOMMANDS`], a few subcommands both list and write
/// depending on their arguments; only their listing forms count here, and
/// anything not recognized as one stays a write.
fn is_read_only_invocation(subcommand: &str, args: &[String]) -> bool {
    if READ_ONLY_SUBCOMMANDS.contains(&subcommand) {
        return true;
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match subcommand {
        "branch" => is_listing(&args, BRANCH_LIST_FLAGS, BRANCH_VALUE_FLAGS),
        "tag" => is_listing(&args, TAG_LIST_FLAGS, TAG_VALUE_FLAGS),
        "worktree" => args.first() == Some(&"list"),
        "stash" => matches!(args.first(), Some(&"list" | &"show")),
        "remote" => match args.first() {
            None => true,
            Some(&"get-url") => true,
            Some(&("-v" | "--verbose")) => args.len() == 1,
            _ => false,
        },
        "config" => is_config_read(&args),
        _ => false,
    }
}

/// `git branch` flags that keep it in list mode.
const BRANCH_LIST_FLAGS: &[&str] = &[
    "--show-current",
    "-a",
    "--all",
    "-r",
    "--remotes",
    "-l",
    "--list",
    "-v",
    "-vv",
    "--verbose",
    "-q",
    "--quiet",
    "--no-color",
    "--column",
    "--no-column",
    "--omit-empty",
    "-i",
    "--ignore-case",
];

/// `git branch` list filters whose value is the next word unless written
/// with `=`.
const BRANCH_VALUE_FLAGS: &[&str] = &[
    "--contains",
    "--no-contains",
    "--merged",
    "--no-merged",
    "--points-at",
    "--sort",
    "--format",
];

const TAG_LIST_FLAGS: &[&str] = &[
    "-l",
    "--list",
    "-n",
    "--no-color",
    "--column",
    "--no-column",
    "--omit-empty",
    "-i",
    "--ignore-case",
];

const TAG_VALUE_FLAGS: &[&str] = &[
    "--contains",
    "--no-contains",
    "--merged",
    "--no-merged",
    "--points-at",
    "--sort",
    "--format",
];

/// Whether a `branch`/`tag` argument list stays in list mode: every option
/// is a listing option, and positional words appear only after an explicit
/// `-l`/`--list`, where they are patterns rather than names to create.
fn is_listing(args: &[&str], list_flags: &[&str], value_flags: &[&str]) -> bool {
    let explicit_list = args.iter().any(|arg| matches!(*arg, "-l" | "--list"));
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if value_flags.contains(arg) {
            index += 2;
            continue;
        }
        let is_option = arg.starts_with('-');
        let known = list_flags.contains(arg)
            || arg.starts_with("--color")
            || value_flags
                .iter()
                .any(|flag| arg.strip_prefix(flag).is_some_and(|rest| rest.starts_with('=')))
            // `-n<num>` (tag annotation lines).
            || (arg.starts_with("-n") && arg[2..].chars().all(|c| c.is_ascii_digit()));
        if is_option && !known {
            return false;
        }
        if !is_option && !explicit_list {
            return false;
        }
        index += 1;
    }
    true
}

/// Whether a `git config` argument list only reads: the `get`/`list`
/// subcommands, or the legacy `--get*`/`--list` modes with no write mode.
fn is_config_read(args: &[&str]) -> bool {
    const READ_MODES: &[&str] = &[
        "--get",
        "--get-all",
        "--get-regexp",
        "--get-urlmatch",
        "--get-color",
        "--get-colorbool",
        "-l",
        "--list",
    ];
    const WRITE_MODES: &[&str] = &[
        "--add",
        "--replace-all",
        "--unset",
        "--unset-all",
        "--rename-section",
        "--remove-section",
        "-e",
        "--edit",
    ];
    // Options whose value is the next word, so that word is not the
    // subcommand.
    const VALUE_OPTIONS: &[&str] = &["-f", "--file", "--blob", "--type", "--default"];
    let mut first_word = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if VALUE_OPTIONS.contains(arg) {
            index += 2;
            continue;
        }
        if !arg.starts_with('-') {
            first_word = Some(arg);
            break;
        }
        index += 1;
    }
    if args.iter().any(|arg| WRITE_MODES.contains(arg)) {
        return false;
    }
    if args.iter().any(|arg| READ_MODES.contains(arg)) {
        return true;
    }
    matches!(first_word, Some(&"get" | &"list"))
}

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

pub(crate) fn command_requires_metadata_write(command: &str) -> bool {
    any_command_segment(command, segment_requires_metadata_write)
}

fn segment_requires_metadata_write(words: &[String]) -> bool {
    let Some(git_index) = git_executable_index(words) else {
        return false;
    };
    let mut index = git_index + 1;
    while let Some(arg) = words.get(index).map(String::as_str) {
        match arg {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
            | "--exec-path" => {
                index += 2;
            }
            "--no-pager"
            | "--paginate"
            | "--no-replace-objects"
            | "--bare"
            | "--literal-pathspecs"
            | "--glob-pathspecs"
            | "--noglob-pathspecs"
            | "--icase-pathspecs"
            | "--no-optional-locks" => {
                index += 1;
            }
            "--version" | "--help" => return false,
            value
                if value.starts_with("--git-dir=")
                    || value.starts_with("--work-tree=")
                    || value.starts_with("--namespace=")
                    || value.starts_with("--config-env=")
                    || value.starts_with("--exec-path=") =>
            {
                index += 1;
            }
            value if value.starts_with('-') => return true,
            subcommand => return !is_read_only_invocation(subcommand, &words[index + 1..]),
        }
    }
    false
}

fn git_executable_index(words: &[String]) -> Option<usize> {
    let index = executable_index(words)?;
    let executable = words.get(index)?;
    (Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("git"))
    .then_some(index)
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
    fn recognizes_writing_git_commands_but_not_read_only_or_quoted_text() {
        for command in [
            "git commit -m test",
            "git -C ../repo add src/lib.rs",
            "echo ok && env TRACE=1 /usr/bin/git push origin main",
            "env -u GIT_DIR git commit -m test",
            "command git branch topic",
        ] {
            assert!(command_requires_metadata_write(command), "{command}");
        }
        for command in [
            "git status --short",
            "git --no-pager diff --stat",
            "git -C ../repo log -1",
            "echo 'git commit -m nope'",
            "printf '%s' git commit",
            "command -v git commit",
            // A compound whose every segment is read-only is not a Git
            // metadata operation at all: the detector returns false, so the
            // command runs as ordinary contained bash (no approval, no judge).
            "git rev-parse --show-toplevel && git status --short && git log --oneline -5",
        ] {
            assert!(!command_requires_metadata_write(command), "{command}");
        }
    }

    #[test]
    fn listing_forms_of_dual_use_subcommands_are_read_only() {
        // Forms agents ran in real sessions (2026-10-19..11-03) that were
        // routed to a Git metadata approval although they only read.
        for command in [
            "git branch --show-current",
            "git branch -a",
            "git branch",
            "git branch -vv --sort=-committerdate",
            "git branch --contains HEAD -r",
            "git branch --list 'horizon/*'",
            "git branch --show-current; git worktree list",
            "git worktree list --porcelain",
            "git stash list",
            "git stash show -p",
            "git tag",
            "git tag -l 'v*'",
            "git remote -v",
            "git remote get-url origin",
            "git config --get-regexp 'url\\..*\\.instead'",
            "git config --show-origin --get-regexp 'url\\..*instead'",
            "git config --global --get-regexp 'credential|user\\.'",
            "git config --list",
            "git config get user.name",
            "git config --global list",
        ] {
            assert!(!command_requires_metadata_write(command), "{command}");
        }
        for command in [
            "git branch topic",
            "git branch -d topic",
            "git branch -a -D topic",
            "git branch -m old new",
            "git branch --set-upstream-to=origin/main",
            "git tag v1",
            "git tag -a v1 -m msg",
            "git tag -d v1",
            "git worktree add ../x",
            "git worktree prune",
            "git stash",
            "git stash push -m x",
            "git stash pop",
            "git remote add up url",
            "git remote set-url origin url",
            "git remote -v add up url",
            "git config user.name x",
            "git config --unset user.name",
            "git config --get user.name --add user.name x",
            "git config set user.name x",
            "git config --file get user.name x",
        ] {
            assert!(command_requires_metadata_write(command), "{command}");
        }
    }

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
