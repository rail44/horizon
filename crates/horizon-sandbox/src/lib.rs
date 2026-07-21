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
//! for the policy-tiers leg (`docs/agent-approval-design.md`). Both OS
//! backends now depend on `nono` (Landlock on Linux, Seatbelt on macOS --
//! `docs/roadmap.md`'s backlog-60 entry), migrated 2026-07-19 from
//! self-built bwrap+seccompiler+landlock (Linux) and `sandbox-exec`+SBPL
//! (macOS) respectively; the two backends apply it very differently --
//! see `linux/mod.rs` and `macos/mod.rs`'s module docs.
//!
//! ## Stdio: the caller must state it explicitly
//!
//! `spawn` rebuilds a fresh `Command` around the caller's `command` through
//! `horizon-sandbox-helper` on both OSes: Linux keeps a reduced supervisor,
//! while macOS self-applies Seatbelt before exec -- see
//! `linux::spawn`/`macos::spawn`. It preserves the program, arguments,
//! working directory, and explicit
//! environment overrides -- all things `std::process::Command` exposes
//! getters for (`get_program`/`get_args`/`get_current_dir`/`get_envs`). It
//! cannot also copy whatever `stdin`/`stdout`/`stderr` the caller
//! configured on `command` itself, because `Command` provides no getter
//! for that at all (write-only API) -- so `spawn` takes a separate
//! [`SandboxStdio`] parameter instead of trying to infer it.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod caps;
mod error;
mod grant;
#[cfg(any(target_os = "macos", all(target_os = "linux", not(test))))]
mod helper;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod policy;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod tmpdir;

pub use error::SandboxError;
pub use grant::revalidate_denial as revalidate_filesystem_denial;
pub use policy::{
    ContainmentDenials, FilesystemDenial, FilesystemGrant, FilesystemGrantAccess,
    FilesystemGrantScope, HelperPolicy, NetworkDenial, NetworkPolicy, ReadableScope, SandboxPolicy,
    SandboxStdio,
};

#[cfg(target_os = "linux")]
pub use linux::SupervisorReport;

#[cfg(target_os = "linux")]
pub use horizon_sandbox_runtime::ReportError as SupervisorReportError;

#[cfg(target_os = "linux")]
#[doc(hidden)]
pub use linux::execute_supervised_helper;

#[cfg(target_os = "macos")]
pub use macos::apply_seatbelt_to_self;

use std::process::{Child, Command};

/// Name of the TMPDIR-parity scratch directory both OS backends provision
/// under a sandboxed command's first writable root (see `crate::tmpdir`'s
/// module doc) -- e.g. `<root>/.horizon-sandbox-tmp`. Exposed so callers
/// that manage the writable root's lifecycle themselves (e.g.
/// `horizon-sessiond`'s isolated-worktree cleanup) can special-case this
/// specific directory without duplicating the literal.
pub const SCRATCH_DIR_NAME: &str = ".horizon-sandbox-tmp";

/// Embedded in the real helper entry point so Cargo unit-test executables
/// can distinguish it from the helper target's own test harness artifact.
#[doc(hidden)]
pub const HELPER_PROTOCOL_MARKER: &str = "HORIZON_SANDBOX_HELPER_PROTOCOL_V1_SUPERVISED_LINUX";

/// A spawned sandboxed process and its authoritative supervisor report.
pub struct SandboxedChild {
    pub child: Child,
    #[cfg(target_os = "linux")]
    pub supervisor_report: Option<SupervisorReport>,
}

/// Prepares `command` to run under `policy` and spawns it, dispatching to
/// the current OS's backend. `command`'s program, arguments, working
/// directory, and explicit environment overrides are preserved; the
/// backend rebuilds the actual spawn around them via
/// `horizon-sandbox-helper` on both OSes -- see
/// `linux::spawn`/`macos::spawn`. `stdio` is
/// applied to the rebuilt command explicitly -- see the crate root doc's
/// "Stdio" section for why `spawn` can't infer it from `command` itself.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn spawn(
    command: Command,
    policy: &SandboxPolicy,
    stdio: SandboxStdio,
) -> Result<SandboxedChild, SandboxError> {
    spawn_with_filesystem_grants(command, policy, &[], stdio)
}

/// Spawns with additional approved filesystem capabilities while preserving
/// the base policy and containment mechanism.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn spawn_with_filesystem_grants(
    command: Command,
    policy: &SandboxPolicy,
    filesystem_grants: &[FilesystemGrant],
    stdio: SandboxStdio,
) -> Result<SandboxedChild, SandboxError> {
    #[cfg(target_os = "linux")]
    {
        linux::spawn_with_grants(command, policy, filesystem_grants, stdio)
    }
    #[cfg(target_os = "macos")]
    {
        macos::spawn_with_grants(command, policy, filesystem_grants, stdio)
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn spawn_with_filesystem_grants(
    _command: Command,
    _policy: &SandboxPolicy,
    _filesystem_grants: &[FilesystemGrant],
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
