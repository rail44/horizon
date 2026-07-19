//! Sandbox-denial classification.
//!
//! Shape and keyword list are informed by OpenAI Codex's
//! `is_likely_sandbox_denied` (Apache-2.0,
//! `codex-rs/sandboxing/src/denial.rs` at
//! github.com/openai/codex, fetched 2026-07-19): a finished command's exit
//! code and stderr get pattern-matched against the keywords real sandboxes
//! actually emit, so a caller can offer a "retry without sandbox?" prompt
//! (the industry-converged denial UX, see
//! `docs/agent-approval-design.md`) instead of surfacing a raw failure the
//! model has no way to distinguish from its own bug.

/// Substrings that show up in sandbox-denial stderr across bwrap,
/// Landlock, seccomp, and macOS Seatbelt.
const SANDBOX_DENIED_KEYWORDS: [&str; 7] = [
    "operation not permitted",
    "permission denied",
    "read-only file system",
    "seccomp",
    "sandbox",
    "landlock",
    "failed to write file",
];

/// Exit codes that are almost always the command's own failure (invalid
/// usage, exec failure, command not found) rather than a sandbox denial --
/// checking them before the signal-based heuristic below avoids
/// misclassifying e.g. a plain "no such file" as a denial.
const QUICK_REJECT_EXIT_CODES: [i32; 3] = [2, 126, 127];

/// `128 + SIGSYS`: the exit code of a process killed by a `Trap`-action
/// seccomp filter. This crate's own network-cut filter uses `Errno`
/// instead of `Trap` (see `linux::seccomp`), so this mostly documents the
/// shape for callers who might install a `Trap`-based filter of their own.
#[cfg(unix)]
fn sigsys_exit_code() -> i32 {
    128 + libc::SIGSYS
}

#[cfg(not(unix))]
fn sigsys_exit_code() -> i32 {
    -1
}

/// Was a finished, sandboxed command most likely denied *by the sandbox*,
/// as opposed to failing on its own logic? `exit_code` is the process's
/// exit status (or a signal-derived code such as `128 + signal`); `stderr`
/// is its captured error output. Always `false` when `sandboxed` is
/// `false` or the command exited `0`.
pub fn is_likely_sandbox_denied(sandboxed: bool, exit_code: i32, stderr: &str) -> bool {
    if !sandboxed || exit_code == 0 {
        return false;
    }

    let lower = stderr.to_lowercase();
    if SANDBOX_DENIED_KEYWORDS
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return true;
    }

    if QUICK_REJECT_EXIT_CODES.contains(&exit_code) {
        return false;
    }

    exit_code == sigsys_exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsandboxed_never_denied() {
        assert!(!is_likely_sandbox_denied(false, 1, "Permission denied"));
    }

    #[test]
    fn success_never_denied() {
        assert!(!is_likely_sandbox_denied(true, 0, "Permission denied"));
    }

    #[test]
    fn keyword_match_is_denied() {
        assert!(is_likely_sandbox_denied(
            true,
            1,
            "bash: /out/side: Read-only file system"
        ));
        assert!(is_likely_sandbox_denied(
            true,
            1,
            "bwrap: setting up uid map: Operation not permitted"
        ));
    }

    #[test]
    fn keyword_match_is_case_insensitive() {
        assert!(is_likely_sandbox_denied(true, 1, "PERMISSION DENIED"));
    }

    #[test]
    fn quick_reject_exit_codes_are_not_denials() {
        assert!(!is_likely_sandbox_denied(true, 127, "command not found"));
        assert!(!is_likely_sandbox_denied(true, 126, "cannot execute"));
        assert!(!is_likely_sandbox_denied(
            true,
            2,
            "No such file or directory"
        ));
    }

    #[test]
    fn plain_ordinary_failure_is_not_a_denial() {
        assert!(!is_likely_sandbox_denied(true, 1, "assertion failed"));
    }

    #[test]
    fn sigsys_exit_code_is_a_denial() {
        assert!(is_likely_sandbox_denied(true, sigsys_exit_code(), ""));
    }
}
