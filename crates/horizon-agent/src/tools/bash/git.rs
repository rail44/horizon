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
pub(super) enum ShellToken {
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
    let index = executable_index(words)?;
    let executable = words.get(index)?;
    (Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("git"))
    .then_some(index)
}

/// Finds a directly invoked command after the small set of shell prefixes
/// understood by Horizon's proactive command classifiers. This is not a
/// security parser: unsupported shell syntax falls through to containment.
pub(super) fn executable_index(words: &[String]) -> Option<usize> {
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
    words.get(index).map(|_| index)
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
pub(super) fn tokenize(command: &str) -> Vec<ShellToken> {
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

/// The deterministic prefilter's verdict for a Git metadata operation —
/// see [`git_prefilter`].
///
/// A pure function (no I/O) that inspects the command a Git operation
/// approval was derived from and decides whether the enforcing judge may
/// evaluate it, or whether a human must be asked directly. The prefilter
/// runs before the judge in `start_approval_gate`; it never widens access —
/// a `HumanDirect` verdict preserves the ordinary human approval flow, and
/// a `PassToJudge` verdict only lets the judge attempt an auto-approve (every
/// judge failure or escalation still falls back to the human).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitPrefilterVerdict {
    /// Skip the judge and ask the human directly. Carries a static
    /// description of the detected construct for the approval reason text.
    HumanDirect(&'static str),
    /// Let the enforcing judge evaluate the command.
    PassToJudge,
}

/// Git global options the prefilter always routes to the human: each can
/// redirect execution to an arbitrary program or override the repository Git
/// operates on.
const PREFILTER_DANGEROUS_OPTIONS: &[&str] = &[
    "-c",
    "--config-env",
    "--exec-path",
    "--git-dir",
    "--work-tree",
    "--upload-pack",
    "--receive-pack",
];

/// The `=`-form equivalents of [`PREFILTER_DANGEROUS_OPTIONS`].
const PREFILTER_DANGEROUS_EQ_PREFIXES: &[&str] = &[
    "--config-env=",
    "--exec-path=",
    "--git-dir=",
    "--work-tree=",
    "--upload-pack=",
    "--receive-pack=",
];

/// Git subcommands that can execute arbitrary code or modify trust settings
/// (hooks, config, filters, credential helpers). The judge handles routine
/// metadata operations; these never bypass the human.
const PREFILTER_DANGEROUS_SUBCOMMANDS: &[&str] = &[
    "config",
    "hook",
    "filter-branch",
    "filter-repo",
    "credential",
];

/// Determines whether a Git metadata operation may go to the enforcing judge
/// or must be asked of a human directly.
///
/// This is a cheap, deterministic first stage in front of the LLM judge
/// (owner decision 2026-08-03): the handful of shell and Git constructs that
/// can redirect execution or smuggle in arbitrary code always go to the
/// human, while a plain `git commit` / `rebase` / `merge` / etc. passes to
/// the judge for a verdict. The function is pure — it tokenizes the command
/// string with the existing [`tokenize`] lexer and inspects the result, with
/// no I/O and no access to the judge or the sandbox.
pub(crate) fn git_prefilter(command: &str) -> GitPrefilterVerdict {
    // Shell redirects (`>` `<`) and command substitution (`$(` backtick) are
    // not surfaced as `Boundary` by `tokenize`; detect them here, quote-aware.
    if contains_shell_redirect_or_substitution(command) {
        return GitPrefilterVerdict::HumanDirect(
            "a shell pipe, redirect, command substitution, or compound command",
        );
    }
    // A compound command (`;` `&&` `||` `|` newline `(` `)`) produces more
    // than one segment.
    let segments = segments_of(command);
    if segments.len() > 1 {
        return GitPrefilterVerdict::HumanDirect(
            "a shell pipe, redirect, command substitution, or compound command",
        );
    }
    let words = &segments[0];
    let Some(git_index) = git_executable_index(words) else {
        return GitPrefilterVerdict::HumanDirect("an unrecognized command shape");
    };
    // Environment-variable assignment prefix before `git` (VAR=x git ...).
    for word in &words[..git_index] {
        if is_assignment(word) {
            return GitPrefilterVerdict::HumanDirect(
                "an environment-variable assignment prefix before `git`",
            );
        }
    }
    // Dangerous Git options anywhere after `git` (global or subcommand-level).
    for arg in &words[git_index + 1..] {
        if PREFILTER_DANGEROUS_OPTIONS.contains(&arg.as_str())
            || PREFILTER_DANGEROUS_EQ_PREFIXES
                .iter()
                .any(|prefix| arg.starts_with(prefix))
        {
            return GitPrefilterVerdict::HumanDirect(
                "a Git option that can redirect execution or configuration \
                 (-c, --config-env, --exec-path, --upload-pack, --receive-pack, \
                 --git-dir, or --work-tree)",
            );
        }
    }
    // Walk the global options to find the subcommand, checking for dangerous
    // subcommands and unknown global flags.
    let mut index = git_index + 1;
    while let Some(arg) = words.get(index).map(String::as_str) {
        match arg {
            "-C" | "--namespace" => index += 2,
            value if value.starts_with("--namespace=") => index += 1,
            "--no-pager"
            | "--paginate"
            | "--no-replace-objects"
            | "--bare"
            | "--literal-pathspecs"
            | "--glob-pathspecs"
            | "--noglob-pathspecs"
            | "--icase-pathspecs"
            | "--no-optional-locks"
            | "--version"
            | "--help" => index += 1,
            value if value.starts_with('-') => {
                return GitPrefilterVerdict::HumanDirect("an unrecognized Git global option");
            }
            subcommand => {
                if PREFILTER_DANGEROUS_SUBCOMMANDS.contains(&subcommand)
                    || subcommand.starts_with("filter-")
                {
                    return GitPrefilterVerdict::HumanDirect(
                        "a Git subcommand that can execute arbitrary code or modify \
                         trust settings (config, hook, filter-branch, filter-repo, \
                         or credential)",
                    );
                }
                break;
            }
        }
    }
    GitPrefilterVerdict::PassToJudge
}

/// Splits a command into shell segments separated by `tokenize`'s `Boundary`
/// tokens, each segment a list of words.
fn segments_of(command: &str) -> Vec<Vec<String>> {
    let mut segments = Vec::new();
    let mut segment = Vec::new();
    for token in tokenize(command) {
        match token {
            ShellToken::Word(word) => segment.push(word),
            ShellToken::Boundary => {
                segments.push(std::mem::take(&mut segment));
            }
        }
    }
    segments.push(segment);
    segments
}

/// Detects shell redirects (`>`, `<`) and command substitution (`$(`,
/// backtick) that `tokenize` does not surface as `Boundary`. Single-quoted
/// content is skipped (literal in the shell); double-quoted content is scanned
/// for `$(` and backtick, which the shell still expands there.
fn contains_shell_redirect_or_substitution(command: &str) -> bool {
    let mut chars = command.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => match ch {
                '"' => quote = None,
                '\\' => {
                    chars.next();
                }
                '`' => return true,
                '$' if chars.peek() == Some(&'(') => return true,
                _ => {}
            },
            Some(_) => unreachable!(),
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '\\' => {
                    chars.next();
                }
                '>' | '<' | '`' => return true,
                '$' if chars.peek() == Some(&'(') => return true,
                _ => {}
            },
        }
    }
    false
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
    fn prefilter_routes_dangerous_constructs_to_human_direct() {
        let dangerous = [
            // Dangerous global options.
            "git -c core.hooksPath=/dev/null commit -m x",
            "git --config-env KEY=val commit",
            "git --exec-path /tmp commit",
            "git --git-dir /tmp commit",
            "git --work-tree /tmp commit",
            "git --upload-pack /tmp/x fetch",
            "git --receive-pack /tmp/x push",
            "git --git-dir=/tmp commit",
            // Environment-variable assignment prefix.
            "GIT_DIR=/tmp git commit -m x",
            "FOO=bar git commit -m x",
            "env FOO=bar git commit -m x",
            // Shell metacharacters.
            "git commit -m x | cat",
            "git commit -m x && git push",
            "git commit -m x; git push",
            "git commit -m x > log",
            "git commit -m \"$(whoami)\"",
            // Dangerous subcommands.
            "git config user.name x",
            "git hook run pre-commit",
            "git filter-branch --all",
        ];
        for command in dangerous {
            assert!(
                matches!(git_prefilter(command), GitPrefilterVerdict::HumanDirect(_)),
                "expected HumanDirect for: {command}"
            );
        }
    }

    #[test]
    fn prefilter_passes_plain_git_metadata_operations_to_judge() {
        let plain = [
            "git commit -m test",
            "git rebase main",
            "git merge feature",
            "git cherry-pick abc123",
            "git add file.txt",
            "git restore file.txt",
            "git stash",
            "git branch topic",
            "git tag v1.0",
            "git -C ../repo commit -m test",
            "git --no-pager commit -m test",
            "git commit --amend",
            "git commit -m \"fix: handle x > y\"",
        ];
        for command in plain {
            assert_eq!(
                git_prefilter(command),
                GitPrefilterVerdict::PassToJudge,
                "expected PassToJudge for: {command}"
            );
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
