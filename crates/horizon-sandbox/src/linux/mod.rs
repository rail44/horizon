//! Linux sandbox backend: `nono` (Landlock-based capability sandboxing;
//! see `docs/agent-approval-design.md`'s "Sandbox architecture" for the
//! design and `crate::caps` for the `SandboxPolicy` -> `nono::CapabilitySet`
//! mapping, shared with the macOS backend). Migrated 2026-07-19 from a
//! self-built bwrap+seccompiler+landlock stack (`docs/roadmap.md`'s
//! backlog-60 entry, "option C"), de-risked in `experiments/nono-spike/`
//! (branch `worktree-agent-afb6d8b9e874320c8`, commit `533554b`).
//!
//! `nono::Sandbox::apply_auto` is a plain blocking call that restricts
//! the *calling thread* (inherited by that thread's future `fork`/`exec`
//! descendants only) -- no `pre_exec` needed. This drops into the exact
//! dedicated-thread shape the old backend already used for its seccomp
//! filter: apply on a throwaway `std::thread::spawn`, spawn the child
//! from that same thread, `join` to hand it back. Every other thread of a
//! multi-threaded host (e.g. `horizon-sessiond`) stays fully
//! unrestricted -- verified in the spike's Q1 probe. macOS's `apply_auto`
//! has no equivalent thread-scoping (see `macos/mod.rs`'s module doc for
//! why that backend needs a separate exec helper instead).
//!
//! **Accepted regression (backlog 60):** nono has no mount or PID
//! namespace, unlike bwrap. A sandboxed child can see the host's full
//! process list (`ps`, `/proc/<pid>`) and mount table. Filesystem,
//! network, and (new, see `crate::caps`) signal containment are still
//! fully enforced via Landlock.

use crate::error::SandboxError;
use crate::policy::{SandboxPolicy, SandboxStdio};
use crate::SandboxedChild;
use std::ffi::OsString;
use std::process::Command;

/// Diagnostic summary of what nono actually applied for a spawned child.
/// Repurposes the old backend's `LandlockReport`'s diagnostic role
/// (`docs/roadmap.md`'s backlog-60 entry): unlike that report, this
/// reflects containment that is genuinely live around `SandboxedChild`,
/// since nono's `Sandbox::apply_auto` *is* the primary containment now,
/// not a separate probe decoupled from bwrap's own mechanism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonoReport {
    /// The Landlock ABI version nono detected on this kernel (e.g. `"V6"`).
    pub abi: &'static str,
    /// Debug-formatted `nono::Sandbox::apply_auto`'s seccomp-fallback
    /// outcome -- whether Landlock alone enforced the policy's network
    /// mode, or a seccomp filter was additionally installed because the
    /// detected ABI lacks network support (< V4). The concrete type
    /// (`nono`'s private `SeccompNetFallback`) isn't part of nono's public
    /// API surface, so this carries its `Debug` rendering instead.
    pub network_fallback: String,
}

/// Cheap capability probe backing [`crate::is_available`]: whether
/// Landlock is available on this kernel at all (nono's own
/// `Sandbox::detect_abi`, internally cached after the first call), with
/// no process spawned and no capability set built.
pub(crate) fn is_available() -> bool {
    nono::Sandbox::detect_abi().is_ok()
}

/// Prepares and spawns `command` under `policy`. Builds a `nono::
/// CapabilitySet` from `policy` (`crate::caps::build`, shared with the
/// macOS backend), then applies it via `Sandbox::apply_auto` on a
/// dedicated thread that also spawns the child -- see the module doc for
/// why that thread shape matters. Unlike the old bwrap backend,
/// `command`'s program/args are run directly: nono has no wrapper binary
/// on Linux, so there is no argv to rebuild around.
pub(crate) fn spawn(
    command: Command,
    policy: &SandboxPolicy,
    stdio: SandboxStdio,
) -> Result<SandboxedChild, SandboxError> {
    let abi = nono::Sandbox::detect_abi()?;
    let caps = crate::caps::build(policy)?;

    let program = command.get_program().to_os_string();
    let args: Vec<OsString> = command.get_args().map(|a| a.to_os_string()).collect();

    let mut wrapped = Command::new(&program);
    wrapped.args(&args);
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

    // TMPDIR parity (`docs/roadmap.md`'s backlog-60 entry): bwrap gave a
    // private tmpfs `/tmp` for free via its mount namespace; nono has no
    // mount namespace at all, so there is nothing to substitute a fresh
    // `/tmp` with. Hoisted to `crate::tmpdir` since macOS needs the exact
    // same substitution -- see that module's doc.
    crate::tmpdir::provision(policy, &command, &mut wrapped)?;

    wrapped
        .stdin(stdio.stdin)
        .stdout(stdio.stdout)
        .stderr(stdio.stderr);

    // `CapabilitySet`, `DetectedAbi`, and `Command` are all `Send +
    // 'static`, so this thread can own everything it needs and hand back
    // the spawned child plus a diagnostic summary.
    let handle = std::thread::spawn(
        move || -> Result<(std::process::Child, String), SandboxError> {
            let fallback = nono::Sandbox::apply_auto_with_abi(&caps, &abi)?;
            let child = wrapped.spawn()?;
            Ok((child, format!("{fallback:?}")))
        },
    );

    let (child, network_fallback) = handle.join().map_err(|_| SandboxError::ThreadPanicked)??;

    Ok(SandboxedChild {
        child,
        nono: Some(NonoReport {
            abi: abi.version_string(),
            network_fallback,
        }),
    })
}

#[cfg(test)]
mod tests;
