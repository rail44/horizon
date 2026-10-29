//! The rows of the `git_history` extension's table function, computed with
//! gitoxide (`gix`): [`tracked_files`] is the worktree's current files (`git
//! ls-files`). The rows follow gix's behavior; `duckdb-ext/README.md` lists
//! where that differs from git.

pub use gix::Repository;

mod tracked;
pub use tracked::{TrackedFileRow, tracked_files};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// Open the repository containing `path` (any directory inside a worktree,
/// or a bare repository).
pub fn open(path: &str) -> Result<Repository> {
    let mut repo = gix::discover(path)?;
    repo.object_cache_size_if_unset(64 * 1024 * 1024);
    Ok(repo)
}
