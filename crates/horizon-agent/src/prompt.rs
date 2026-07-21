//! Provider-agnostic system prompt for Horizon agent sessions.
//!
//! Deliberately thin, per `docs/agent-tools-design.md`'s "System Prompt"
//! section: identity, an environment block, a few lines of tool policy, a
//! retry nudge, a destructive-action caution list, and — added 2026-07-07
//! after the prompting survey (`docs/research/agent-prompting.md` Part
//! 1.4) showed them near-universal even among thin prompts — short
//! communication and verification norms. No step-by-step workflow
//! prescriptions — over-prescription measurably harms newer models, and
//! the environment block is the only part that varies per session. The
//! norms are deliberately model-agnostic (owner decision, 2026-07-07:
//! Horizon expects to switch models, so provider-specific prompt lore is
//! out of scope).
//!
//! [`system_prompt`]'s `extra_sections` parameter is a back-compatible
//! injection point (`docs/research/agent-prompting.md` Part 2.5): an empty
//! slice reproduces the thin prompt above byte-for-byte, while each
//! passed-in section is appended verbatim, in order. `instructions::
//! extra_sections` is its first consumer (repository instruction files,
//! see that module); a future role/skill mechanism can reuse the same seam
//! without another signature change.

use std::path::{Path, PathBuf};

/// Facts about the session's environment, gathered once when the session
/// starts (cheap: a `current_dir` call, a `consts::OS` read, and a bounded
/// walk up the directory tree for `.git`). Provider-agnostic — any provider
/// driving a Horizon agent session can build its system prompt from this.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionEnvironment {
    pub cwd: PathBuf,
    pub os: &'static str,
    pub git_repo: bool,
}

impl SessionEnvironment {
    /// Gathers environment facts for a session whose real working directory
    /// is `workspace_root` -- an isolated session's isolated worktree, or a
    /// plain shared directory, already resolved by the caller
    /// (`contract::StartSession::workspace_root`, threaded in by
    /// `horizon-sessiond`'s `session::run_session`). Falls back to this
    /// process's own `cwd` only when `workspace_root` is `None` (a session
    /// with no workspace concept at all), and to `/` if even that can't be
    /// read (rare -- e.g. the directory was removed out from under the
    /// process). Using anything other than the session's own root here was
    /// exactly the 2026-07-19 dogfooding bug: an isolated session's system
    /// prompt claimed the daemon's own cwd (the root repository checkout)
    /// as its working directory, so the model tried to write files there
    /// instead of into its actual isolated worktree.
    pub fn for_workspace_root(workspace_root: Option<&Path>) -> Self {
        let cwd = workspace_root
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
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
/// "preamble") from session environment facts, followed by any
/// `extra_sections` appended verbatim (each separated by a blank line) — see
/// the module doc. Passing an empty slice reproduces the base prompt
/// unchanged, so every existing call site keeps its exact current behavior.
pub fn system_prompt(environment: &SessionEnvironment, extra_sections: &[String]) -> String {
    let mut prompt = format!(
        "You are the Horizon agent, a coding assistant embedded in the Horizon desktop shell.\n\
         Answer directly; use tools only when they add information you don't already have.\n\
         Your session outlives this conversation window: it survives application restarts, and \
         its full history is retained beyond what you can currently see.\n\
         \n\
         Communication:\n\
         - Be concise; don't restate what the transcript already shows.\n\
         - Report outcomes faithfully: state failures and partial results plainly rather than \
         presenting them as success.\n\
         - Before reporting work as done, verify it — build or test the change — and \
         say what you checked.\n\
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
    );
    for section in extra_sections {
        prompt.push_str("\n\n");
        prompt.push_str(section);
    }
    prompt
}
