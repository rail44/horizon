//! Isolation/worktree creation at spawn -- `docs/session-relationship-design.md`
//! decisions 3 and 5. Sessiond-side (the design's pinned call): a spawn
//! requesting isolation gets its own `git worktree` under the target
//! repository's `.horizon/worktrees/<slug>`, branched per decision 3's base-ref
//! rule, and that worktree is removed (never the branch) on a clean terminate
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
pub(crate) struct WorktreeInfo {
    pub(crate) repo_root: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) branch: String,
}

/// Decision 3's base-ref rule, as a pure decision (no IO): whether the new
/// worktree is a lineage root (branch fresh from the repo's origin default)
/// or a derived child (branch from the spawn source's own worktree HEAD).
/// [`create_isolated_worktree`] turns this into an actual git ref/commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BaseRefStrategy {
    /// No owned parent worktree: this spawn is a lineage root.
    FreshFromOrigin,
    /// The spawn source itself owns a worktree: branch from *its* current
    /// HEAD, so a multi-level delegation chain includes the parent's own
    /// commits, not just origin's ("a child of an agent's worktree must
    /// branch from that worktree, not origin/main").
    SourceWorktreeHead,
}

pub(crate) fn base_ref_strategy(source_is_owned_worktree: bool) -> BaseRefStrategy {
    if source_is_owned_worktree {
        BaseRefStrategy::SourceWorktreeHead
    } else {
        BaseRefStrategy::FreshFromOrigin
    }
}

/// Resolves where a new isolated worktree should be created *from* and
/// which [`BaseRefStrategy`] applies -- the one pure function the design's
/// implementation notes ask for. `source` is `Some((dir, source_is_owned_
/// worktree))` when the spawn's source session is still live in `sessiond`
/// (see `SessiondState::session_directory`): `dir` is that session's own
/// worktree path if it owns one, else its plain `workspace_root`; `source_
/// is_owned_worktree` says which. `source` is `None` for an unknown/foreign
/// source id (a terminal isn't tracked here yet -- deferred, see the design
/// doc's "agents-first" note) or no source pane at all, in which case
/// `fallback` (the spawn's own `workspace_root`, or this process's cwd)
/// stands in and the spawn is treated as a lineage root.
pub(crate) fn resolve_isolation_source(
    source: Option<(PathBuf, bool)>,
    fallback: PathBuf,
) -> (PathBuf, bool) {
    match source {
        Some((dir, source_is_owned_worktree)) => (dir, source_is_owned_worktree),
        None => (fallback, false),
    }
}

/// The worktree directory name / branch suffix derived from a session id:
/// short enough to keep `.horizon/worktrees/<slug>` paths reasonable, long
/// enough that two live sessions colliding is practically a non-issue.
pub(crate) fn short_slug(session_id: Uuid) -> String {
    session_id.simple().to_string()[..8].to_string()
}

/// Strips every inherited `GIT_*` environment variable from `cmd`. Git
/// honors several of these (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
/// `GIT_COMMON_DIR`, ...) as overrides that take precedence over `-C`: an
/// absolute `GIT_DIR` in particular silently redirects an entire invocation
/// to a *different* repository, regardless of `-C`'s target directory.
/// Backlog 53: this repo's own `hooks/pre-commit` runs the full test suite,
/// and git exports exactly such an absolute `GIT_DIR` (pointing at
/// `$GIT_COMMON_DIR/worktrees/<name>`) to hook processes invoked from a
/// *linked* worktree -- which every session in this repo works from. That
/// env var then propagates through cargo -> nextest -> the test binary -> a
/// `git` subprocess spawned here, none of which sanitize it. Every git
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
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    scrub_git_env(&mut cmd);
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

fn repo_root_from_common_dir(common_dir: &Path) -> Result<PathBuf, String> {
    common_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("git common dir {} has no parent", common_dir.display()))
}

/// `origin/<default-branch>` resolved via `origin/HEAD`'s symbolic ref, the
/// same thing a normal `git clone` sets up automatically (this repo may not
/// have gone through a clone, e.g. a fresh scratch repo in a test, or one
/// where `git remote set-head origin -a` was never run). Falls back to the
/// local `HEAD` when there's no configured origin at all -- a sane default
/// for local-only development, and what keeps this hermetic in tests (no
/// network, see the module doc).
fn fresh_origin_ref(repo_root: &Path) -> String {
    match run_git(
        repo_root,
        &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"],
    ) {
        Ok(target) => target
            .strip_prefix("refs/remotes/")
            .map(str::to_string)
            .unwrap_or(target),
        Err(_) => "HEAD".to_string(),
    }
}

/// Best-effort: makes sure `.horizon` won't show up as untracked clutter in
/// the target repository's own `git status`, mirroring this repo's own
/// `/.horizon` `.gitignore` entry -- but via `.git/info/exclude` rather than
/// editing the target repo's tracked `.gitignore`, since sessiond has no
/// business committing a change to a file the repo's own history owns.
/// Also excludes `horizon_sandbox::SCRATCH_DIR_NAME` (the TMPDIR-parity
/// scratch directory a sandboxed bash tool provisions under its first
/// writable root -- for an isolated agent session that root is this very
/// worktree, so any leftover scratch file would otherwise show up as
/// untracked clutter too, and worse, make `remove_worktree_if_clean` refuse
/// to remove an otherwise-clean worktree). Never fails the worktree creation
/// itself: an ignore-file write is a nicety, not a correctness requirement
/// -- `remove_worktree_if_clean` pre-deletes the scratch dir itself before
/// removal, so a failed write here doesn't block cleanup.
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

    let scratch_pattern = format!("/{}/", horizon_sandbox::SCRATCH_DIR_NAME);
    if !content.lines().any(|line| line.trim() == scratch_pattern) {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&scratch_pattern);
        content.push('\n');
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

/// Creates a fresh isolated worktree for `session_id`, per decision 3:
/// discovers `source_dir`'s repository, resolves the base ref per
/// [`base_ref_strategy`], then `git worktree add -b horizon/<slug>
/// <repo_root>/.horizon/worktrees/<slug> <base_ref>`. Branch naming is
/// stable and session-derived (`horizon/<slug>`); the directory lives at
/// the repository root regardless of whether `source_dir` is itself a
/// linked worktree, so chained (multi-level) isolation never nests a
/// worktree inside another one.
pub(crate) fn create_isolated_worktree(
    source_dir: &Path,
    source_is_owned_worktree: bool,
    session_id: Uuid,
) -> Result<WorktreeInfo, String> {
    let common_dir = git_common_dir(source_dir)?;
    let repo_root = repo_root_from_common_dir(&common_dir)?;
    ensure_horizon_ignored(&common_dir);

    let base_ref = match base_ref_strategy(source_is_owned_worktree) {
        BaseRefStrategy::SourceWorktreeHead => run_git(source_dir, &["rev-parse", "HEAD"])?,
        BaseRefStrategy::FreshFromOrigin => fresh_origin_ref(&repo_root),
    };

    let slug = short_slug(session_id);
    let worktree_path = repo_root.join(".horizon").join("worktrees").join(&slug);
    let branch = format!("horizon/{slug}");
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
    })
}

/// Decision 5's terminate-cleanup rule: removes `info`'s worktree if it's
/// clean, keeps it (returns `false`) if it has any uncommitted or untracked
/// changes -- `git worktree remove` already refuses a dirty worktree on its
/// own (without `--force`), which is exactly "no uncommitted changes", so
/// this doesn't duplicate that check with a hand-rolled `git status` parse.
/// The branch itself is never deleted either way.
///
/// Before asking git, best-effort deletes `horizon_sandbox::SCRATCH_DIR_NAME`
/// under `info.path`: a sandboxed bash tool provisions that directory as its
/// TMPDIR-parity scratch space (see `horizon-sandbox::linux::spawn`), and for
/// an isolated agent session the worktree itself is the first writable root,
/// so any leftover scratch file makes `git worktree remove` measurably
/// refuse ("contains modified or untracked files") on an otherwise-clean
/// worktree. `ensure_horizon_ignored`'s exclude entry for this same
/// directory already fixes this the same way `/.horizon` does (an ignored
/// path doesn't count as untracked clutter to `remove`'s dirtiness check),
/// but that write is documented there as a nicety, not a correctness
/// requirement -- it can silently fail. This delete makes removal
/// deterministic regardless of whether that write landed; a genuinely dirty
/// worktree (real uncommitted/untracked files elsewhere) must still make git
/// refuse, so nothing else is touched and no `--force` is added.
pub(crate) fn remove_worktree_if_clean(info: &WorktreeInfo) -> bool {
    let Some(path_str) = info.path.to_str() else {
        return false;
    };
    let _ = std::fs::remove_dir_all(info.path.join(horizon_sandbox::SCRATCH_DIR_NAME));
    run_git(&info.repo_root, &["worktree", "remove", path_str]).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Pure logic ---------------------------------------------------

    #[test]
    fn resolve_isolation_source_with_no_source_uses_the_fallback_as_a_root() {
        let fallback = PathBuf::from("/tmp/fallback");
        assert_eq!(
            resolve_isolation_source(None, fallback.clone()),
            (fallback, false)
        );
    }

    #[test]
    fn resolve_isolation_source_uses_a_plain_source_dir_as_a_root() {
        let source_dir = PathBuf::from("/tmp/source");
        assert_eq!(
            resolve_isolation_source(
                Some((source_dir.clone(), false)),
                PathBuf::from("/tmp/fallback")
            ),
            (source_dir, false)
        );
    }

    #[test]
    fn resolve_isolation_source_marks_an_owned_source_worktree_as_derived() {
        let worktree_dir = PathBuf::from("/tmp/source-worktree");
        assert_eq!(
            resolve_isolation_source(
                Some((worktree_dir.clone(), true)),
                PathBuf::from("/tmp/fallback")
            ),
            (worktree_dir, true)
        );
    }

    #[test]
    fn base_ref_strategy_maps_owned_source_worktree_to_source_head() {
        assert_eq!(base_ref_strategy(true), BaseRefStrategy::SourceWorktreeHead);
        assert_eq!(base_ref_strategy(false), BaseRefStrategy::FreshFromOrigin);
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

        let info = create_isolated_worktree(repo.path(), false, session_id)
            .expect("worktree creation should succeed");

        let slug = short_slug(session_id);
        assert_eq!(info.branch, format!("horizon/{slug}"));
        assert_eq!(
            info.path,
            repo.path().join(".horizon").join("worktrees").join(&slug)
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
    fn create_isolated_worktree_falls_back_to_local_head_without_an_origin_remote() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        commit_file(repo.path(), "second.txt", "second\n", "second commit");

        let info = create_isolated_worktree(repo.path(), false, Uuid::new_v4())
            .expect("worktree creation should succeed");

        // The new worktree must include the *local* tip (no origin to
        // fall back to at all), proving the "HEAD" fallback actually ran.
        assert!(info.path.join("second.txt").is_file());
    }

    #[test]
    fn create_isolated_worktree_branches_fresh_from_the_origin_default_branch() {
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
        // A local-only commit past what origin has -- the new worktree must
        // NOT include this, proving it branched from origin, not local HEAD.
        commit_file(
            repo.path(),
            "local-only.txt",
            "local\n",
            "local-only commit",
        );

        let info = create_isolated_worktree(repo.path(), false, Uuid::new_v4())
            .expect("worktree creation should succeed");

        assert!(
            !info.path.join("local-only.txt").is_file(),
            "a root spawn must branch from origin's tip, not the local-only commit"
        );
        assert!(info.path.join("README.md").is_file());
    }

    #[test]
    fn create_isolated_worktree_branches_from_the_source_head_when_the_source_is_owned() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let parent_id = Uuid::new_v4();
        let parent = create_isolated_worktree(repo.path(), false, parent_id)
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

        let child = create_isolated_worktree(&parent.path, true, Uuid::new_v4())
            .expect("child worktree creation should succeed");

        assert!(child.path.join("parent-work.txt").is_file());
        assert_eq!(child.repo_root, parent.repo_root);
    }

    #[test]
    fn ensure_horizon_ignored_adds_the_exclude_line_exactly_once() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        create_isolated_worktree(repo.path(), false, Uuid::new_v4())
            .expect("first worktree creation should succeed");
        create_isolated_worktree(repo.path(), false, Uuid::new_v4())
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
    fn ensure_horizon_ignored_adds_the_scratch_dir_pattern_exactly_once() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        create_isolated_worktree(repo.path(), false, Uuid::new_v4())
            .expect("first worktree creation should succeed");
        create_isolated_worktree(repo.path(), false, Uuid::new_v4())
            .expect("second worktree creation should succeed");

        let exclude = std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap();
        let pattern = format!("/{}/", horizon_sandbox::SCRATCH_DIR_NAME);
        assert_eq!(
            exclude
                .lines()
                .filter(|line| line.trim() == pattern)
                .count(),
            1,
            "exclude file should list {pattern} exactly once, got:\n{exclude}"
        );
    }

    /// Empirically confirmed defect (fixed here): the TMPDIR-parity scratch
    /// dir a sandboxed bash tool provisions under a session's own worktree
    /// (`horizon_sandbox::SCRATCH_DIR_NAME`) previously made
    /// `remove_worktree_if_clean` permanently refuse to remove an otherwise
    /// clean worktree once anything was left inside it -- this test must
    /// fail against the unfixed code (before `remove_worktree_if_clean`
    /// pre-deleted the scratch dir).
    #[test]
    fn remove_worktree_if_clean_removes_a_worktree_with_only_scratch_dir_leftovers() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let info = create_isolated_worktree(repo.path(), false, Uuid::new_v4())
            .expect("worktree creation should succeed");
        let scratch_dir = info.path.join(horizon_sandbox::SCRATCH_DIR_NAME);
        std::fs::create_dir_all(&scratch_dir).unwrap();
        std::fs::write(scratch_dir.join("leftover.tmp"), "scratch\n").unwrap();

        assert!(
            remove_worktree_if_clean(&info),
            "a worktree dirtied only by sandbox scratch-dir leftovers should still be removed"
        );
        assert!(
            !info.path.exists(),
            "worktree should be removed despite the scratch-dir leftover"
        );
    }

    #[test]
    fn remove_worktree_if_clean_keeps_a_worktree_with_a_real_untracked_file_alongside_scratch() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let info = create_isolated_worktree(repo.path(), false, Uuid::new_v4())
            .expect("worktree creation should succeed");
        let scratch_dir = info.path.join(horizon_sandbox::SCRATCH_DIR_NAME);
        std::fs::create_dir_all(&scratch_dir).unwrap();
        std::fs::write(scratch_dir.join("leftover.tmp"), "scratch\n").unwrap();
        let real_file = info.path.join("real-untracked.txt");
        std::fs::write(&real_file, "real work\n").unwrap();

        assert!(
            !remove_worktree_if_clean(&info),
            "a genuinely dirty worktree must still refuse removal"
        );
        assert!(info.path.is_dir(), "dirty worktree must be kept");
        assert_eq!(
            std::fs::read_to_string(&real_file).unwrap(),
            "real work\n",
            "the real untracked file must not be touched by the scratch-dir pre-delete"
        );
    }

    #[test]
    fn remove_worktree_if_clean_removes_a_clean_worktree_but_keeps_the_branch() {
        let _canary = EnclosingRepoGuard::capture();
        let repo = scratch_repo();
        let info = create_isolated_worktree(repo.path(), false, Uuid::new_v4())
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
        let info = create_isolated_worktree(repo.path(), false, Uuid::new_v4())
            .expect("worktree creation should succeed");
        std::fs::write(info.path.join("untracked.txt"), "dirty\n").unwrap();

        assert!(!remove_worktree_if_clean(&info));
        assert!(info.path.is_dir(), "dirty worktree must be kept");
    }
}
