//! Opening a log-backed store: resolving the project's event file from a
//! directory, and pairing it with the logd socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::store::source::{LogSource, Source};
use crate::store::types::StoreError;
use crate::store::Store;

impl Store {
    /// Resolves the store from the current directory's main git root.
    pub fn from_cwd() -> Result<Self, StoreError> {
        let cwd = std::env::current_dir()?;
        Self::from_dir(&cwd)
    }

    /// Resolves the store from an explicit directory inside a git repo --
    /// the main git root is resolved from `dir` (so a linked worktree maps
    /// to the same store as the main checkout), exactly as `from_cwd` does
    /// but without depending on the process cwd. The GUI shell uses this so
    /// a board modal reading the active session's `workspace_root` reads the
    /// same store the board CLI (`from_cwd`) reads.
    pub fn from_dir(dir: &Path) -> Result<Self, StoreError> {
        let root = crate::path::main_root(dir).ok_or(StoreError::NotInGitRepo)?;
        Ok(Self::at(crate::path::events_path(&root)))
    }

    /// Opens a store at an explicit path (for testing).
    pub fn at(path: PathBuf) -> Self {
        Self::at_with_socket(path, horizon_wire::socket::default_logd_socket_path())
    }

    /// Opens a store at an explicit path with an explicit logd socket (for
    /// tests that spawn logd on an isolated socket).
    pub fn at_with_socket(path: PathBuf, logd_socket: PathBuf) -> Self {
        Self {
            source: Source::Log(Arc::new(LogSource {
                events: path,
                socket: logd_socket,
            })),
        }
    }
}
