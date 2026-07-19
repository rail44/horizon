//! TMPDIR parity, shared by both OS backends (`docs/roadmap.md`'s
//! backlog-60 entry). Neither nono backend gives a private tmpfs the way
//! bwrap gave Linux for free via its mount namespace -- Landlock has no
//! mount namespace at all, and Seatbelt's `apply_auto` doesn't touch
//! mounts either. Both backends substitute the same thing: if the caller
//! hasn't already set `TMPDIR` and the policy has at least one writable
//! root, provision `<root>/SCRATCH_DIR_NAME` and inject `TMPDIR` pointing
//! at it, so TMPDIR-respecting tools (`mktemp`, most language runtimes'
//! temp-file helpers) keep working exactly as they did against bwrap's
//! private tmpfs, without every backend re-implementing this itself. A
//! caller with no writable roots (a fully read-only sandbox) correctly
//! gets no scratch space. A literal `/tmp` write that ignores `TMPDIR` is
//! denied outright (`/tmp` is only ever readable, never a writable root)
//! -- a real, deliberate behavior change from bwrap's private-tmpfs
//! illusion; see `linux::tests` for the regression coverage (Linux-only:
//! it spawns real processes, which this crate can only do on the host OS
//! it's actually built for).

use crate::error::SandboxError;
use crate::policy::SandboxPolicy;
use std::process::Command;

/// Whether `command`'s explicit environment overrides already set
/// `TMPDIR`, or this process's own ambient environment does (which
/// `Command` inherits into the child by default unless the caller
/// explicitly cleared it) -- see [`provision`]'s doc for why this gates
/// the scratch-dir provisioning below.
fn command_already_sets_tmpdir(command: &Command) -> bool {
    let mut overrides = command.get_envs();
    if let Some((_, value)) = overrides.find(|(key, _)| *key == "TMPDIR") {
        return value.is_some();
    }
    std::env::var_os("TMPDIR").is_some()
}

/// Provisions the scratch dir and injects `TMPDIR` onto `wrapped` if
/// needed. `command` is the caller's original, unwrapped command (consulted
/// only to check whether it already set `TMPDIR` explicitly); `wrapped` is
/// the backend's rebuilt command that will actually run (the direct spawn
/// on Linux, the exec-helper invocation on macOS) -- `TMPDIR` is set on
/// this one so it's already part of the sandboxed process's environment by
/// the time it starts (on macOS, the helper simply inherits it across its
/// own `exec()` into the real command, since `exec` only changes what a
/// `Command` explicitly overrides).
pub(crate) fn provision(
    policy: &SandboxPolicy,
    command: &Command,
    wrapped: &mut Command,
) -> Result<(), SandboxError> {
    if command_already_sets_tmpdir(command) {
        return Ok(());
    }
    let Some(root) = policy.writable_roots.first() else {
        return Ok(());
    };
    let scratch = root.join(crate::SCRATCH_DIR_NAME);
    std::fs::create_dir_all(&scratch)?;
    wrapped.env("TMPDIR", &scratch);
    Ok(())
}
