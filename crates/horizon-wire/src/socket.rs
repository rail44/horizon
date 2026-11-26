//! Where each runtime daemon binds, and where its clients look --
//! `docs/runtime-crate-alignment-design.md` judgment 2's "socket path
//! conventions for every daemon". The daemon and the shell arrive at the
//! same default without either depending on the other, and the
//! daemons' conventions sit side by side so they cannot drift apart
//! unnoticed (the reason this module is one file rather than one per
//! runtime).
//!
//! Both sides also accept an explicit override: `horizon-terminald
//! --socket <path>` / `horizon-logd --socket <path>` always wins on the bind
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

/// Where `horizon-terminald` binds (`docs/terminald-split-design.md`
/// decision 1): `$HORIZON_TERMINALD_SOCKET`, else
/// `$XDG_RUNTIME_DIR/horizon/terminald.sock`, else
/// `/tmp/horizon-terminald-$UID.sock`.
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

/// [`default_terminald_socket_path`]'s `horizon-logd` sibling
/// (`docs/logd-design.md`): the log daemon binds its own socket next to
/// terminald's, so a drain aimed at one never reaches the other. Same resolution rules -- `$HORIZON_LOGD_SOCKET`, else
/// `$XDG_RUNTIME_DIR/horizon/logd.sock`, else
/// `/tmp/horizon-logd-$UID.sock`.
pub fn default_logd_socket_path() -> PathBuf {
    let override_path = std::env::var("HORIZON_LOGD_SOCKET").ok();
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    // SAFETY: `getuid()` is a plain syscall wrapper with no preconditions.
    let uid = unsafe { libc::getuid() };
    daemon_socket_path_from(
        override_path,
        xdg_runtime_dir,
        uid,
        "logd.sock",
        "horizon-logd",
    )
}

/// The shared resolution the daemons' defaults use, factored out for
/// unit-testability without mutating process environment variables
/// (`cargo test` runs tests in parallel within one process), and parameterized by the
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

    fn terminald(override_path: Option<&str>, xdg_runtime_dir: Option<&str>) -> PathBuf {
        daemon_socket_path_from(
            override_path.map(str::to_string),
            xdg_runtime_dir.map(str::to_string),
            1000,
            "terminald.sock",
            "horizon-terminald",
        )
    }

    #[test]
    fn prefers_xdg_runtime_dir_when_set() {
        assert_eq!(
            terminald(None, Some("/run/user/1000")),
            PathBuf::from("/run/user/1000/horizon/terminald.sock")
        );
    }

    #[test]
    fn falls_back_to_tmp_with_uid_when_xdg_runtime_dir_is_unset_or_empty() {
        assert_eq!(
            terminald(None, None),
            PathBuf::from("/tmp/horizon-terminald-1000.sock")
        );
        assert_eq!(
            terminald(None, Some("")),
            PathBuf::from("/tmp/horizon-terminald-1000.sock")
        );
    }

    #[test]
    fn override_wins_over_xdg_runtime_dir() {
        assert_eq!(
            terminald(Some("/tmp/scratch/t.sock"), Some("/run/user/1000")),
            PathBuf::from("/tmp/scratch/t.sock")
        );
        assert_eq!(
            terminald(Some("/tmp/scratch/t.sock"), None),
            PathBuf::from("/tmp/scratch/t.sock")
        );
    }

    #[test]
    fn empty_override_falls_through_to_the_usual_resolution() {
        assert_eq!(
            terminald(Some(""), Some("/run/user/1000")),
            PathBuf::from("/run/user/1000/horizon/terminald.sock")
        );
    }

    #[test]
    fn the_logd_default_is_a_sibling_of_terminalds() {
        let logd = daemon_socket_path_from(
            None,
            Some("/run/user/1000".to_string()),
            1000,
            "logd.sock",
            "horizon-logd",
        );
        assert_eq!(logd, PathBuf::from("/run/user/1000/horizon/logd.sock"));
        assert_ne!(logd, terminald(None, Some("/run/user/1000")));
        assert_eq!(
            daemon_socket_path_from(None, None, 1000, "logd.sock", "horizon-logd"),
            PathBuf::from("/tmp/horizon-logd-1000.sock")
        );
    }
}
