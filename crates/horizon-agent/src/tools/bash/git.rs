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

#[derive(Clone, Debug, Eq, PartialEq)]
enum ShellToken {
    Word(String),
    Boundary,
}

/// Whether a bash tool input contains a directly-recognizable Git invocation
/// that may write repository metadata.
pub(crate) fn requires_metadata_write(input: &Value) -> bool {
    input
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(command_requires_metadata_write)
}

pub(crate) fn approved_metadata_roots(output: &Value) -> Option<Vec<PathBuf>> {
    if output
        .get("git_operation_approved")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let roots = output
        .get("approved_git_metadata_roots")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(PathBuf::from))
        .collect::<Option<Vec<_>>>()?;
    (!roots.is_empty()).then_some(roots)
}

fn command_requires_metadata_write(command: &str) -> bool {
    let mut segment = Vec::new();
    for token in tokenize(command) {
        match token {
            ShellToken::Word(word) => segment.push(word),
            ShellToken::Boundary => {
                if segment_requires_metadata_write(&segment) {
                    return true;
                }
                segment.clear();
            }
        }
    }
    segment_requires_metadata_write(&segment)
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
            subcommand => return !READ_ONLY_SUBCOMMANDS.contains(&subcommand),
        }
    }
    false
}

fn git_executable_index(words: &[String]) -> Option<usize> {
    let mut index = 0;
    while words.get(index).is_some_and(|word| is_assignment(word)) {
        index += 1;
    }
    loop {
        match words.get(index).map(String::as_str) {
            Some("command") => {
                index += 1;
                while let Some(option) = words.get(index).map(String::as_str) {
                    match option {
                        "-p" => index += 1,
                        "--" => {
                            index += 1;
                            break;
                        }
                        "-v" | "-V" => return None,
                        value if value.starts_with('-') => return None,
                        _ => break,
                    }
                }
            }
            Some("env") => {
                index += 1;
                while let Some(word) = words.get(index).map(String::as_str) {
                    match word {
                        value if is_assignment(value) => index += 1,
                        "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string" => {
                            index += 2;
                        }
                        "--" => {
                            index += 1;
                            break;
                        }
                        value
                            if value.starts_with("--unset=")
                                || value.starts_with("--chdir=")
                                || value.starts_with("--split-string=")
                                || value.starts_with("--argv0=") =>
                        {
                            index += 1;
                        }
                        value if value.starts_with('-') => index += 1,
                        _ => break,
                    }
                }
            }
            _ => break,
        }
    }
    let executable = words.get(index)?;
    (Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("git"))
    .then_some(index)
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// Small shell lexer used only as a proactive UX classifier. It preserves
/// quoted words and command boundaries without trying to execute expansions.
/// Unsupported shell constructs can only cause the generic sandbox-denial
/// fallback; they never widen access.
fn tokenize(command: &str) -> Vec<ShellToken> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut chars = command.chars().peekable();
    let mut quote = None;

    let push_word = |tokens: &mut Vec<ShellToken>, word: &mut String| {
        if !word.is_empty() {
            tokens.push(ShellToken::Word(std::mem::take(word)));
        }
    };

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    word.push(ch);
                }
            }
            Some('"') => match ch {
                '"' => quote = None,
                '\\' => {
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                }
                _ => word.push(ch),
            },
            Some(_) => unreachable!(),
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '\\' => {
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                }
                ' ' | '\t' | '\r' => push_word(&mut tokens, &mut word),
                '\n' | ';' | '|' | '&' | '(' | ')' => {
                    push_word(&mut tokens, &mut word);
                    tokens.push(ShellToken::Boundary);
                    if matches!(ch, '|' | '&') && chars.peek() == Some(&ch) {
                        chars.next();
                    }
                }
                '#' if word.is_empty() => {
                    for next in chars.by_ref() {
                        if next == '\n' {
                            tokens.push(ShellToken::Boundary);
                            break;
                        }
                    }
                }
                _ => word.push(ch),
            },
        }
    }
    push_word(&mut tokens, &mut word);
    tokens
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
    Ok(current
        .into_iter()
        .map(|path| FilesystemGrant {
            path,
            access: FilesystemGrantAccess::ReadWrite,
            scope: FilesystemGrantScope::DirectoryTree,
        })
        .collect())
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
        ] {
            assert!(!command_requires_metadata_write(command), "{command}");
        }
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
