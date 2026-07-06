//! This process's control-plane socket path -- see
//! `docs/cli-control-plane-design.md`'s "Discovery" decision. Modeled on
//! `horizon_agent::socket`'s stale-socket handling and default-path shape,
//! without sharing code with it: `horizon-agent` is Horizon's *agent* seam
//! (contract/providers/tools/persistence) and must stay a library any future
//! consumer beyond Horizon can depend on without pulling in workspace-
//! control concerns -- reusing its socket module here would point the
//! dependency the wrong way (see `docs/agent-runtime-split-design.md`'s
//! reusable-asset boundary).
//!
//! Unlike `horizon_agent::socket::default_socket_path` (one fixed path per
//! *user*, so agentd and Horizon's client independently arrive at the same
//! path without either depending on the other), this path is scoped per
//! *process*: each Horizon instance -- a stable one plus a nested dev one,
//! per `docs/trust-boundaries.md` -- owns its own control socket, and there
//! is no second binary that needs to independently compute the same
//! default. A future CLI discovers it purely through `HORIZON_SOCKET` env
//! var injection (see `docs/cli-control-plane-design.md`'s "Discovery"
//! decision), never by recomputing this function itself.

use std::path::PathBuf;

/// `$XDG_RUNTIME_DIR/horizon/control-<pid>.sock`, falling back to
/// `/tmp/horizon-control-<uid>-<pid>.sock` when `XDG_RUNTIME_DIR` is unset or
/// empty. Pure and deterministic within one process run (only reads the env
/// var and this process's own uid/pid), so every call site -- the listener's
/// bind, and every terminal/agentd spawn site that injects `HORIZON_SOCKET`
/// -- can call this independently and always agree, with no shared mutable
/// state required.
pub(crate) fn default_socket_path() -> PathBuf {
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    // SAFETY: `getuid()` is a plain syscall wrapper with no preconditions.
    let uid = unsafe { libc::getuid() };
    let pid = std::process::id();
    default_socket_path_from(xdg_runtime_dir, uid, pid)
}

fn default_socket_path_from(xdg_runtime_dir: Option<String>, uid: u32, pid: u32) -> PathBuf {
    match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir)
            .join("horizon")
            .join(format!("control-{pid}.sock")),
        None => PathBuf::from(format!("/tmp/horizon-control-{uid}-{pid}.sock")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_xdg_runtime_dir_when_set() {
        assert_eq!(
            default_socket_path_from(Some("/run/user/1000".to_string()), 1000, 4242),
            PathBuf::from("/run/user/1000/horizon/control-4242.sock")
        );
    }

    #[test]
    fn falls_back_to_tmp_with_uid_and_pid_when_xdg_runtime_dir_is_unset_or_empty() {
        assert_eq!(
            default_socket_path_from(None, 1000, 4242),
            PathBuf::from("/tmp/horizon-control-1000-4242.sock")
        );
        assert_eq!(
            default_socket_path_from(Some(String::new()), 1000, 4242),
            PathBuf::from("/tmp/horizon-control-1000-4242.sock")
        );
    }

    #[test]
    fn different_pids_never_collide_on_the_same_path() {
        let a = default_socket_path_from(Some("/run/user/1000".to_string()), 1000, 1);
        let b = default_socket_path_from(Some("/run/user/1000".to_string()), 1000, 2);
        assert_ne!(a, b);
    }

    #[test]
    fn default_socket_path_is_stable_across_repeated_calls_in_one_process() {
        assert_eq!(default_socket_path(), default_socket_path());
    }
}
