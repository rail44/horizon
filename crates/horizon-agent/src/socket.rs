//! The default `horizon-agentd` socket path -- shared between
//! `horizon-agentd` (which binds it) and Horizon's `agent::agentd_client`
//! (which connects to it), so the two independently arrive at the same
//! default without either depending on the other. Both sides also accept an
//! explicit override (`horizon-agentd --socket <path>` / a future
//! `HORIZON_AGENTD_SOCKET` on the client side); this module only resolves
//! the fallback used when neither is given.
//!
//! Deliberately not part of [`crate::wire`]: the wire module's framing is
//! transport-agnostic (see its module doc, ACP guardrail 2) and shouldn't
//! know that Unix sockets exist, whereas this default path only makes sense
//! for that one transport.

use std::path::PathBuf;

/// `$XDG_RUNTIME_DIR/horizon/agentd.sock`, falling back to
/// `/tmp/horizon-agentd-$UID.sock` when `XDG_RUNTIME_DIR` is unset or empty.
pub fn default_socket_path() -> PathBuf {
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    // SAFETY: `getuid()` is a plain syscall wrapper with no preconditions.
    let uid = unsafe { libc::getuid() };
    default_socket_path_from(xdg_runtime_dir, uid)
}

/// Pure resolution logic behind [`default_socket_path`], factored out for
/// unit-testability without mutating process environment variables --
/// `cargo test` runs tests in parallel within one process, so real env
/// mutation in a test would race every other test reading the same
/// variable (same rationale as `config::resolve_model` and friends).
fn default_socket_path_from(xdg_runtime_dir: Option<String>, uid: u32) -> PathBuf {
    match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("horizon").join("agentd.sock"),
        None => PathBuf::from(format!("/tmp/horizon-agentd-{uid}.sock")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_xdg_runtime_dir_when_set() {
        assert_eq!(
            default_socket_path_from(Some("/run/user/1000".to_string()), 1000),
            PathBuf::from("/run/user/1000/horizon/agentd.sock")
        );
    }

    #[test]
    fn falls_back_to_tmp_with_uid_when_xdg_runtime_dir_is_unset_or_empty() {
        assert_eq!(
            default_socket_path_from(None, 1000),
            PathBuf::from("/tmp/horizon-agentd-1000.sock")
        );
        assert_eq!(
            default_socket_path_from(Some(String::new()), 1000),
            PathBuf::from("/tmp/horizon-agentd-1000.sock")
        );
    }
}
