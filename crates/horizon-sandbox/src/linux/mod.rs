//! Linux sandbox backend: bubblewrap (namespace/fs containment) +
//! seccompiler (network-syscall cut). See `docs/agent-approval-design.md`'s
//! "Sandbox architecture" for the design, and each submodule for its
//! slice.
//!
//! Landlock (`landlock.rs`) is negotiated for its `LandlockReport`
//! diagnostic but is **not** applied around the bwrap-spawning thread
//! below: doing so was tried and reliably breaks bwrap itself (a landlocked
//! thread can never call `mount(2)` again, a kernel-level Landlock
//! limitation, and bwrap's entire mechanism *is* mount syscalls). See
//! `landlock.rs`'s module doc for the full finding and what real
//! Landlock-as-live-backstop would require.

mod bwrap;
mod landlock;
mod seccomp;

pub use landlock::LandlockReport;

use crate::error::SandboxError;
use crate::policy::{NetworkPolicy, SandboxPolicy, SandboxStdio};
use crate::SandboxedChild;
use std::path::Path;
use std::process::Command;

/// Absolute locations checked for `bwrap`, in order. Deliberately not a
/// bare-name `PATH` lookup: the same "don't trust an environment the
/// sandboxed command might influence" reasoning the design doc calls for
/// on macOS's hardcoded `sandbox-exec` path.
const BWRAP_CANDIDATES: [&str; 2] = ["/usr/bin/bwrap", "/bin/bwrap"];

fn resolve_bwrap() -> Result<&'static str, SandboxError> {
    BWRAP_CANDIDATES
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .ok_or(SandboxError::BwrapNotFound {
            searched: BWRAP_CANDIDATES.to_vec(),
        })
}

/// Cheap capability probe backing [`crate::is_available`]: whether `bwrap`
/// resolves at all, with no process spawned and no Landlock/seccomp
/// negotiation.
pub(crate) fn is_available() -> bool {
    resolve_bwrap().is_ok()
}

/// Prepares and spawns `command` under `policy`. Rebuilds `command`'s
/// program/args/cwd/env onto a fresh `bwrap` invocation, applies the
/// seccomp network-cut on the thread that spawns it (seccomp, unlike
/// Landlock, doesn't interfere with bwrap's own `mount(2)`-heavy setup --
/// verified directly), and separately negotiates (but does not apply) the
/// Landlock report on its own throwaway thread for diagnostics.
pub(crate) fn spawn(
    command: Command,
    policy: &SandboxPolicy,
    stdio: SandboxStdio,
) -> Result<SandboxedChild, SandboxError> {
    let bwrap_path = resolve_bwrap()?;

    let program = command.get_program().to_os_string();
    let args: Vec<_> = command.get_args().map(|a| a.to_os_string()).collect();
    let argv = bwrap::build_args(policy, &program, &args)?;

    let mut wrapped = Command::new(bwrap_path);
    wrapped.args(&argv);
    if let Some(cwd) = command.get_current_dir() {
        wrapped.current_dir(cwd);
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(v) => {
                wrapped.env(key, v);
            }
            None => {
                wrapped.env_remove(key);
            }
        }
    }
    wrapped
        .stdin(stdio.stdin)
        .stdout(stdio.stdout)
        .stderr(stdio.stderr);

    let network_filter = if policy.network == NetworkPolicy::Disabled {
        Some(seccomp::build_network_cut_filter().map_err(SandboxError::Seccomp)?)
    } else {
        None
    };

    // Diagnostic only -- see module doc and `landlock.rs` for why this
    // negotiation is deliberately decoupled from the spawn below.
    let landlock_report = landlock::negotiate(policy)?;

    // `Command` and `BpfProgram` (`Vec<u64>`) are `Send + 'static`, so
    // this thread can own both and hand back the spawned child. Only
    // seccomp is applied here (see module doc for why Landlock isn't).
    let handle = std::thread::spawn(move || -> Result<std::process::Child, SandboxError> {
        if let Some(filter) = &network_filter {
            seccompiler::apply_filter(filter).map_err(|e| SandboxError::Seccomp(e.to_string()))?;
        }
        Ok(wrapped.spawn()?)
    });

    let child = handle.join().map_err(|_| SandboxError::ThreadPanicked)??;

    Ok(SandboxedChild {
        child,
        landlock: Some(landlock_report),
    })
}

#[cfg(test)]
mod tests;
