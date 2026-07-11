//! The default `horizon-sessiond` socket path -- shared between
//! `horizon-sessiond` (which binds it) and Horizon's `agent::connection`
//! (which connects to it), so the two independently arrive at the same
//! default without either depending on the other. Both sides also accept an
//! explicit override: `horizon-sessiond --socket <path>` always wins on the
//! bind side (see that binary's `main`), and `$HORIZON_SESSIOND_SOCKET` --
//! resolved right here, so it's honored identically by both the bind-side
//! fallback (`horizon-sessiond` only reaches [`default_socket_path`] when
//! `--socket` was *not* given) and every client-side call
//! (`agent::connection` never passes a CLI flag at
//! all) -- overrides the plain XDG/`/tmp` fallback below. This is what lets
//! a test harness point an isolated `horizon-sessiond` instance and Horizon's
//! own connect attempt at the same scratch path with a single env var,
//! without separately threading `--socket` through a spawn (see
//! `docs/tasks/backlog.md` item 14).
//!
//! Deliberately not part of [`crate::wire`]: the wire module's framing is
//! transport-agnostic (see its module doc, ACP guardrail 2) and shouldn't
//! know that Unix sockets exist, whereas this default path only makes sense
//! for that one transport.

use std::path::PathBuf;

/// `$HORIZON_SESSIOND_SOCKET` if set to a non-empty value, otherwise
/// `$XDG_RUNTIME_DIR/horizon/sessiond.sock`, falling back to
/// `/tmp/horizon-sessiond-$UID.sock` when `XDG_RUNTIME_DIR` is also unset or
/// empty.
pub fn default_socket_path() -> PathBuf {
    let override_path = std::env::var("HORIZON_SESSIOND_SOCKET").ok();
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    // SAFETY: `getuid()` is a plain syscall wrapper with no preconditions.
    let uid = unsafe { libc::getuid() };
    default_socket_path_from(override_path, xdg_runtime_dir, uid)
}

/// Pure resolution logic behind [`default_socket_path`], factored out for
/// unit-testability without mutating process environment variables --
/// `cargo test` runs tests in parallel within one process, so real env
/// mutation in a test would race every other test reading the same
/// variable (same rationale as `config::resolve_model` and friends).
fn default_socket_path_from(
    override_path: Option<String>,
    xdg_runtime_dir: Option<String>,
    uid: u32,
) -> PathBuf {
    if let Some(path) = override_path.filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("horizon").join("sessiond.sock"),
        None => PathBuf::from(format!("/tmp/horizon-sessiond-{uid}.sock")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_xdg_runtime_dir_when_set() {
        assert_eq!(
            default_socket_path_from(None, Some("/run/user/1000".to_string()), 1000),
            PathBuf::from("/run/user/1000/horizon/sessiond.sock")
        );
    }

    #[test]
    fn falls_back_to_tmp_with_uid_when_xdg_runtime_dir_is_unset_or_empty() {
        assert_eq!(
            default_socket_path_from(None, None, 1000),
            PathBuf::from("/tmp/horizon-sessiond-1000.sock")
        );
        assert_eq!(
            default_socket_path_from(None, Some(String::new()), 1000),
            PathBuf::from("/tmp/horizon-sessiond-1000.sock")
        );
    }

    #[test]
    fn override_wins_over_xdg_runtime_dir() {
        assert_eq!(
            default_socket_path_from(
                Some("/tmp/scratch/sessiond.sock".to_string()),
                Some("/run/user/1000".to_string()),
                1000
            ),
            PathBuf::from("/tmp/scratch/sessiond.sock")
        );
    }

    #[test]
    fn override_wins_when_xdg_runtime_dir_is_unset() {
        assert_eq!(
            default_socket_path_from(Some("/tmp/scratch/sessiond.sock".to_string()), None, 1000),
            PathBuf::from("/tmp/scratch/sessiond.sock")
        );
    }

    #[test]
    fn empty_override_falls_through_to_the_usual_resolution() {
        assert_eq!(
            default_socket_path_from(
                Some(String::new()),
                Some("/run/user/1000".to_string()),
                1000
            ),
            PathBuf::from("/run/user/1000/horizon/sessiond.sock")
        );
    }
}
