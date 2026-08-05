//! Path resolution for the board event log.
//!
//! The canonical store is `<data-home>/horizon/board/<sanitized-root>/events.jsonl`,
//! where `<sanitized-root>` is the main git root's absolute path with every
//! `/` replaced by `-` (matching the knowledge layer's convention in
//! `crates/horizon-agent/src/knowledge.rs`). Linked worktrees share one
//! store because the main root — not the worktree root — is sanitised.

use std::path::{Path, PathBuf};

/// `$XDG_DATA_HOME` (if non-empty) → `~/.local/share` (if `$HOME`
/// non-empty) → `std::env::temp_dir()`.  Mirrors
/// `horizon-agent::config::agent_data_home_from` so this crate stays
/// independent of `horizon-agent`.
fn data_home() -> PathBuf {
    let non_empty = |value: Option<String>| value.filter(|v| !v.is_empty());
    match non_empty(std::env::var("XDG_DATA_HOME").ok()) {
        Some(dir) => PathBuf::from(dir),
        None => match non_empty(std::env::var("HOME").ok()) {
            Some(home) => PathBuf::from(home).join(".local").join("share"),
            None => std::env::temp_dir(),
        },
    }
}

/// Sanitises a project root's absolute path into a directory-safe segment:
/// every `/` (including the leading one) becomes `-`. E.g.
/// `/home/user/src/project` → `-home-user-src-project`.
fn sanitize_root(root: &Path) -> String {
    root.to_string_lossy().replace('/', "-")
}

/// Resolves the *main* git root for `dir` via
/// `git rev-parse --git-common-dir` (so linked worktrees map to the same
/// store). Returns `None` when not inside a git repo.
///
/// `GIT_*` env vars (except `GIT_CEILING_DIRECTORIES`) are scrubbed so a
/// worktree's `GIT_DIR` doesn't redirect the command to the worktree's own
/// `.git` path — same hazard and same fix as the knowledge layer.
pub fn main_root(dir: &Path) -> Option<PathBuf> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    scrub_git_env(&mut cmd);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let common_dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let repo_root = common_dir.parent()?;
    std::fs::canonicalize(repo_root).ok()
}

/// Strips inherited `GIT_*` environment variables (except
/// `GIT_CEILING_DIRECTORIES`) so `git rev-parse` isn't misled by a
/// worktree-session's `GIT_DIR`/`GIT_WORK_TREE`.
fn scrub_git_env(cmd: &mut std::process::Command) {
    cmd.env_clear();
    // Re-export a minimal environment so git can still find its config.
    for key in ["PATH", "HOME", "XDG_CONFIG_HOME", "GIT_CEILING_DIRECTORIES"] {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
}

/// Computes the events.jsonl path for the project whose main root is `root`.
pub fn events_path(root: &Path) -> PathBuf {
    data_home()
        .join("horizon")
        .join("board")
        .join(sanitize_root(root))
        .join("events.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_slashes() {
        let p = sanitize_root(Path::new("/home/user/src/project"));
        assert_eq!(p, "-home-user-src-project");
    }

    #[test]
    fn events_path_composition() {
        // We can't test data_home() without env manipulation, but we can
        // verify the suffix composition given a known data_home.
        let root = Path::new("/home/user/src/project");
        let suffix = PathBuf::from("horizon")
            .join("board")
            .join("-home-user-src-project")
            .join("events.jsonl");
        // The actual path is data_home().join(suffix) — just verify the
        // suffix is correct shape.
        let full = events_path(root);
        assert!(full.ends_with(&suffix));
    }
}
