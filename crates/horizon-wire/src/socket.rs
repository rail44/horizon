//! Where each runtime daemon binds, and where its clients look --
//! `docs/runtime-crate-alignment-design.md` judgment 2's "socket path
//! conventions for every daemon". The daemon and the shell arrive at the
//! same default without either depending on the other, and the two
//! daemons' conventions sit side by side so they cannot drift apart
//! unnoticed (the reason this module is one file rather than one per
//! runtime).
//!
//! Both sides also accept an explicit override: `horizon-agentd --socket
//! <path>` / `horizon-terminald --socket <path>` always wins on the bind
//! side (see those binaries' `main`), and the `$HORIZON_*_SOCKET`
//! environment variables -- resolved right here, so they are honored
//! identically by the bind-side fallback (a daemon only reaches these
//! functions when `--socket` was *not* given) and by every client-side call
//! -- override the plain XDG/`/tmp` fallback below. This is what lets a
//! test harness point an isolated daemon and Horizon's own connect attempt
//! at the same scratch path with a single env var, without separately
//! threading `--socket` through a spawn (see `docs/tasks/backlog.md` item
//! 14).
//!
//! Deliberately not part of any hub vocabulary: hub framing is
//! transport-agnostic, whereas these default paths only make sense for the
//! one transport (Unix sockets).

use std::path::PathBuf;

/// `$HORIZON_AGENTD_SOCKET` (or its legacy alias `$HORIZON_SESSIOND_SOCKET`)
/// if set to a non-empty value, otherwise `$XDG_RUNTIME_DIR/horizon/agentd.sock`,
/// falling back to `/tmp/horizon-agentd-$UID.sock` when `XDG_RUNTIME_DIR` is also
/// unset or empty.
pub fn default_agentd_socket_path() -> PathBuf {
    // Legacy fallback: `HORIZON_SESSIOND_SOCKET` (the pre-rename env var) is
    // still honored so existing test scripts and environments don't break.
    let override_path = std::env::var("HORIZON_AGENTD_SOCKET")
        .ok()
        .or_else(|| std::env::var("HORIZON_SESSIOND_SOCKET").ok());
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    // SAFETY: `getuid()` is a plain syscall wrapper with no preconditions.
    let uid = unsafe { libc::getuid() };
    default_agentd_socket_path_from(override_path, xdg_runtime_dir, uid)
}

/// [`default_agentd_socket_path`]'s sibling for `horizon-terminald`
/// (`docs/terminald-split-design.md` decision 1): the terminal daemon binds
/// its own socket next to agentd's, so draining one never disturbs the
/// other. Same resolution rules, one directory level and one file name
/// apart -- `$HORIZON_TERMINALD_SOCKET`, else
/// `$XDG_RUNTIME_DIR/horizon/terminald.sock`, else
/// `/tmp/horizon-terminald-$UID.sock`. Kept here, beside agentd's, so the
/// two conventions cannot drift apart unnoticed.
pub fn default_terminald_socket_path() -> PathBuf {
    let override_path = std::env::var("HORIZON_TERMINALD_SOCKET").ok();
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    // SAFETY: `getuid()` is a plain syscall wrapper with no preconditions.
    let uid = unsafe { libc::getuid() };
    daemon_socket_path_from(
        override_path,
        xdg_runtime_dir,
        uid,
        "terminald.sock",
        "horizon-terminald",
    )
}

/// Pure resolution logic behind [`default_agentd_socket_path`], factored out
/// for unit-testability without mutating process environment variables --
/// `cargo test` runs tests in parallel within one process, so real env
/// mutation in a test would race every other test reading the same
/// variable (same rationale as `config::resolve_model` and friends).
fn default_agentd_socket_path_from(
    override_path: Option<String>,
    xdg_runtime_dir: Option<String>,
    uid: u32,
) -> PathBuf {
    daemon_socket_path_from(
        override_path,
        xdg_runtime_dir,
        uid,
        "agentd.sock",
        "horizon-agentd",
    )
}

/// The shared resolution both daemons' defaults use, parameterized by the
/// two names that differ: the file inside `$XDG_RUNTIME_DIR/horizon/`, and
/// the `/tmp` fallback's prefix.
fn daemon_socket_path_from(
    override_path: Option<String>,
    xdg_runtime_dir: Option<String>,
    uid: u32,
    file_name: &str,
    tmp_prefix: &str,
) -> PathBuf {
    if let Some(path) = override_path.filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("horizon").join(file_name),
        None => PathBuf::from(format!("/tmp/{tmp_prefix}-{uid}.sock")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_xdg_runtime_dir_when_set() {
        assert_eq!(
            default_agentd_socket_path_from(None, Some("/run/user/1000".to_string()), 1000),
            PathBuf::from("/run/user/1000/horizon/agentd.sock")
        );
    }

    #[test]
    fn falls_back_to_tmp_with_uid_when_xdg_runtime_dir_is_unset_or_empty() {
        assert_eq!(
            default_agentd_socket_path_from(None, None, 1000),
            PathBuf::from("/tmp/horizon-agentd-1000.sock")
        );
        assert_eq!(
            default_agentd_socket_path_from(None, Some(String::new()), 1000),
            PathBuf::from("/tmp/horizon-agentd-1000.sock")
        );
    }

    #[test]
    fn override_wins_over_xdg_runtime_dir() {
        assert_eq!(
            default_agentd_socket_path_from(
                Some("/tmp/scratch/agentd.sock".to_string()),
                Some("/run/user/1000".to_string()),
                1000
            ),
            PathBuf::from("/tmp/scratch/agentd.sock")
        );
    }

    #[test]
    fn override_wins_when_xdg_runtime_dir_is_unset() {
        assert_eq!(
            default_agentd_socket_path_from(
                Some("/tmp/scratch/agentd.sock".to_string()),
                None,
                1000
            ),
            PathBuf::from("/tmp/scratch/agentd.sock")
        );
    }

    #[test]
    fn the_terminald_default_is_a_sibling_of_agentds() {
        // Decision 1's "sibling socket": same directory, different file --
        // so a drain aimed at one daemon can never reach the other.
        assert_eq!(
            daemon_socket_path_from(
                None,
                Some("/run/user/1000".to_string()),
                1000,
                "terminald.sock",
                "horizon-terminald",
            ),
            PathBuf::from("/run/user/1000/horizon/terminald.sock")
        );
        assert_eq!(
            daemon_socket_path_from(None, None, 1000, "terminald.sock", "horizon-terminald"),
            PathBuf::from("/tmp/horizon-terminald-1000.sock")
        );
        assert_ne!(
            daemon_socket_path_from(
                None,
                Some("/run/user/1000".to_string()),
                1000,
                "terminald.sock",
                "horizon-terminald",
            ),
            default_agentd_socket_path_from(None, Some("/run/user/1000".to_string()), 1000)
        );
    }

    #[test]
    fn empty_override_falls_through_to_the_usual_resolution() {
        assert_eq!(
            default_agentd_socket_path_from(
                Some(String::new()),
                Some("/run/user/1000".to_string()),
                1000
            ),
            PathBuf::from("/run/user/1000/horizon/agentd.sock")
        );
    }
}
