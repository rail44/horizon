//! Cross-platform pid -> cwd sampling, per `docs/session-relationship-
//! design.md`'s "cwd sourcing is shell-independent" implementation note:
//! reads the *live* current working directory of a running process by pid
//! via `sysinfo` (backed by `/proc/<pid>/cwd` on Linux, libproc on macOS --
//! Horizon's two targets), never OSC 7 or any other shell-dependent
//! mechanism.

use std::path::PathBuf;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Samples the current working directory of the process identified by
/// `pid` right now. `None` covers every failure mode uniformly (the
/// process already exited, its cwd isn't readable -- e.g. permission
/// denied -- or the platform doesn't support it): callers only need "did
/// this work or not", not why.
///
/// Builds a fresh `System` per call rather than keeping one refreshed in
/// the background: this is an on-demand sample ("sample its cwd on
/// demand", not a continuously-tracked process table), and terminal-cwd
/// sourcing only ever needs one pid's cwd, once, at spawn time.
pub(crate) fn sample_cwd(pid: u32) -> Option<PathBuf> {
    let sysinfo_pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[sysinfo_pid]),
        false,
        ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always),
    );
    system
        .process(sysinfo_pid)
        .and_then(|process| process.cwd())
        .map(|path| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live integration sanity check against the current test process's
    /// own pid -- always available, no child process needed. Confirms the
    /// `sysinfo` wiring (feature flags, `UpdateKind::Always`) actually
    /// reads a real cwd end to end; the cwd-resolution *logic* that
    /// consumes this is tested separately, behind a mockable seam, so it
    /// doesn't depend on a live pid (see `app::runtime::spawn_cwd`).
    #[test]
    fn sample_cwd_reads_the_current_process_own_cwd() {
        let expected = std::env::current_dir().expect("current dir must be readable in tests");
        let sampled = sample_cwd(std::process::id())
            .expect("sampling this test process's own live pid must succeed");
        assert_eq!(sampled, expected);
    }

    #[test]
    fn sample_cwd_returns_none_for_a_pid_that_does_not_exist() {
        // Not a portable "impossible pid" guarantee, but PID_MAX_LIMIT on
        // Linux tops out well below this, and macOS pids are 32-bit too --
        // this value is never a live process on either target.
        assert_eq!(sample_cwd(u32::MAX), None);
    }
}
