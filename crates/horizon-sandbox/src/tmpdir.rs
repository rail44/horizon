//! TMPDIR parity, shared by both OS backends (`docs/roadmap.md`'s
//! backlog-60 entry). Neither nono backend gives a private tmpfs the way
//! bwrap gave Linux for free via its mount namespace -- Landlock has no
//! mount namespace at all, and Seatbelt's `apply_auto` doesn't touch
//! mounts either. Both backends substitute the same thing: unless the
//! child's inherited `TMPDIR` is already writable under the policy (see
//! [`inherited_tmpdir_is_usable`]) and the policy has at least one
//! writable root, provision `<root>/SCRATCH_DIR_NAME` and inject `TMPDIR`
//! pointing at it, so TMPDIR-respecting tools (`mktemp`, most language
//! runtimes' temp-file helpers) keep working exactly as they did against
//! bwrap's private tmpfs, without every backend re-implementing this
//! itself. A caller with no writable roots (a fully read-only sandbox)
//! correctly gets no scratch space. A literal `/tmp` write that ignores
//! `TMPDIR` is denied outright (`/tmp` is only ever readable, never a
//! writable root) -- a real, deliberate behavior change from bwrap's
//! private-tmpfs illusion; see `linux::tests` for the regression coverage
//! (Linux-only: it spawns real processes, which this crate can only do on
//! the host OS it's actually built for).
//!
//! 2026-09-07: the old gate treated "this process has an ambient `TMPDIR`"
//! as "the child's temp dir already works" and skipped provisioning. On
//! macOS that ambient value is launchd's `/var/folders/...`, which the
//! Seatbelt profile denies -- so every TMPDIR-respecting tool inside the
//! sandbox (clang's link step, rustdoc's doctest dir, `std::env::temp_dir()`
//! users, gpg's lockfiles) failed with "Operation not permitted", and the
//! pre-commit hook's own `*.horizon-sandbox-tmp` containment probe never
//! fired. The gate now provisions unless the inherited `TMPDIR` is
//! provably inside a writable root.

use crate::error::SandboxError;
use crate::policy::SandboxPolicy;
use std::path::Path;
use std::process::Command;

/// Whether the child's inherited `TMPDIR` is already usable inside the
/// sandbox: either `command` explicitly set `TMPDIR` (a deliberate choice,
/// honored as-is even when it points outside the writable roots), or the
/// harness process's ambient `TMPDIR` -- which `Command` inherits when the
/// caller says nothing about it -- lies inside one of the policy's
/// writable roots. An ambient `TMPDIR` *outside* every writable root does
/// not count (Seatbelt/Landlock would deny the writes), and an
/// explicitly *cleared* `TMPDIR` (`env_remove`) means the child inherits
/// nothing at all, so both fall through to provisioning. `ambient_tmpdir`
/// is threaded in rather than read from the environment so the rule stays
/// a pure function tests can drive.
fn inherited_tmpdir_is_usable(
    command: &Command,
    policy: &SandboxPolicy,
    ambient_tmpdir: Option<&std::ffi::OsStr>,
) -> bool {
    let mut overrides = command.get_envs();
    match overrides.find(|(key, _)| *key == "TMPDIR") {
        Some((_, Some(_))) => return true,
        Some((_, None)) => return false,
        None => {}
    }
    let Some(ambient) = ambient_tmpdir else {
        return false;
    };
    let ambient = Path::new(ambient);
    policy
        .writable_roots
        .iter()
        .any(|root| ambient.starts_with(root))
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
    if inherited_tmpdir_is_usable(command, policy, std::env::var_os("TMPDIR").as_deref()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{NetworkPolicy, ReadableScope};
    use std::ffi::OsStr;
    use std::path::PathBuf;

    fn policy_with_root(root: &str) -> SandboxPolicy {
        SandboxPolicy {
            writable_roots: vec![PathBuf::from(root)],
            readable_scope: ReadableScope::Full,
            network: NetworkPolicy::Disabled,
        }
    }

    #[test]
    fn an_explicit_command_tmpdir_is_honored_even_outside_the_writable_roots() {
        let mut command = Command::new("true");
        command.env("TMPDIR", "/some/explicit/tmp");
        assert!(inherited_tmpdir_is_usable(
            &command,
            &policy_with_root("/workspace"),
            Some(OsStr::new("/var/folders/real"))
        ));
    }

    #[test]
    fn an_explicitly_cleared_command_tmpdir_still_provisions() {
        // `env_remove` means the child inherits no TMPDIR at all, so the
        // ambient value's usability is irrelevant.
        let mut command = Command::new("true");
        command.env_remove("TMPDIR");
        assert!(!inherited_tmpdir_is_usable(
            &command,
            &policy_with_root("/workspace"),
            Some(OsStr::new("/workspace"))
        ));
    }

    #[test]
    fn ambient_tmpdir_inside_a_writable_root_is_kept() {
        let command = Command::new("true");
        assert!(inherited_tmpdir_is_usable(
            &command,
            &policy_with_root("/workspace"),
            Some(OsStr::new("/workspace/.horizon-sandbox-tmp"))
        ));
    }

    #[test]
    fn the_macos_ambient_tmpdir_outside_every_writable_root_is_replaced() {
        // The 2026-09-07 regression: launchd's `/var/folders/...` made the
        // old ambient-existence gate skip provisioning on macOS, so the
        // child inherited a Seatbelt-denied temp dir.
        let command = Command::new("true");
        assert!(!inherited_tmpdir_is_usable(
            &command,
            &policy_with_root("/Users/me/project"),
            Some(OsStr::new("/var/folders/jw/95t21_s11g5c2l/T"))
        ));
    }

    #[test]
    fn no_tmpdir_anywhere_provisions() {
        let command = Command::new("true");
        assert!(!inherited_tmpdir_is_usable(
            &command,
            &policy_with_root("/workspace"),
            None
        ));
    }
}
