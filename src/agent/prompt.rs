//! Provider-agnostic system prompt for Horizon agent sessions.
//!
//! Deliberately thin, per `docs/agent-tools-design.md`'s "System Prompt"
//! section: identity, an environment block, a few lines of tool policy, a
//! retry nudge, and a destructive-action caution list. No step-by-step
//! workflow prescriptions — over-prescription measurably harms newer
//! models, and the environment block is the only part that varies per
//! session.

use std::path::{Path, PathBuf};

/// Facts about the session's environment, gathered once when the session
/// starts (cheap: a `current_dir` call, a `consts::OS` read, and a bounded
/// walk up the directory tree for `.git`). Provider-agnostic — any provider
/// driving a Horizon agent session can build its system prompt from this.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionEnvironment {
    pub(crate) cwd: PathBuf,
    pub(crate) os: &'static str,
    pub(crate) git_repo: bool,
}

impl SessionEnvironment {
    /// Gathers environment facts for the current process. Falls back to
    /// `/` for `cwd` if it cannot be read (rare — e.g. the directory was
    /// removed out from under the process).
    pub(crate) fn current() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let git_repo = is_git_repository(&cwd);
        Self {
            cwd,
            os: std::env::consts::OS,
            git_repo,
        }
    }
}

/// Walks up from `cwd` looking for a `.git` entry (a directory in a normal
/// checkout, a file in a worktree). Bounded by the filesystem's depth, so
/// it's cheap enough to run once at session start.
fn is_git_repository(cwd: &Path) -> bool {
    cwd.ancestors().any(|dir| dir.join(".git").exists())
}

/// Builds the system prompt (rig calls this the completion request's
/// "preamble") from session environment facts.
pub(crate) fn system_prompt(environment: &SessionEnvironment) -> String {
    format!(
        "You are the Horizon agent, a coding assistant embedded in the Horizon desktop shell.\n\
         Answer directly; use tools only when they add information you don't already have.\n\
         \n\
         Environment:\n\
         - Working directory: {cwd}\n\
         - OS: {os}\n\
         - Git repository: {git_repo}\n\
         \n\
         Tool policy:\n\
         - Tools require absolute paths; relative paths are rejected.\n\
         - Prefer targeted reads and searches (grep, glob, line-windowed reads) over dumping whole files.\n\
         - If a tool call fails, read the error and retry with adjusted input rather than giving up.\n\
         \n\
         Treat these as destructive — confirm they match what the user asked before doing them:\n\
         - Overwriting or deleting files or data.\n\
         - Force-pushing, resetting, or discarding uncommitted changes.\n\
         - Dropping, truncating, or migrating a database.\n\
         - Any command whose effects reach outside the current workspace.",
        cwd = environment.cwd.display(),
        os = environment.os,
        git_repo = if environment.git_repo { "yes" } else { "no" },
    )
}
