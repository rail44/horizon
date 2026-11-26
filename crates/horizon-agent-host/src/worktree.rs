//! Session worktrees -- `docs/session-relationship-design.md` decision 5 and
//! `docs/sandbox-perimeter-design.md`. A session that enters a worktree
//! (environment activation: `worktree_enter`, the board's `implement`) gets
//! its own `git worktree` under the target repository's
//! `.horizon/worktrees/<slug>`, branched at the requested base; restore
//! re-adopts it, and it is removed (never the branch) on a clean terminate
//! (decision 5). Everything here shells out to the `git` binary rather than
//! adding a libgit2/gix dependency -- consistent with there being no existing
//! git-library dependency anywhere in this workspace, and `git worktree`/
//! `git worktree remove`'s own dirtiness check already implements exactly the
//! "no uncommitted changes" rule decision 5 asks for, so there is no reason to
//! duplicate it with a hand-rolled status parse.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// A session's own isolated worktree: enough to confine its file tools to
/// `path`, and to let a later child spawned *from* this session (decision
/// 3's multi-level chaining) find both the branch point (`path`'s HEAD) and
/// where sibling worktrees belong (`repo_root`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeInfo {
    pub repo_root: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub base: String,
}

/// The worktree directory name / branch suffix derived from a session id:
/// short enough to keep `.horizon/worktrees/<slug>` paths reasonable, long
/// enough that two live sessions colliding is practically a non-issue.
pub fn short_slug(session_id: Uuid) -> String {
    session_id.simple().to_string()[..8].to_string()
}

/// Strips every inherited `GIT_*` environment variable from `cmd`. Git
/// honors several of these (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
/// `GIT_COMMON_DIR`, ...) as overrides that take precedence over `-C`: an
/// absolute `GIT_DIR` in particular silently redirects an entire invocation
/// to a *different* repository, regardless of `-C`'s target directory.
/// Git exports exactly such an absolute `GIT_DIR` (pointing at
/// `$GIT_COMMON_DIR/worktrees/<name>`) to hook processes invoked from a
/// *linked* worktree -- which every session in this repo works from. When a
/// hook runs the tests, that env var propagates through cargo -> nextest ->
/// the test binary -> a `git` subprocess spawned here, none of which
/// sanitize it. Every git
/// invocation in this module (production and test) must scrub it so `-C
/// <dir>` is the sole source of truth for which repository is targeted.
fn scrub_git_env(cmd: &mut std::process::Command) {
    for (key, _) in std::env::vars() {
        if key.starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
}

fn run_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    run_git_in(dir, args, &[])
}

/// [`run_git`] with `envs` set for the command, after the scrub.
pub fn run_git_in(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    scrub_git_env(&mut cmd);
    cmd.envs(envs.iter().copied());
    let output = cmd
        .output()
        .map_err(|error| format!("failed to run git {args:?} in {}: {error}", dir.display()))?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `reference` in `dir`'s repository, resolved to a full commit id.
pub fn resolve_commit(dir: &Path, reference: &str) -> Result<String, String> {
    run_git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )
}

/// The repository's shared git dir (`<repo_root>/.git` for an ordinary
/// non-bare repo), resolved from `dir` -- which may itself already be a
/// linked worktree, in which case `--show-toplevel` would report *that*
/// worktree's own path, not the main repository's. `--git-common-dir` is
/// shared by every worktree of the same repository, so it's the stable way
/// to find the one true repo root regardless of which worktree `dir` is.
fn git_common_dir(dir: &Path) -> Result<PathBuf, String> {
    run_git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(PathBuf::from)
}

/// The project `dir` belongs to, for `[grants]` lookup
/// (`docs/containment-denial-narrow-grants-design.md`'s 2026-07-26
/// decision): the *main* repository's toplevel, canonicalized.
///
/// Resolved through `--git-common-dir` rather than `--show-toplevel`, which
/// is the whole point: the common dir is shared by every worktree of a
/// repository, so an isolated session working in `.horizon/worktrees/<slug>`
/// resolves to the repository it was derived from and inherits that
/// project's grants — as does a session in any other linked worktree
/// (`.claude/worktrees/*`, a hand-made one, ...). The same call in an
/// ordinary checkout returns that checkout. This needs no persisted lineage
/// and no live parent session, so it answers identically for a fresh
/// session, a resumed one, and one whose parent has since terminated.
///
/// `None` when `dir` is not in a Git repository at all, or the repository's
/// paths cannot be canonicalized. A session with no resolvable project
/// simply gets no configured grants; nothing degrades to a wider default.
pub fn project_root(dir: &Path) -> Option<PathBuf> {
    let common_dir = git_common_dir(dir).ok()?;
    let repo_root = repo_root_from_common_dir(&common_dir).ok()?;
    std::fs::canonicalize(repo_root).ok()
}

fn repo_root_from_common_dir(common_dir: &Path) -> Result<PathBuf, String> {
    common_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("git common dir {} has no parent", common_dir.display()))
}

/// Best-effort: makes sure `.horizon` won't show up as untracked clutter in
/// the target repository's own `git status`, mirroring this repo's own
/// `/.horizon` `.gitignore` entry -- but via `.git/info/exclude` rather than
/// editing the target repo's tracked `.gitignore`, since the host has no
/// business committing a change to a file the repo's own history owns.
/// Never fails the worktree creation itself: an ignore-file write is a
/// nicety, not a correctness requirement.
fn ensure_horizon_ignored(common_dir: &Path) {
    let Ok(repo_root) = repo_root_from_common_dir(common_dir) else {
        return;
    };
    let exclude_path = common_dir.join("info").join("exclude");
    let mut content = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    let mut changed = false;

    if run_git(&repo_root, &["check-ignore", "-q", ".horizon"]).is_err()
        && !content.lines().any(|line| line.trim() == "/.horizon")
    {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("/.horizon\n");
        changed = true;
    }

    if !changed {
        return;
    }
    if let Some(parent) = exclude_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&exclude_path, content);
}

/// Creates a fresh worktree for `session_id`: discovers `source_dir`'s
/// repository, then `git worktree add -b horizon/<slug>
/// <repo_root>/.horizon/worktrees/<slug> <base>`, with `base` resolved in
/// `source_dir`. Branch naming is stable and session-derived
/// (`horizon/<slug>`); the directory lives at the repository root regardless
/// of whether `source_dir` is itself a linked worktree, so a worktree created
/// from another one is never nested inside it. Environment activation
/// (`worktree_enter`, the board's `implement`) is the only production caller.
pub fn create_isolated_worktree_at(
    source_dir: &Path,
    session_id: Uuid,
    base: &str,
) -> Result<WorktreeInfo, String> {
    let common_dir = git_common_dir(source_dir)?;
    let repo_root = repo_root_from_common_dir(&common_dir)?;
    ensure_horizon_ignored(&common_dir);

    let base_ref = run_git(
        source_dir,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
    )?;

    let slug = short_slug(session_id);
    let worktree_path = repo_root.join(".horizon").join("worktrees").join(&slug);
    let branch = session_branch(&slug);
    let worktree_path_str = worktree_path.to_str().ok_or_else(|| {
        format!(
            "worktree path {} is not valid UTF-8",
            worktree_path.display()
        )
    })?;

    run_git(
        &repo_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            worktree_path_str,
            &base_ref,
        ],
    )?;

    Ok(WorktreeInfo {
        repo_root,
        path: worktree_path,
        branch,
        base: base_ref,
    })
}

/// Whether `dir`'s working tree has changes not yet committed -- a write
/// child starts from its requester's `HEAD`, so what is not committed
/// there would not reach it (`docs/agent-subagent-design.md` decision 3).
pub fn has_uncommitted_changes(dir: &Path) -> Result<bool, String> {
    Ok(!run_git(dir, &["status", "--porcelain"])?.trim().is_empty())
}

/// Whether a worktree holds work worth keeping: commits its branch alone
/// has, or changes not committed (`docs/agent-subagent-design.md` decision
/// 11). "Its branch alone" rather than "past its base": a worktree adopted
/// after a restart knows only its current `HEAD` as its base, while a tip
/// that no other branch contains is a commit made there. An unreadable
/// worktree counts as holding work, so it is never removed on a guess.
pub fn holds_work(info: &WorktreeInfo) -> bool {
    let committed = run_git(
        &info.repo_root,
        &[
            "branch",
            "--format=%(refname:short)",
            "--contains",
            &format!("refs/heads/{}", info.branch),
        ],
    )
    .map(|branches| branches.lines().all(|branch| branch.trim() == info.branch))
    .unwrap_or(true);
    committed || has_uncommitted_changes(&info.path).unwrap_or(true)
}

/// The branch a session's worktree is made on: one per session, in a
/// directory of its own under `refs/heads/horizon/`, so that the session can
/// be let write its own ref without being let write any other session's
/// (`docs/agent-subagent-design.md` decision 10). Git updates a ref by
/// writing `<ref>.lock` beside it and renaming it into place, which the
/// sandbox allows only to a writable directory.
fn session_branch(slug: &str) -> String {
    format!("horizon/{slug}/work")
}

/// What a session in its own worktree may write of the repository's Git
/// metadata without asking (`docs/agent-subagent-design.md` decision 10):
/// the shared `.git`, but what would have Git run something outside the
/// sandbox -- the repository's `hooks` and `config`, each worktree's
/// `config.worktree` (which can set `core.hooksPath`), `commondir` and
/// `gitdir` (which point Git elsewhere), and the submodules' gitdirs
/// (`modules`, each with hooks and a config of its own). A sandbox keeps a
/// path read-only by mounting over it, so an absent `hooks` or
/// `config.worktree` of the session's own is made empty first: empty, it
/// runs and sets nothing. Empty for a worktree that is not one made for the
/// session (its branch `horizon/<slug>/work`), or whose layout does not
/// check out.
pub fn own_metadata_grants(
    worktree: &Path,
    session_id: Uuid,
) -> Vec<horizon_sandbox::FilesystemGrant> {
    own_metadata_grants_checked(worktree, session_id).unwrap_or_default()
}

fn own_metadata_grants_checked(
    worktree: &Path,
    session_id: Uuid,
) -> Option<Vec<horizon_sandbox::FilesystemGrant>> {
    use horizon_sandbox::{FilesystemGrant, FilesystemGrantAccess, FilesystemGrantScope};

    let absolute = |flag: &str| {
        run_git(worktree, &["rev-parse", "--path-format=absolute", flag])
            .ok()
            .and_then(|path| std::fs::canonicalize(path.trim()).ok())
    };
    let common = absolute("--git-common-dir")?;
    let gitdir = absolute("--git-dir")?;
    if !gitdir.starts_with(common.join("worktrees")) {
        return None;
    }
    let slug = short_slug(session_id);
    if !common
        .join("refs/heads/horizon")
        .join(&slug)
        .join("work")
        .is_file()
    {
        return None;
    }
    std::fs::create_dir_all(common.join("hooks")).ok()?;
    let config_worktree = gitdir.join("config.worktree");
    if !config_worktree.exists() {
        std::fs::write(&config_worktree, "").ok()?;
    }
    let mut excluded: Vec<PathBuf> = vec!["hooks".into(), "config".into()];
    if common.join("modules").exists() {
        excluded.push("modules".into());
    }
    for entry in std::fs::read_dir(common.join("worktrees")).ok()?.flatten() {
        for name in ["config.worktree", "commondir", "gitdir"] {
            let path = entry.path().join(name);
            if path.exists() {
                excluded.push(
                    PathBuf::from("worktrees")
                        .join(entry.file_name())
                        .join(name),
                );
            }
        }
    }
    Some(vec![FilesystemGrant {
        path: common,
        access: FilesystemGrantAccess::ReadWrite,
        scope: FilesystemGrantScope::DirectoryTree,
        excluded_subpaths: excluded,
    }])
}

/// Re-adopts the existing worktree owned by a resumed session without
/// creating or checking out anything. Every relationship is recomputed from
/// Git and canonical paths; persisted text alone never grants an arbitrary
/// directory isolated-session trust.
pub fn adopt_isolated_worktree(
    worktree_path: &Path,
    session_id: Uuid,
) -> Result<WorktreeInfo, String> {
    let canonical_path = std::fs::canonicalize(worktree_path).map_err(|error| {
        format!(
            "could not canonicalize persisted worktree {}: {error}",
            worktree_path.display()
        )
    })?;
    if !canonical_path.join(".git").is_file() {
        return Err(format!(
            "persisted worktree {} has no linked-worktree .git file",
            canonical_path.display()
        ));
    }

    let reported_root = PathBuf::from(run_git(
        &canonical_path,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
    )?);
    let reported_root = std::fs::canonicalize(&reported_root).map_err(|error| {
        format!(
            "could not canonicalize Git toplevel {}: {error}",
            reported_root.display()
        )
    })?;
    if reported_root != canonical_path {
        return Err(format!(
            "persisted worktree {} reports a different Git toplevel {}",
            canonical_path.display(),
            reported_root.display()
        ));
    }

    let common_dir = std::fs::canonicalize(git_common_dir(&canonical_path)?)
        .map_err(|error| format!("could not canonicalize persisted Git common dir: {error}"))?;
    let repo_root = std::fs::canonicalize(repo_root_from_common_dir(&common_dir)?)
        .map_err(|error| format!("could not canonicalize persisted repository root: {error}"))?;
    let slug = short_slug(session_id);
    let expected_path = repo_root.join(".horizon").join("worktrees").join(&slug);
    if canonical_path != expected_path {
        return Err(format!(
            "persisted worktree {} is not the expected path {} for session {session_id}",
            canonical_path.display(),
            expected_path.display()
        ));
    }

    let git_dir = PathBuf::from(run_git(
        &canonical_path,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    )?);
    let git_dir = std::fs::canonicalize(&git_dir)
        .map_err(|error| format!("could not canonicalize persisted worktree gitdir: {error}"))?;
    if !git_dir.starts_with(common_dir.join("worktrees")) {
        return Err(format!(
            "persisted worktree gitdir {} is outside {}",
            git_dir.display(),
            common_dir.join("worktrees").display()
        ));
    }

    let branch = run_git(&canonical_path, &["branch", "--show-current"])?;
    Ok(WorktreeInfo {
        repo_root,
        base: run_git(&canonical_path, &["rev-parse", "HEAD"])?,
        path: canonical_path,
        branch,
    })
}

/// Decision 5's terminate-cleanup rule: removes `info`'s worktree if it's
/// clean, keeps it (returns `false`) if it has any uncommitted or untracked
/// changes -- `git worktree remove` already refuses a dirty worktree on its
/// own (without `--force`), which is exactly "no uncommitted changes", so
/// this doesn't duplicate that check with a hand-rolled `git status` parse.
/// The branch itself is never deleted either way.
pub fn remove_worktree_if_clean(info: &WorktreeInfo) -> bool {
    let Some(path_str) = info.path.to_str() else {
        return false;
    };
    run_git(&info.repo_root, &["worktree", "remove", path_str]).is_ok()
}

impl WorktreeInfo {
    pub fn identity(&self) -> horizon_agent::contract::SessionWorktree {
        horizon_agent::contract::SessionWorktree {
            repository: self.repo_root.clone(),
            path: self.path.clone(),
            branch: self.branch.clone(),
            base: self.base.clone(),
        }
    }
}

/// Restore only the retained branch and exact owned placement. Never recreate
/// a missing branch at an arbitrary base or overwrite a dirty existing tree.
pub fn restore_worktree(
    identity: &horizon_agent::contract::SessionWorktree,
    session_id: Uuid,
) -> Result<WorktreeInfo, String> {
    let slug = short_slug(session_id);
    let repository =
        std::fs::canonicalize(&identity.repository).map_err(|error| error.to_string())?;
    let expected_path = repository
        .join(".horizon/worktrees")
        .join(short_slug(session_id));
    // A worktree made before branches moved under the session's own
    // directory keeps its `horizon/<slug>` branch; it is still the
    // session's, and only lacks the metadata grant (`own_metadata_grants`).
    if identity.path != expected_path
        || (identity.branch != session_branch(&slug)
            && identity.branch != format!("horizon/{slug}"))
    {
        return Err("Retained environment identity does not match session ownership".into());
    }
    run_git(
        &repository,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{}^{{commit}}", identity.base),
        ],
    )?;
    run_git(
        &repository,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", identity.branch),
        ],
    )?;
    run_git(
        &repository,
        &[
            "merge-base",
            "--is-ancestor",
            &identity.base,
            &format!("refs/heads/{}", identity.branch),
        ],
    )?;
    if !identity.path.exists() {
        let path = identity.path.to_str().ok_or("Worktree path is not UTF-8")?;
        run_git(&repository, &["worktree", "add", path, &identity.branch])?;
    }
    let mut adopted = adopt_isolated_worktree(&identity.path, session_id)?;
    if adopted.repo_root != repository || adopted.branch != identity.branch {
        return Err("Restored worktree differs from retained repository or branch".into());
    }
    adopted.base = identity.base.clone();
    Ok(adopted)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Pure logic ---------------------------------------------------

    /// `create_isolated_worktree_at` at `source_dir`'s working-tree HEAD.
    fn create_isolated_worktree(
        source_dir: &Path,
        session_id: Uuid,
    ) -> Result<WorktreeInfo, String> {
        create_isolated_worktree_at(source_dir, session_id, "HEAD")
    }

    #[test]
    fn short_slug_is_short_and_deterministic_for_the_same_id() {
        let id = Uuid::new_v4();
        let slug = short_slug(id);
        assert_eq!(slug.len(), 8);
        assert!(slug.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(slug, short_slug(id));
    }

    // --- Real git, in temp repositories --------------------------------

    fn git(dir: &Path, args: &[&str]) {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(dir).args(args);
        scrub_git_env(&mut cmd);
        // Tests are additionally hermetic against the machine's global and
        // system git config (a global core.hooksPath was observed turning
        // fixture commits into >60s toolchain invocations under full-suite
        // parallelism); production run_git deliberately is not -- the
        // host operates on the owner's real repository, where the
        // owner's config is intent, and the repo-local hooksPath governs.
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
        let status = cmd
            .status()
            .unwrap_or_else(|error| panic!("failed to run git {args:?}: {error}"));
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    // --- Hermeticity canary (backlog 53) -------------------------------
    //
    // 2026-07-18: a leaked `GIT_DIR` (see `scrub_git_env`'s doc) made every
    // "isolated" TempDir-scoped git call below actually operate on the
    // enclosing repository -- flipping its `core.bare` to `true`, spawning
    // a `.horizon/worktrees/` skeleton at its root, and landing a phantom
    // commit (tree content matching `scratch_repo`'s fixture) on whatever
    // branch was checked out in the worktree that ran the tests. Every real
    // -git test below captures an `EnclosingRepoGuard` first: it snapshots
    // the enclosing repo (discovered once from `CARGO_MANIFEST_DIR`, a
    // compile-time constant immune to any runtime env/cwd leak) and
    // re-asserts it unchanged on drop, so any future escape fails loudly
    // right at the offending test instead of surfacing as unrelated damage
    // discovered later. The snapshot covers `core.bare`, working-tree
    // status, `horizon/*` branches, and the committer identity
    // (`user.name`/`user.email`) -- the last of these specifically catches
    // backlog 53's identity-config pollution, where a test writing
    // `git config user.*` under a leaked `GIT_DIR` landed in the enclosing
    // repo's shared config and silently re-authored every subsequent commit.

    #[derive(Debug, PartialEq, Eq)]
    struct EnclosingRepoState {
        /// `false`/unset in every sane repo; the leak observed in practice
        /// flipped this to `true` on the shared (common) config, breaking
        /// `git status` for every worktree of the repo at once.
        bare: String,
        /// Catches both a stray untracked `.horizon/` directory and any
        /// other working-tree/index fallout from an escaped commit.
        status: String,
        /// Catches a stray `horizon/<slug>` branch landing in the
        /// enclosing repo instead of a scratch repo.
        horizon_branches: String,
        /// Backlog 53: catches a test's `Test <test@example.com>` identity
        /// leaking into the enclosing repo's shared config (via a `git config
        /// user.*` write under a leaked `GIT_DIR`), which would silently
        /// re-author every later commit made from any worktree of this repo.
        /// Empty when unset. Merged config (`--get`), so it reflects exactly
        /// the identity commits would actually use.
        user_name: String,
        user_email: String,
    }

    fn enclosing_repo_root() -> PathBuf {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(manifest_dir).args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
        ]);
        scrub_git_env(&mut cmd);
        let output = cmd
            .output()
            .expect("discovering the enclosing repo's root should never fail");
        assert!(
            output.status.success(),
            "git rev-parse --show-toplevel in {} failed: {}",
            manifest_dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
    }

    fn enclosing_repo_state(root: &Path) -> EnclosingRepoState {
        EnclosingRepoState {
            bare: run_git(root, &["config", "--get", "core.bare"])
                .unwrap_or_else(|_| "false".to_string()),
            status: run_git(root, &["status", "--porcelain"]).unwrap_or_default(),
            horizon_branches: run_git(
                root,
                &["for-each-ref", "--format=%(refname)", "refs/heads/horizon"],
            )
            .unwrap_or_default(),
            user_name: run_git(root, &["config", "--get", "user.name"]).unwrap_or_default(),
            user_email: run_git(root, &["config", "--get", "user.email"]).unwrap_or_default(),
        }
    }

    struct EnclosingRepoGuard {
        root: PathBuf,
        before: EnclosingRepoState,
    }

    impl EnclosingRepoGuard {
        fn capture() -> Self {
            let root = enclosing_repo_root();
            let before = enclosing_repo_state(&root);
            Self { root, before }
        }
    }

    impl Drop for EnclosingRepoGuard {
        fn drop(&mut self) {
            // Don't mask a genuine test failure's panic with a canary
            // panic during unwind -- and don't risk a double-panic abort.
            if std::thread::panicking() {
                return;
            }
            let after = enclosing_repo_state(&self.root);
            assert_eq!(
                self.before,
                after,
                "hermeticity canary: the enclosing repository at {} changed during \
                 a worktree test -- a git invocation escaped its TempDir scratch repo \
                 (see worktree.rs's scrub_git_env doc / backlog 53)",
                self.root.display()
            );
        }
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
    }

    /// Commits `name` in `dir` with a deterministic `Test` identity supplied
    /// *per invocation* via `-c user.name`/`-c user.email` -- never written
    /// into any git config. Backlog 53: persisting that identity with `git
    /// config user.*` (as this used to) meant that under a leaked absolute
    /// `GIT_DIR` (see `scrub_git_env`) the write landed in the *enclosing*
    /// real repository's shared config, silently re-authoring every later
    /// commit as `Test <test@example.com>`. A `-c` override touches no config
    /// and still stamps both the author and committer, so a CI box with no
    /// global identity commits fine and nothing leaks even if `-C` is bypassed.
    fn commit_file(dir: &Path, name: &str, contents: &str, message: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
        git(dir, &["add", name]);
        git(
            dir,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                message,
            ],
        );
    }

    /// The base "no origin remote" shape every other real-git test builds
    /// on: an initialized repo with one commit, so `HEAD` always resolves.
    fn scratch_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create temp dir");
        init_repo(dir.path());
        commit_file(dir.path(), "README.md", "root\n", "root commit");
        dir
    }

    // --- project root resolution ---------------------------------------
    //
    // The `[grants]` lookup key (`docs/containment-denial-narrow-grants-
    // design.md`'s 2026-07-26 decision). The load-bearing case is the third
    // one: an isolated session must resolve to the repository it was
    // derived from, or every isolated session -- which is every session
    // that can reach tier 1 at all -- would silently miss its project's
    // grants.

    #[test]
    fn project_root_of_an_ordinary_checkout_is_that_checkout() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let canonical = std::fs::canonicalize(repo.path()).unwrap();

        assert_eq!(project_root(repo.path()), Some(canonical));
    }

    #[test]
    fn explicit_base_and_retained_branch_restore_preserve_same_environment() {
        let repo = scratch_repo();
        init_repo(repo.path());
        commit_file(repo.path(), "file", "base", "base");
        let base = run_git(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        commit_file(repo.path(), "file", "later", "later");
        let id = Uuid::new_v4();
        let worktree = create_isolated_worktree_at(repo.path(), id, &base).unwrap();
        assert_eq!(
            std::fs::read_to_string(worktree.path.join("file")).unwrap(),
            "base"
        );
        commit_file(&worktree.path, "file", "session tip", "session tip");
        let identity = worktree.identity();
        assert!(remove_worktree_if_clean(&worktree));
        let restored = restore_worktree(&identity, id).unwrap();
        assert_eq!(restored.base, base);
        assert_eq!(
            std::fs::read_to_string(restored.path.join("file")).unwrap(),
            "session tip"
        );
        std::fs::write(restored.path.join("file"), "dirty").unwrap();
        assert!(!remove_worktree_if_clean(&restored));
        restore_worktree(&identity, id).unwrap();
        assert_eq!(
            std::fs::read_to_string(restored.path.join("file")).unwrap(),
            "dirty"
        );
        assert!(create_isolated_worktree_at(repo.path(), Uuid::new_v4(), "missing-base").is_err());
    }

    #[test]
    fn retained_branch_must_still_descend_from_the_selected_base() {
        let repo = scratch_repo();
        init_repo(repo.path());
        commit_file(repo.path(), "file", "old", "old");
        let old = run_git(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        commit_file(repo.path(), "file", "base", "base");
        let id = Uuid::new_v4();
        let worktree = create_isolated_worktree_at(repo.path(), id, "HEAD").unwrap();
        let identity = worktree.identity();
        assert!(remove_worktree_if_clean(&worktree));
        run_git(repo.path(), &["branch", "-f", &identity.branch, &old]).unwrap();
        assert!(restore_worktree(&identity, id).is_err());
        assert!(!identity.path.exists());
    }

    #[test]
    fn removed_branch_never_falls_back_to_recorded_base() {
        let repo = scratch_repo();
        init_repo(repo.path());
        commit_file(repo.path(), "file", "base", "base");
        let id = Uuid::new_v4();
        let worktree = create_isolated_worktree_at(repo.path(), id, "HEAD").unwrap();
        let identity = worktree.identity();
        assert!(remove_worktree_if_clean(&worktree));
        run_git(repo.path(), &["branch", "-D", &identity.branch]).unwrap();
        assert!(restore_worktree(&identity, id).is_err());
        assert!(!identity.path.exists());
    }

    #[test]
    fn project_root_of_a_subdirectory_is_still_the_repository_toplevel() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let nested = repo.path().join("crates").join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        let canonical = std::fs::canonicalize(repo.path()).unwrap();

        assert_eq!(project_root(&nested), Some(canonical));
    }

    #[test]
    fn project_root_of_a_derived_worktree_is_its_source_repository() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let info = create_isolated_worktree(repo.path(), Uuid::new_v4())
            .expect("create isolated worktree");
        let canonical = std::fs::canonicalize(repo.path()).unwrap();

        // The worktree is its own Git toplevel, so a `--show-toplevel`-based
        // resolution would answer with the worktree and miss the project.
        assert_ne!(std::fs::canonicalize(&info.path).unwrap(), canonical);
        assert_eq!(
            project_root(&info.path),
            Some(canonical),
            "an isolated worktree must inherit its source repository's project identity"
        );

        remove_worktree_if_clean(&info);
    }

    #[test]
    fn project_root_of_a_directory_outside_any_repository_is_none() {
        let _canary = EnclosingRepoGuard::capture();
        let dir = tempfile::tempdir().expect("create temp dir");

        // A bare temp dir has no repository above it only if the temp root
        // itself isn't inside one -- which is the normal case, and the only
        // one where this assertion means anything.
        if project_root(dir.path()).is_some() {
            return;
        }
        assert_eq!(project_root(dir.path()), None);
    }

    /// Backlog 53 root cause + fix, exercised end to end: `commit_file` must
    /// stamp a deterministic `Test` author (so tests commit fine on a CI box
    /// with no global git identity) *purely* via a per-invocation `-c
    /// user.*` override -- writing nothing to any git config, so that even
    /// under a leaked absolute `GIT_DIR` (which makes `-C <dir>` operate on
    /// the enclosing repo) no identity can land in the enclosing repo's
    /// shared config and re-author its future commits. Proves both halves:
    /// the temp commit carries the `Test` identity, `init_repo` left no
    /// identity in the temp repo's own config (so it came from the override),
    /// and the enclosing repo's `user.name`/`user.email` are untouched. The
    /// `EnclosingRepoGuard` now also asserts the identity half on drop for
    /// every real-git test; this test additionally proves the override works.
    #[test]
    fn commit_file_stamps_the_test_identity_without_writing_git_config() {
        let _canary = EnclosingRepoGuard::capture();
        let enclosing = enclosing_repo_root();
        let before_name =
            run_git(&enclosing, &["config", "--get", "user.name"]).unwrap_or_default();
        let before_email =
            run_git(&enclosing, &["config", "--get", "user.email"]).unwrap_or_default();

        // scratch_repo() calls init_repo + commit_file for the root commit.
        let repo = scratch_repo();

        assert_eq!(
            run_git(repo.path(), &["log", "-1", "--format=%an"]).expect("HEAD author name"),
            "Test",
            "the per-commit -c override must stamp the author name"
        );
        assert_eq!(
            run_git(repo.path(), &["log", "-1", "--format=%ae"]).expect("HEAD author email"),
            "test@example.com",
            "the per-commit -c override must stamp the author email"
        );
        assert!(
            run_git(repo.path(), &["config", "--local", "--get", "user.name"]).is_err(),
            "init_repo must not persist an identity in the temp repo's own config"
        );

        assert_eq!(
            run_git(&enclosing, &["config", "--get", "user.name"]).unwrap_or_default(),
            before_name,
            "the enclosing repo's user.name must be unchanged by a temp-repo commit"
        );
        assert_eq!(
            run_git(&enclosing, &["config", "--get", "user.email"]).unwrap_or_default(),
            before_email,
            "the enclosing repo's user.email must be unchanged by a temp-repo commit"
        );
    }

    #[test]
    fn create_isolated_worktree_has_the_expected_branch_and_path_shape() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let session_id = Uuid::new_v4();

        let info = create_isolated_worktree(repo.path(), session_id)
            .expect("worktree creation should succeed");

        let slug = short_slug(session_id);
        assert_eq!(info.branch, format!("horizon/{slug}/work"));
        assert_eq!(
            info.path,
            repo.path()
                .canonicalize()
                .unwrap()
                .join(".horizon")
                .join("worktrees")
                .join(&slug)
        );
        assert!(info.path.is_dir(), "worktree directory should exist");
        assert_eq!(
            std::fs::read_to_string(info.path.join("README.md")).unwrap(),
            "root\n"
        );

        let current_branch =
            run_git(&info.path, &["branch", "--show-current"]).expect("branch --show-current");
        assert_eq!(current_branch, info.branch);
    }

    #[test]
    fn adopt_isolated_worktree_reconstructs_a_valid_session_worktree() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let session_id = Uuid::new_v4();
        let created = create_isolated_worktree(repo.path(), session_id)
            .expect("worktree creation should succeed");

        let adopted = adopt_isolated_worktree(&created.path, session_id)
            .expect("the owning session should be able to re-adopt its worktree");

        assert_eq!(adopted, created);
    }

    #[test]
    fn adopt_isolated_worktree_rejects_another_sessions_worktree() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let owner_session_id = Uuid::new_v4();
        let created = create_isolated_worktree(repo.path(), owner_session_id)
            .expect("worktree creation should succeed");

        let error = adopt_isolated_worktree(&created.path, Uuid::new_v4())
            .expect_err("a different session id must not adopt this worktree");

        assert!(
            error.contains("is not the expected path"),
            "unexpected rejection: {error}"
        );
    }

    #[test]
    fn adopt_isolated_worktree_rejects_an_ordinary_checkout() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();

        let error = adopt_isolated_worktree(repo.path(), Uuid::new_v4())
            .expect_err("an ordinary checkout is not an owned linked worktree");

        assert!(
            error.contains("has no linked-worktree .git file"),
            "unexpected rejection: {error}"
        );
    }

    #[test]
    fn create_isolated_worktree_branches_from_the_launching_checkout_head() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        commit_file(repo.path(), "second.txt", "second\n", "second commit");

        let info = create_isolated_worktree(repo.path(), Uuid::new_v4())
            .expect("worktree creation should succeed");

        // The new worktree must include the *local* tip of the source
        // directory (issue 006: a launcher's own commits, pushed or not,
        // are what the child should see -- not origin's stale default).
        assert!(info.path.join("second.txt").is_file());
        assert!(info.path.join("README.md").is_file());
    }

    #[test]
    fn create_isolated_worktree_includes_unpushed_local_commits_ahead_of_origin() {
        // Issue 006's direct repro: the launching checkout has a local
        // commit past `origin/HEAD`, and the isolated worktree must
        // branch from that commit (so the child sees the exact code state
        // the launcher dispatched from), not from `origin/main`.
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();

        // A bare "origin" the local repo tracks, one commit ahead of it --
        // simulates a properly cloned repo (`git clone` sets `origin/HEAD`
        // for us; here it's done explicitly since there was no clone).
        let origin = tempfile::tempdir().expect("create temp dir");
        git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
        git(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-q", "origin", "main"]);
        git(repo.path(), &["remote", "set-head", "origin", "-a"]);
        // A local-only commit past what origin has -- the new worktree
        // MUST include this, proving it branched from the launching
        // checkout's HEAD (not origin/HEAD, which would silently drop it).
        commit_file(
            repo.path(),
            "local-only.txt",
            "local\n",
            "local-only commit",
        );

        let info = create_isolated_worktree(repo.path(), Uuid::new_v4())
            .expect("worktree creation should succeed");

        assert!(
            info.path.join("local-only.txt").is_file(),
            "a root spawn must branch from the launching checkout's HEAD, \
             including any local commits past origin"
        );
        assert!(info.path.join("README.md").is_file());
    }

    #[test]
    fn create_isolated_worktree_branches_from_the_source_head_when_the_source_is_owned() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let parent_id = Uuid::new_v4();
        let parent = create_isolated_worktree(repo.path(), parent_id)
            .expect("parent worktree creation should succeed");
        // Simulate the parent session's own agent having committed work in
        // its worktree -- the child must see this, since it's supposed to
        // derive from the parent's *worktree* HEAD, not the repo root's.
        commit_file(
            &parent.path,
            "parent-work.txt",
            "parent work\n",
            "parent work",
        );

        let child = create_isolated_worktree(&parent.path, Uuid::new_v4())
            .expect("child worktree creation should succeed");

        assert!(child.path.join("parent-work.txt").is_file());
        assert_eq!(child.repo_root, parent.repo_root);
    }

    #[test]
    fn ensure_horizon_ignored_adds_the_exclude_line_exactly_once() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        create_isolated_worktree(repo.path(), Uuid::new_v4())
            .expect("first worktree creation should succeed");
        create_isolated_worktree(repo.path(), Uuid::new_v4())
            .expect("second worktree creation should succeed");

        let exclude = std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap();
        assert_eq!(
            exclude
                .lines()
                .filter(|line| line.trim() == "/.horizon")
                .count(),
            1,
            "exclude file should list /.horizon exactly once, got:\n{exclude}"
        );
    }

    #[test]
    fn remove_worktree_if_clean_removes_a_clean_worktree_but_keeps_the_branch() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let info = create_isolated_worktree(repo.path(), Uuid::new_v4())
            .expect("worktree creation should succeed");

        assert!(remove_worktree_if_clean(&info));
        assert!(!info.path.exists(), "clean worktree should be removed");

        let branches = run_git(repo.path(), &["branch", "--list", &info.branch])
            .expect("branch --list should succeed");
        assert!(
            branches.contains(&info.branch),
            "the branch must survive worktree removal"
        );
    }

    #[test]
    fn remove_worktree_if_clean_keeps_a_dirty_worktree() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let info = create_isolated_worktree(repo.path(), Uuid::new_v4())
            .expect("worktree creation should succeed");
        std::fs::write(info.path.join("untracked.txt"), "dirty\n").unwrap();

        assert!(!remove_worktree_if_clean(&info));
        assert!(info.path.is_dir(), "dirty worktree must be kept");
    }

    /// `docs/agent-subagent-design.md` decision 10, through the real
    /// sandbox: a session in its own worktree commits there without asking,
    /// with nothing refused on the way (board #113), and still cannot
    /// write what would have Git run something outside the sandbox: the
    /// repository's hooks and config, or its worktree's `config.worktree`.
    /// Runs the real sandbox, so the `sandboxed` nextest profile skips it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_session_commits_in_its_own_worktree_and_reaches_no_hook_or_config() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let session_id = Uuid::new_v4();
        let info = create_isolated_worktree(repo.path(), session_id).unwrap();
        let grants = own_metadata_grants(&info.path, session_id);
        assert_eq!(grants.len(), 1, "{grants:?}");

        let common = info.repo_root.join(".git");
        let gitdir = common.join("worktrees").join(short_slug(session_id));
        let fence = horizon_sandbox::runtime::FenceRuntime::new(
            horizon_agent::tools::sandbox::fence_on_path(),
        );
        horizon_sandbox::runtime::SandboxRuntime::probe(&fence)
            .expect("this test needs Fence (https://github.com/fencesandbox/fence)");
        let mut containment = horizon_sandbox::runtime::Containment {
            writable: vec![info.path.clone()],
            read_only: vec![info.path.join(".git")],
            ..Default::default()
        };
        for grant in &grants {
            containment.writable.push(grant.path.clone());
            for excluded in &grant.excluded_subpaths {
                containment.read_only.push(grant.path.join(excluded));
            }
        }
        let scratch = tempfile::tempdir().unwrap();
        let run = |script: String| {
            let script =
                format!("export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null; {script}");
            let mut contained = horizon_sandbox::runtime::SandboxRuntime::contain(
                &fence,
                &script,
                &containment,
                scratch.path(),
            )
            .expect("contain the command");
            scrub_git_env(&mut contained.command);
            let output = contained.command.current_dir(&info.path).output().unwrap();
            (
                output.status.success(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )
        };

        let (committed, said) = run("echo changed > README.md && git add README.md && \
             git -c user.name=Test -c user.email=test@example.com commit -q -m change"
            .into());
        assert!(committed, "{said}");
        assert!(!said.contains("packed-refs.lock"), "{said}");
        assert_ne!(
            run_git(&info.repo_root, &["rev-parse", &info.branch]).unwrap(),
            info.base,
            "the commit moved the session's own branch"
        );

        // What the command sees of a path outside its writable ones may
        // not be the host's (Fence gives it a private `/tmp`, where this
        // fixture lives), so what is asserted is that nothing of these
        // changed, not how each attempt failed.
        let common_config_before = std::fs::read_to_string(common.join("config")).unwrap();
        run(format!(
            "echo '[core]' >> {}",
            gitdir.join("config.worktree").display()
        ));
        run(format!("echo x >> {}", common.join("config").display()));
        run(format!(
            "printf '#!/bin/sh\\nexit 1\\n' > {}",
            common.join("hooks/pre-commit").display()
        ));
        assert_eq!(
            std::fs::read_to_string(gitdir.join("config.worktree")).unwrap(),
            ""
        );
        assert_eq!(
            std::fs::read_to_string(common.join("config")).unwrap(),
            common_config_before
        );
        assert!(!common.join("hooks/pre-commit").exists());
    }

    /// A worktree made before its branch had a directory of its own is
    /// still restored, and gets no metadata grant.
    #[test]
    fn an_old_style_branch_is_restored_without_a_metadata_grant() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let session_id = Uuid::new_v4();
        let slug = short_slug(session_id);
        let path = repo.path().join(".horizon/worktrees").join(&slug);
        git(
            repo.path(),
            &[
                "worktree",
                "add",
                "-b",
                &format!("horizon/{slug}"),
                path.to_str().unwrap(),
            ],
        );
        let base = run_git(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        let identity = horizon_agent::contract::SessionWorktree {
            repository: repo.path().to_path_buf(),
            path: path.canonicalize().unwrap(),
            branch: format!("horizon/{slug}"),
            base,
        };
        let restored = restore_worktree(&identity, session_id).expect("restored");
        assert!(own_metadata_grants(&restored.path, session_id).is_empty());
    }

    /// `docs/agent-subagent-design.md` decision 11: a worktree holds work
    /// when its branch has a commit no other branch has, or changes not
    /// committed -- judged without its base, which an adopted worktree no
    /// longer knows.
    #[test]
    fn a_worktree_holds_work_once_it_commits_or_changes_something() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let session_id = Uuid::new_v4();
        let info = create_isolated_worktree(repo.path(), session_id).unwrap();
        let adopted = adopt_isolated_worktree(&info.path, session_id).unwrap();
        assert!(!holds_work(&adopted), "fresh from the requester's HEAD");

        std::fs::write(info.path.join("README.md"), "changed\n").unwrap();
        assert!(holds_work(&adopted), "a change not committed");

        git(&info.path, &["add", "README.md"]);
        git(
            &info.path,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                "x",
            ],
        );
        let adopted = adopt_isolated_worktree(&info.path, session_id).unwrap();
        assert!(holds_work(&adopted), "a commit only its branch has");
    }
}
