//! This process's control-plane socket path -- see
//! `docs/cli-control-plane-design.md`'s "Discovery" decision, amended by its
//! Second revision's "Fixed well-known socket path" item. Modeled on
//! `horizon_agent::socket`'s stale-socket handling and default-path shape,
//! without sharing code with it: `horizon-agent` is Horizon's *agent* seam
//! (contract/providers/tools/persistence) and must stay a library any future
//! consumer beyond Horizon can depend on without pulling in workspace-
//! control concerns -- reusing its socket module here would point the
//! dependency the wrong way (see `docs/agent-runtime-split-design.md`'s
//! reusable-asset boundary).
//!
//! The path is fixed per *user*, exactly like
//! `horizon_agent::socket::default_socket_path` -- the single-instance norm
//! justifies this (a second Horizon finding a live owner at this path
//! doesn't steal it: it starts without a control listener and logs a
//! warning, see `listener::bind`/`listener::spawn`). `crates/horizon-ctl`
//! independently computes the identical formula on the client side (it
//! can't depend on this crate, and `crates/horizon-control` deliberately
//! stays transport-agnostic, see that crate's doc comment) -- the same
//! "small pure formula duplicated across a client/server pair" shape this
//! module already uses relative to `horizon_agent::socket`. `HORIZON_SOCKET`
//! remains the override on both sides and is still injected into
//! panes/agentd, which is what keeps a nested dev instance addressable (see
//! the design doc).
use std::path::PathBuf;

/// `$XDG_RUNTIME_DIR/horizon/control.sock`, falling back to
/// `/tmp/horizon-control-<uid>.sock` when `XDG_RUNTIME_DIR` is unset or
/// empty. Pure and deterministic within one process run (only reads the env
/// var and this process's own uid), so every call site -- the listener's
/// bind, and every terminal/agentd spawn site that injects `HORIZON_SOCKET`
/// -- can call this independently and always agree, with no shared mutable
/// state required.
pub fn default_socket_path() -> PathBuf {
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    // SAFETY: `getuid()` is a plain syscall wrapper with no preconditions.
    let uid = unsafe { libc::getuid() };
    default_socket_path_from(xdg_runtime_dir, uid)
}

fn default_socket_path_from(xdg_runtime_dir: Option<String>, uid: u32) -> PathBuf {
    match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("horizon").join("control.sock"),
        None => PathBuf::from(format!("/tmp/horizon-control-{uid}.sock")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_xdg_runtime_dir_when_set() {
        assert_eq!(
            default_socket_path_from(Some("/run/user/1000".to_string()), 1000),
            PathBuf::from("/run/user/1000/horizon/control.sock")
        );
    }

    #[test]
    fn falls_back_to_tmp_with_uid_when_xdg_runtime_dir_is_unset_or_empty() {
        assert_eq!(
            default_socket_path_from(None, 1000),
            PathBuf::from("/tmp/horizon-control-1000.sock")
        );
        assert_eq!(
            default_socket_path_from(Some(String::new()), 1000),
            PathBuf::from("/tmp/horizon-control-1000.sock")
        );
    }

    #[test]
    fn default_socket_path_is_stable_across_repeated_calls_in_one_process() {
        assert_eq!(default_socket_path(), default_socket_path());
    }
}
