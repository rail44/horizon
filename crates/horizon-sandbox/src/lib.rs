//! Sandbox spike for the agent approval trust model
//! (`docs/agent-approval-design.md`, "Sandbox architecture"): a thin,
//! product-owned unified API over per-OS process containment.
//!
//! ```no_run
//! use horizon_sandbox::{NetworkPolicy, ReadableScope, SandboxPolicy};
//! use std::process::Command;
//!
//! let policy = SandboxPolicy {
//!     writable_roots: vec!["/tmp/some-session-worktree".into()],
//!     readable_scope: ReadableScope::Full,
//!     network: NetworkPolicy::Disabled,
//! };
//! let mut command = Command::new("bash");
//! command.arg("-c").arg("echo hi");
//! let sandboxed = horizon_sandbox::spawn(command, &policy);
//! ```
//!
//! Platform support is Linux and macOS; anything else is a typed
//! [`SandboxError::UnsupportedPlatform`]. This crate is a prototype: it is
//! not yet wired into any tool-call spawn site (see the crate-level
//! `Cargo.toml` doc comment and the roadmap item that dispatched this
//! spike).
//!
//! ## Known limitation: `command`'s stdio configuration isn't preserved
//!
//! `spawn` rebuilds a fresh `Command` around the caller's `command`
//! (`bwrap <args> -- <command>` on Linux, `sandbox-exec -f <profile> --
//! <command>` on macOS), copying over the program, arguments, working
//! directory, and explicit environment overrides -- all things
//! `std::process::Command` exposes getters for
//! (`get_program`/`get_args`/`get_current_dir`/`get_envs`). It does
//! **not** copy `stdin`/`stdout`/`stderr` configuration, because
//! `Command` provides no getter for it at all (write-only API). A caller
//! that needs piped output has no way to get it through today's `spawn`;
//! the rebuilt command inherits this process's real stdio, same as a
//! plain `Command::spawn()` with no stdio configured. Solving this
//! (explicit stdio parameters, most likely) is follow-up work, not
//! attempted in this spike.

mod denial;
mod error;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod policy;

pub use denial::is_likely_sandbox_denied;
pub use error::SandboxError;
pub use policy::{NetworkPolicy, ReadableScope, SandboxPolicy};

#[cfg(target_os = "linux")]
pub use linux::LandlockReport;

use std::process::{Child, Command};

/// A spawned sandboxed process, plus whatever per-backend containment
/// report is available.
///
/// `landlock` is only ever `Some` on Linux; other backends carry no
/// equivalent (macOS's `sandbox-exec` has no comparable negotiated-ABI
/// concept). **It is diagnostic, not a live guarantee for this specific
/// child**: applying Landlock to the thread that spawns bwrap breaks
/// bwrap itself (a landlocked thread can never call `mount(2)` again --
/// see `linux::landlock`'s module doc for the finding), so this reports
/// what the kernel *would* enforce, negotiated on an isolated throwaway
/// thread, not what actually wraps `child`. `bwrap`'s own bind-mount
/// containment is what actually protects `child` today.
pub struct SandboxedChild {
    pub child: Child,
    #[cfg(target_os = "linux")]
    pub landlock: Option<LandlockReport>,
}

/// Prepares `command` to run under `policy` and spawns it, dispatching to
/// the current OS's backend. `command`'s program, arguments, working
/// directory, and explicit environment overrides are preserved; the
/// backend rebuilds the actual spawn around them (e.g. as
/// `bwrap <containment args> -- <command>` on Linux).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn spawn(command: Command, policy: &SandboxPolicy) -> Result<SandboxedChild, SandboxError> {
    #[cfg(target_os = "linux")]
    {
        linux::spawn(command, policy)
    }
    #[cfg(target_os = "macos")]
    {
        macos::spawn(command, policy)
    }
}

/// Always returns [`SandboxError::UnsupportedPlatform`] on any OS other
/// than Linux or macOS.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn spawn(_command: Command, _policy: &SandboxPolicy) -> Result<SandboxedChild, SandboxError> {
    Err(SandboxError::UnsupportedPlatform)
}
