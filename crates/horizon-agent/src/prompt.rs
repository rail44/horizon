//! Provider-agnostic system prompt for Horizon agent sessions.
//!
//! Deliberately thin, per `docs/agent-tools-design.md`'s "System Prompt"
//! section: identity, an environment block, a few lines of tool policy, a
//! retry nudge, a destructive-action caution list, and — added 2026-07-07
//! after the prompting survey (`docs/research/agent-prompting.md` Part
//! 1.4) showed them near-universal even among thin prompts — short
//! communication and verification norms. The environment block is the only
//! part that varies per session. The norms are deliberately model-agnostic
//! (owner decision, 2026-07-07: Horizon expects to switch models, so
//! provider-specific prompt lore is out of scope).
//!
//! **Workflow prescriptions (owner decision, 2026-07-27).** This doc used
//! to say "no step-by-step workflow prescriptions — over-prescription
//! measurably harms newer models", model-agnostically (2026-07-07). That is
//! now narrowed: a workflow prescription is permitted once it has been
//! *measured* effective against Horizon's production models, and stays out
//! otherwise. [`DELEGATION_ROUTING_SECTION`] is the first and so far only
//! one to clear that bar (`docs/research/agent-delegation-and-batching-
//! probes-2026-07-27.md` cells C5 and C7b, run against both production
//! models). The model-agnostic stance survives the change: the shipped
//! wording is the one that worked for *both* models, and no per-model
//! branching was introduced.
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

/// The delegation-routing block: how a session that *has* the `task` tool
/// (`tools::explore`) is told to use it. Not part of the base prompt --
/// `providers::rig::session::session_extra_sections` appends it only for a
/// session whose advertised toolset actually contains `task`, so an
/// exploration session (whose role allows `fs.read`/`fs.grep`/`fs.glob`
/// and nothing else) is never told to make a call it cannot make.
///
/// Both clauses are the measured winners, transplanted verbatim from
/// `docs/research/agent-delegation-and-batching-probes-2026-07-27.md`: the
/// exploration-entry clause is cell C5 and the implementation-entry clause
/// is cell C7b, the only interventions in that probe that closed delegation
/// adoption for *both* production models. Two properties are load-bearing
/// and must not be softened without re-measuring: the ban on orienting
/// first is explicit (imperative tone alone, negative routing on `bash`,
/// and renaming the tool were each measured insufficient), and the
/// implementation clause is unconditional -- the earlier conditional
/// wording ("in an unfamiliar area", cell C7) was measurably used as an
/// escape hatch. The `when it is available` hedge the old one-line policy
/// carried is deliberately gone too: conditionality lives in *whether this
/// section is included*, not in its wording.
///
/// Three later amendments. The first two are confined to the clause's
/// **final, report-return sentence**, so every measured word from cells
/// C3/C5/C7b stays byte-identical and the probe's result is not re-opened;
/// the third is the first one to also touch the implementation clause's
/// opening sentence:
///
/// 1. **2026-07-27.** The implementation clause used to end "Start
///    implementing only after its report returns", which a dogfooded
///    session read as license to delegate the implementation itself to task
///    children. Task agents are read-only by construction, so such a child
///    can only explore again until its budget runs out. The replacement
///    stated where implementation happens and that delegating it is not an
///    option.
/// 2. **2026-07-28.** `task` became asynchronous
///    (`docs/agent-async-task-design.md`), so "only after its report
///    returns" stopped describing reality at all: nothing returns a report
///    to wait on. The sentence now states the async contract — keep
///    working, the report arrives as a notification — while keeping the
///    read-only constraint the previous amendment added. This is the second
///    deliberate amendment to the measured clause, and the sentence it
///    touches is the same one; the two orienting bans above it are
///    untouched.
/// 3. **2026-07-28, later the same day.** The first validation run of the
///    asynchronous loop (session 5dd49a85, recorded in
///    `docs/agent-async-task-design.md`'s measurement plan) drove the whole
///    mechanism correctly but launched exactly one monolithic task, so
///    consumption kept the synchronous era's shape. The clause's singular
///    phrasing ("the up-front investigation and planning") is what selects
///    that shape, and entry-point prescriptions are the part these models
///    follow reliably (cells C5/C7b) — so the decomposition is stated in
///    the entry sentence itself: split the investigation into independent,
///    narrowly scoped questions and launch them as parallel calls in one
///    response. The same amendment appends a closing sentence prescribing
///    *continued* delegation of narrow follow-up questions during
///    implementation. That closing sentence is a new prescription, not a
///    measured one: it is to be measured on the next dogfood run, and comes
///    back out if delegation granularity does not move.
///
/// `tools::catalog`'s `task` description carries the matching amendments.
pub const DELEGATION_ROUTING_SECTION: &str = "Delegation:\n\
     - For an open-ended exploration goal, calling task must be your FIRST action — do not run \
     bash/ls or read files to orient yourself first; the task agent does its own orientation \
     inside its own session, which costs you nothing.\n\
     - For any implementation task, your FIRST action must be to delegate the up-front \
     investigation and planning to the task tool — split it into independent, narrowly scoped \
     questions and launch them as parallel task calls in one response (up to 3 run concurrently) \
     — even when the change targets look already known or the task statement names concrete \
     components. State the question and the exact deliverable (relevant files, line numbers, a \
     step plan). Do not grep/read/bash to orient yourself first. Keep working while it runs; its \
     report will arrive as a notification, and you implement the changes yourself in this \
     session — task agents cannot write files; never delegate the implementation itself. While \
     implementing, keep delegating narrow follow-up questions (where is something defined, how \
     is it used) to task instead of reading through the code yourself — results arrive as \
     notifications while you keep working.";

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
         - After modifying code or files, verify the change with an appropriate build or test \
         before reporting it done, and say what you checked. Read-only investigation does not \
         require a build or test unless it is needed to validate a specific claim.\n\
         \n\
         Environment:\n\
         - Working directory: {cwd}\n\
         - OS: {os}\n\
         - Git repository: {git_repo}\n\
         \n\
         Tool policy:\n\
         - Tools require absolute paths; relative paths are rejected.\n\
         - Prefer targeted reads and searches (grep, glob, line-windowed reads) over dumping whole files.\n\
         - Use $TMPDIR for temporary files so sandboxed calls do not rely on literal /tmp being writable.\n\
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
