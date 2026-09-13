//! Git operations owned by the coordinator. Verification runs in contained
//! agent sessions; only a checked candidate may update the main checkout.

use horizon_board::workflow::{Integration, Worker};
use std::path::{Path, PathBuf};

pub(super) fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    for (key, _) in std::env::vars() {
        if key.starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    let result = cmd.output().map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().into())
}

pub(super) fn main_checkout(root: &Path) -> Result<PathBuf, String> {
    let listing = run(root, &["worktree", "list", "--porcelain"])?;
    for record in listing.split("\n\n") {
        if record.lines().any(|l| l == "branch refs/heads/main") {
            if let Some(path) = record.lines().find_map(|l| l.strip_prefix("worktree ")) {
                return Ok(PathBuf::from(path));
            }
        }
    }
    Err("No checkout of main is available for automatic integration".into())
}

pub(super) fn clean_head(worker: &Worker) -> Result<String, String> {
    let dir = Path::new(&worker.worktree);
    if run(dir, &["branch", "--show-current"])? != worker.branch {
        return Err("Task worktree changed branches".into());
    }
    if !run(dir, &["status", "--porcelain"])?.is_empty() {
        return Err(
            "Commit the complete task and leave the worktree clean before reporting".into(),
        );
    }
    run(dir, &["rev-parse", "HEAD"])
}

pub(super) fn prepare(root: &Path, worker: &Worker) -> Result<Integration, String> {
    clean_head(worker)?;
    let base = run(root, &["rev-parse", "refs/heads/main"])?;
    run(Path::new(&worker.worktree), &["merge", "--no-edit", &base])?;
    let head = clean_head(worker)?;
    Ok(Integration { base, head })
}

pub(super) fn check_scope(
    root: &Path,
    worker: &Worker,
    scope: &horizon_board::workflow::ChangeScope,
) -> Result<(), String> {
    let base = run(root, &["merge-base", "refs/heads/main", &worker.branch])?;
    let changed = run(
        Path::new(&worker.worktree),
        &["diff", "--name-only", "--no-renames", "-z", &base, "HEAD"],
    )?;
    for path in changed.split('\0').filter(|s| !s.is_empty()) {
        if !scope.paths.iter().any(|p| {
            p == "*"
                || p.trim_end_matches('/') == path
                || path.starts_with(&format!("{}/", p.trim_end_matches('/')))
        }) {
            return Err(format!("Task changed {path} outside its planned source scope; replan its scope and conflicts before continuing"));
        }
    }
    Ok(())
}

pub(super) enum MergeOutcome {
    Integrated,
    Stale,
}

pub(super) fn integrate(
    root: &Path,
    worker: &Worker,
    candidate: &Integration,
) -> Result<MergeOutcome, String> {
    let main = main_checkout(root)?;
    // Recovery after Git succeeded but durable board persistence did not.
    if run(
        &main,
        &[
            "merge-base",
            "--is-ancestor",
            &candidate.head,
            "refs/heads/main",
        ],
    )
    .is_ok()
    {
        return Ok(MergeOutcome::Integrated);
    }
    if run(&main, &["rev-parse", "refs/heads/main"])? != candidate.base {
        return Ok(MergeOutcome::Stale);
    }
    if clean_head(worker)? != candidate.head {
        return Err("Candidate changed after verification".into());
    }
    if run(&main, &["symbolic-ref", "--short", "HEAD"])? != "main" {
        return Err("Integration checkout changed branches".into());
    }
    if !run(&main, &["status", "--porcelain"])?.is_empty() {
        return Err("Main has local changes; preserve them before integration can continue".into());
    }
    // A concurrent main update that is not an ancestor makes ff-only fail.
    // Git's own index/ref locks protect the final update; no reset or force.
    run(&main, &["merge", "--ff-only", "--no-edit", &candidate.head])?;
    Ok(MergeOutcome::Integrated)
}

#[cfg(test)]
mod tests;
