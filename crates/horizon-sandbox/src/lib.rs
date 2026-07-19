//! Sandbox spike for the agent approval trust model
//! (`docs/agent-approval-design.md`, "Sandbox architecture"): a thin,
//! product-owned unified API over per-OS process containment.
//!
//! ```no_run
//! use horizon_sandbox::{NetworkPolicy, ReadableScope, SandboxPolicy, SandboxStdio};
//! use std::process::Command;
//!
//! let policy = SandboxPolicy {
//!     writable_roots: vec!["/tmp/some-session-worktree".into()],
//!     readable_scope: ReadableScope::Full,
//!     network: NetworkPolicy::Disabled,
//! };
//! let mut command = Command::new("bash");
//! command.arg("-c").arg("echo hi");
//! let sandboxed = horizon_sandbox::spawn(command, &policy, SandboxStdio::piped_output());
//! ```
//!
//! Platform support is Linux and macOS; anything else is a typed
//! [`SandboxError::UnsupportedPlatform`]. This crate started as a prototype
//! (see the crate-level `Cargo.toml` doc comment and the roadmap item that
//! dispatched the spike) and is now wired into `horizon-agent`'s bash tool
//! for the policy-tiers leg (`docs/agent-approval-design.md`). The Linux
//! backend migrated from a self-built bwrap+seccompiler+landlock stack to
//! depend on `nono` (Landlock-based capability sandboxing) on
//! 2026-07-19 (`docs/roadmap.md`'s backlog-60 entry); macOS still uses
//! `sandbox-exec`+SBPL.
//!
//! ## Stdio: the caller must state it explicitly
//!
//! `spawn` rebuilds a fresh `Command` around the caller's `command`
//! (on macOS, `sandbox-exec -f <profile> -- <command>`; on Linux the
//! program/args are run directly, with nono's capabilities applied to the
//! spawning thread beforehand -- see `linux::spawn`), copying over the
//! program, arguments, working directory, and explicit environment
//! overrides -- all things `std::process::Command` exposes getters for
//! (`get_program`/`get_args`/`get_current_dir`/`get_envs`). It cannot
//! also copy whatever `stdin`/`stdout`/`stderr` the caller configured on
//! `command` itself, because `Command` provides no getter for that at all
//! (write-only API) -- so `spawn` takes a separate [`SandboxStdio`]
//! parameter instead of trying to infer it.

mod denial;
mod error;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod policy;

pub use denial::is_likely_sandbox_denied;
pub use error::SandboxError;
pub use policy::{NetworkPolicy, ReadableScope, SandboxPolicy, SandboxStdio};

#[cfg(target_os = "linux")]
pub use linux::NonoReport;

use std::process::{Child, Command};

/// Name of the TMPDIR-parity scratch directory the Linux backend provisions
/// under a sandboxed command's first writable root (see `linux::spawn`'s
/// TMPDIR comment) -- e.g. `<root>/.horizon-sandbox-tmp`. Exposed so callers
/// that manage the writable root's lifecycle themselves (e.g.
/// `horizon-sessiond`'s isolated-worktree cleanup) can special-case this
/// specific directory without duplicating the literal.
pub const SCRATCH_DIR_NAME: &str = ".horizon-sandbox-tmp";

/// A spawned sandboxed process, plus whatever per-backend containment
/// report is available.
///
/// `nono` is only ever `Some` on Linux; other backends carry no equivalent
/// (macOS's `sandbox-exec` has no comparable negotiated-ABI concept).
/// Unlike the old backend's `LandlockReport` (a diagnostic negotiated on a
/// throwaway thread, decoupled from what actually protected the spawned
/// bwrap child -- Landlock and bwrap could not share a thread), this
/// report reflects the containment that is genuinely live around `child`:
/// nono's `Sandbox::apply_auto` *is* what restricts the thread that then
/// spawns it.
pub struct SandboxedChild {
    pub child: Child,
    #[cfg(target_os = "linux")]
    pub nono: Option<NonoReport>,
}

/// Prepares `command` to run under `policy` and spawns it, dispatching to
/// the current OS's backend. `command`'s program, arguments, working
/// directory, and explicit environment overrides are preserved; the
/// backend rebuilds the actual spawn around them (e.g. as
/// `bwrap <containment args> -- <command>` on Linux). `stdio` is applied to
/// the rebuilt command explicitly -- see the crate root doc's "Stdio"
/// section for why `spawn` can't infer it from `command` itself.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn spawn(
    command: Command,
    policy: &SandboxPolicy,
    stdio: SandboxStdio,
) -> Result<SandboxedChild, SandboxError> {
    #[cfg(target_os = "linux")]
    {
        linux::spawn(command, policy, stdio)
    }
    #[cfg(target_os = "macos")]
    {
        macos::spawn(command, policy, stdio)
    }
}

/// Always returns [`SandboxError::UnsupportedPlatform`] on any OS other
/// than Linux or macOS.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn spawn(
    _command: Command,
    _policy: &SandboxPolicy,
    _stdio: SandboxStdio,
) -> Result<SandboxedChild, SandboxError> {
    Err(SandboxError::UnsupportedPlatform)
}

/// Whether this crate's sandbox backend can actually engage on this host --
/// a cheap, side-effect-free capability probe (no process spawned, no
/// namespace/profile negotiation), for a caller (the agent approval policy,
/// `docs/agent-approval-design.md`'s tier 1) that needs to know *before*
/// deciding whether to auto-approve a contained action, not just when
/// `spawn` is finally called. `spawn` itself still fails informatively
/// (`SandboxError::Nono`/`UnsupportedPlatform`) if this was skipped or went
/// stale between the check and the call (e.g. Landlock disabled mid-session
/// via a kernel reconfiguration) -- this is a fast-path decision, never a
/// substitute for that error handling.
#[cfg(target_os = "linux")]
pub fn is_available() -> bool {
    linux::is_available()
}

#[cfg(target_os = "macos")]
pub fn is_available() -> bool {
    macos::is_available()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn is_available() -> bool {
    false
}
