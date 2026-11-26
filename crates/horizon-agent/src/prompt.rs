//! Provider-agnostic system prompt for Horizon agent sessions.
//!
//! Deliberately thin, per `docs/agent-tools-design.md`'s "System Prompt"
//! section: identity, an environment block, a few lines of tool policy, a
//! destructive-action caution list, and — added 2026-07-07 after the
//! prompting survey (`docs/research/agent-prompting.md` Part 1.4) showed
//! them near-universal even among thin prompts — short communication
//! norms. The environment block is the only part that varies per session.
//! The norms are deliberately model-agnostic (Horizon expects to switch
//! models, so provider-specific prompt lore is out of scope).
//!
//! **Workflow prescriptions.** This doc used
//! to say "no step-by-step workflow prescriptions — over-prescription
//! measurably harms newer models", model-agnostically (2026-07-07). That is
//! now narrowed: a workflow prescription is permitted once it has been
//! *measured* effective against Horizon's production models, and stays out
//! otherwise.
//!
//! [`system_prompt`]'s `extra_sections` parameter is a back-compatible
//! injection point (`docs/research/agent-prompting.md` Part 2.5): an empty
//! slice reproduces the thin prompt above byte-for-byte, while each
//! passed-in section is appended verbatim, in order. `instructions::
//! extra_sections` is its first consumer (repository instruction files,
//! see that module); `providers::rig::session::session_extra_sections`
//! composes the role section, the skills listing, and
//! [`DELEGATION_ROUTING_SECTION`] through the same seam.

use std::path::{Path, PathBuf};

/// Facts about the session's environment, gathered once when the session
/// starts (cheap: a `current_dir` call, a `consts::OS` read, a bounded walk
/// up the directory tree for `.git`, the clock, and one `git` call for the
/// branch). Provider-agnostic — any provider driving a Horizon agent
/// session can build its system prompt from this.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionEnvironment {
    pub cwd: PathBuf,
    pub os: &'static str,
    pub git_repo: bool,
    /// The date the session started, `YYYY-MM-DD` in UTC.
    pub date: String,
    /// The branch checked out at the start, when one is.
    pub git_branch: Option<String>,
}

impl SessionEnvironment {
    /// Gathers environment facts for a session whose real working directory
    /// is `workspace_root` -- an isolated session's isolated worktree, or a
    /// plain shared directory, already resolved by the caller
    /// (`contract::StartSession::workspace_root`, threaded in by
    /// `horizon-agent-host`'s `session::run_session`). Falls back to this
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
        let git_branch = git_repo.then(|| current_branch(&cwd)).flatten();
        Self {
            cwd,
            os: std::env::consts::OS,
            git_repo,
            date: utc_date(std::time::SystemTime::now()),
            git_branch,
        }
    }
}

/// The branch `cwd`'s checkout has, by `git symbolic-ref`: `None` when
/// detached, or when `git` cannot say.
fn current_branch(cwd: &Path) -> Option<String> {
    let mut command = std::process::Command::new("git");
    // A parent Git process can point these elsewhere than `cwd`'s repository.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(key);
    }
    let output = command
        .arg("-C")
        .arg(cwd)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let branch = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (output.status.success() && !branch.is_empty()).then_some(branch)
}

/// `at` as a UTC calendar date, `YYYY-MM-DD` (the proleptic Gregorian
/// calendar, from the days since the Unix epoch).
pub(crate) fn utc_date(at: std::time::SystemTime) -> String {
    let days = at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    // Howard Hinnant's days-to-civil: eras of 400 years from 0000-03-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Walks up from `cwd` looking for a `.git` entry (a directory in a normal
/// checkout, a file in a worktree). Bounded by the filesystem's depth, so
/// it's cheap enough to run once at session start.
fn is_git_repository(cwd: &Path) -> bool {
    cwd.ancestors().any(|dir| dir.join(".git").exists())
}

/// The delegation-routing block: how a session that *has* the `task` tool
/// (`tools::explore`) is told to use it. Not part of the base prompt --
/// `providers::rig::session::session_extra_sections` appends it only for a
/// session whose advertised toolset actually contains `task`, so an
/// exploration session (whose role allows `fs_read`/`fs_grep`/`fs_glob`
/// and nothing else) is never told to make a call it cannot make.
///
/// `tools::catalog`'s `task` description states the same constraints.
pub const DELEGATION_ROUTING_SECTION: &str = "Delegation:\n\
     - `task` runs a piece of work in a parallel session and reports back, keeping the \
     intermediate work out of your context.\n\
     - Read tasks (the default) investigate: use them for open-ended exploration or a multi-file \
     search; read one to three known files yourself.\n\
     - Write tasks do a self-contained change on a branch of their own; you review and merge it.";

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
         \n\
         Environment:\n\
         - Working directory at session start: {cwd}\n\
         - Date at session start: {date}\n\
         - OS: {os}\n\
         - Git repository: {git_repo}\n\
         \n\
         Tool policy:\n\
         - File tools take a path that is absolute or relative to the workspace root, where \
         bash commands start too.\n\
         - Prefer targeted reads and searches (grep, glob, line-windowed reads) over dumping whole files.\n\
         - Use $TMPDIR for temporary files: it is kept from one call to the next for the session.\n\
         \n\
         Treat these as destructive — confirm they match what the user asked before doing them:\n\
         - Overwriting or deleting files or data.\n\
         - Force-pushing, resetting, or discarding uncommitted changes.\n\
         - Dropping, truncating, or migrating a database.\n\
         - Any command whose effects reach outside the current workspace.",
        cwd = environment.cwd.display(),
        os = environment.os,
        date = environment.date,
        git_repo = match (environment.git_repo, &environment.git_branch) {
            (true, Some(branch)) => format!("yes, on branch {branch}"),
            (true, None) => "yes".to_string(),
            (false, _) => "no".to_string(),
        },
    );
    for section in extra_sections {
        prompt.push_str("\n\n");
        prompt.push_str(section);
    }
    prompt
}
