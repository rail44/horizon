//! File checkpoints (`docs/agent-core-extension-design.md`, "ファイル"): a
//! session's work tree recorded as a commit at each settled position it
//! leaves, so a branch from that position can have the files it had.
//!
//! A checkpoint is built in an index of its own (`GIT_INDEX_FILE`) and kept
//! under `refs/horizon/checkpoints/<session>/<record>`, where `<record>` is
//! the event-log record the position follows. Recording touches no branch,
//! no index of the repository's and no file. What `.gitignore` leaves out
//! is not recorded, and a directory outside a git work tree has none.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::worktree::run_git_in;

const PREFIX: &str = "refs/horizon/checkpoints";

/// The identity a checkpoint commit is written with: the repository's own
/// configuration may have none.
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Horizon"),
    ("GIT_AUTHOR_EMAIL", "horizon@localhost"),
    ("GIT_COMMITTER_NAME", "Horizon"),
    ("GIT_COMMITTER_EMAIL", "horizon@localhost"),
];

fn ref_name(session_id: Uuid, record: &str) -> String {
    format!("{PREFIX}/{session_id}/{record}")
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    run_git_in(dir, args, &[])
}

/// Records `dir`'s work tree, as `HEAD` with every change and every file
/// not ignored on top, as the checkpoint of `session_id` at the position
/// after `record`. `Ok(None)` when `dir` is not in a git work tree.
pub(crate) fn record(dir: &Path, session_id: Uuid, record: &str) -> Result<Option<String>, String> {
    if git(dir, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Ok("true") {
        return Ok(None);
    }
    let toplevel = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?);
    let git_dir = PathBuf::from(git(dir, &["rev-parse", "--absolute-git-dir"])?);
    let index = git_dir.join(format!("horizon-checkpoint-{}.index", Uuid::new_v4()));
    // Starting from the repository's index keeps `add -A` from hashing
    // every file again.
    let _ = std::fs::copy(git_dir.join("index"), &index);
    let built = (|| {
        let env = [(
            "GIT_INDEX_FILE",
            index.as_os_str().to_str().ok_or("index path")?,
        )];
        run_git_in(&toplevel, &["add", "-A"], &env)?;
        run_git_in(&toplevel, &["write-tree"], &env)
    })();
    let _ = std::fs::remove_file(&index);
    let tree = built?;
    let parent = git(&toplevel, &["rev-parse", "--verify", "-q", "HEAD^{commit}"]).ok();
    let message = format!("Horizon checkpoint of session {session_id} after {record}");
    let mut args = vec!["commit-tree", tree.as_str(), "-m", message.as_str()];
    if let Some(parent) = &parent {
        args.extend(["-p", parent.as_str()]);
    }
    let commit = run_git_in(&toplevel, &args, &IDENTITY)?;
    git(
        &toplevel,
        &["update-ref", &ref_name(session_id, record), &commit],
    )?;
    Ok(Some(commit))
}

/// The checkpoint of `session_id` at the position after `record`, in
/// `dir`'s repository.
pub(crate) fn find(dir: &Path, session_id: Uuid, record: &str) -> Option<String> {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("{}^{{commit}}", ref_name(session_id, record)),
        ],
    )
    .ok()
}

/// Puts the files of `checkpoint` into the work tree `dir`: afterwards the
/// work tree holds the checkpoint's tree, the index matches `HEAD` again,
/// and `HEAD` and its branch are where they were. A file no checkpoint or
/// index knows (untracked and new since) is left where it is.
pub(crate) fn restore(dir: &Path, checkpoint: &str) -> Result<(), String> {
    git(dir, &["read-tree", "-u", "--reset", checkpoint])?;
    git(dir, &["reset", "-q"])?;
    Ok(())
}

/// Removes every checkpoint of `session_id` from `dir`'s repository; none
/// to remove outside one.
pub(crate) fn remove_all(dir: &Path, session_id: Uuid) -> Result<(), String> {
    if git(dir, &["rev-parse", "--git-dir"]).is_err() {
        return Ok(());
    }
    let refs = git(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("{PREFIX}/{session_id}/"),
        ],
    )?;
    for name in refs.lines().filter(|name| !name.is_empty()) {
        git(dir, &["update-ref", "-d", name])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
