//! Builds a nono `CapabilitySet` from a `SandboxPolicy`
//! (`docs/agent-approval-design.md`'s "Sandbox architecture"). Pure
//! capability-set construction -- no process spawning here (see
//! `linux::spawn` for how the built set is applied).
//!
//! nono grants nothing implicitly: every path a sandboxed command needs,
//! including the baseline system directories required just to exec
//! anything at all, must be requested explicitly (unlike bwrap, which
//! bind-mounted a fresh namespace and so needed those grants only for
//! `ReadableScope::Roots`). `BASELINE_DIRS` is the same list the old
//! `linux::bwrap` module's bind-mount composition used (now removed),
//! extended with `/dev`/`/proc` (which bwrap instead gave via its own
//! `--dev`/`--proc` fresh-mount flags, unconditionally) -- reused here as
//! the source of truth for what a `Roots`-scoped policy needs beyond its
//! own explicit roots; a `Full`-scoped policy's single `/` grant already
//! subsumes all of it.

use crate::error::SandboxError;
use crate::policy::{NetworkPolicy, ReadableScope, SandboxPolicy};
use nono::{AccessMode, CapabilitySet, NetworkMode, SignalMode};
use std::path::Path;

/// Directories every command needs read+execute (or, for `/proc`/`/dev`,
/// read) access to just to run *anything* (dynamic linker, libc,
/// coreutils, shell, procfs/devfs). Only consulted for
/// `ReadableScope::Roots`; a `Full` scope's `/` grant already covers all
/// of these. Missing entries (e.g. no separate `/lib64` on some layouts)
/// are skipped rather than erroring, mirroring bwrap's own
/// best-effort handling of the same list.
const BASELINE_DIRS: [&str; 8] = [
    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/dev", "/proc",
];

/// Builds the `CapabilitySet` for `policy`: readable scope, writable
/// roots, network mode, and signal scoping. `SignalMode::AllowSameSandbox`
/// is a new containment win over the old bwrap+seccompiler backend
/// (`docs/roadmap.md`'s backlog-60 entry): it scopes `kill(2)` to the
/// sandbox's own process tree on kernels with Landlock ABI V6, denying a
/// sandboxed command from signaling an external same-uid process.
pub(super) fn build(policy: &SandboxPolicy) -> Result<CapabilitySet, SandboxError> {
    let mut caps = CapabilitySet::new();

    match &policy.readable_scope {
        ReadableScope::Full => {
            caps = allow_dir(caps, Path::new("/"), AccessMode::Read)?;
        }
        ReadableScope::Roots(roots) => {
            for dir in BASELINE_DIRS {
                caps = allow_dir_if_present(caps, Path::new(dir), AccessMode::Read)?;
            }
            for root in roots {
                caps = allow_dir(caps, root, AccessMode::Read)?;
            }
        }
    }

    for root in &policy.writable_roots {
        caps = allow_dir(caps, root, AccessMode::ReadWrite)?;
    }

    caps = match &policy.network {
        NetworkPolicy::Enabled => caps.set_network_mode(NetworkMode::AllowAll),
        NetworkPolicy::Disabled => caps.set_network_mode(NetworkMode::Blocked),
        NetworkPolicy::Proxied { bridge_socket } => {
            let mut caps = caps.set_network_mode(NetworkMode::Blocked);
            // The proxy leg's only egress path (`docs/agent-approval-design.md`):
            // the bridge socket's containing directory must stay readable
            // so the child can `connect(2)` to it, even under a `Roots`
            // scope that otherwise wouldn't cover it. Spike-confirmed
            // (`experiments/nono-spike`'s Q2): plain directory Read access
            // is enough on Linux, no `allow_unix_socket`/Write grant
            // needed. Best-effort (silently skipped if the parent doesn't
            // exist yet) since `Full` scope already covers it and some
            // callers may not have created the socket's directory before
            // building the policy.
            if let Some(parent) = bridge_socket.parent() {
                caps = allow_dir_if_present(caps, parent, AccessMode::Read)?;
            }
            caps
        }
    };

    Ok(caps.set_signal_mode(SignalMode::AllowSameSandbox))
}

/// Grants `mode` on an explicit, caller-specified policy path (a
/// `writable_root` or a `ReadableScope::Roots` entry). A missing path is
/// a hard error -- these are policy inputs, not best-effort extras.
fn allow_dir(
    caps: CapabilitySet,
    path: &Path,
    mode: AccessMode,
) -> Result<CapabilitySet, SandboxError> {
    require_exists(path)?;
    Ok(caps.allow_path(path, mode)?)
}

/// Grants `mode` on a best-effort baseline system path, silently skipped
/// if absent (e.g. no separate `/lib64` on some layouts).
fn allow_dir_if_present(
    caps: CapabilitySet,
    path: &Path,
    mode: AccessMode,
) -> Result<CapabilitySet, SandboxError> {
    if std::fs::metadata(path).is_err() {
        return Ok(caps);
    }
    allow_dir(caps, path, mode)
}

fn require_exists(path: &Path) -> Result<(), SandboxError> {
    std::fs::metadata(path)
        .map(|_| ())
        .map_err(|source| SandboxError::InvalidRoot {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(writable_roots: Vec<std::path::PathBuf>, network: NetworkPolicy) -> SandboxPolicy {
        SandboxPolicy {
            writable_roots,
            readable_scope: ReadableScope::Full,
            network,
        }
    }

    #[test]
    fn network_disabled_blocks_network() {
        let caps = build(&policy(vec![], NetworkPolicy::Disabled)).unwrap();
        assert_eq!(*caps.network_mode(), NetworkMode::Blocked);
    }

    #[test]
    fn network_enabled_allows_all() {
        let caps = build(&policy(vec![], NetworkPolicy::Enabled)).unwrap();
        assert_eq!(*caps.network_mode(), NetworkMode::AllowAll);
    }

    #[test]
    fn network_proxied_blocks_network_and_grants_bridge_dir() {
        let socket_dir = std::env::temp_dir();
        let policy = policy(
            vec![],
            NetworkPolicy::Proxied {
                bridge_socket: socket_dir.join("bridge.sock"),
            },
        );
        let caps = build(&policy).unwrap();
        assert_eq!(*caps.network_mode(), NetworkMode::Blocked);
        assert!(
            caps.fs_capabilities()
                .iter()
                .any(|cap| cap.resolved == socket_dir.canonicalize().unwrap()),
            "the bridge socket's parent directory should be granted Read"
        );
    }

    #[test]
    fn full_scope_grants_root_read() {
        let caps = build(&policy(vec![], NetworkPolicy::Disabled)).unwrap();
        assert!(caps
            .fs_capabilities()
            .iter()
            .any(|cap| cap.resolved == Path::new("/").canonicalize().unwrap()
                && cap.access == AccessMode::Read));
    }

    #[test]
    fn writable_root_is_granted_read_write() {
        let dir = std::env::temp_dir();
        let caps = build(&policy(vec![dir.clone()], NetworkPolicy::Disabled)).unwrap();
        assert!(caps
            .fs_capabilities()
            .iter()
            .any(|cap| cap.resolved == dir.canonicalize().unwrap()
                && cap.access == AccessMode::ReadWrite));
    }

    #[test]
    fn missing_writable_root_is_a_typed_error() {
        let policy = policy(
            vec![std::path::PathBuf::from(
                "/definitely/does/not/exist/horizon-sandbox",
            )],
            NetworkPolicy::Disabled,
        );
        let err = build(&policy).unwrap_err();
        assert!(matches!(err, SandboxError::InvalidRoot { .. }));
    }

    #[test]
    fn signal_mode_is_always_allow_same_sandbox() {
        let caps = build(&policy(vec![], NetworkPolicy::Disabled)).unwrap();
        assert_eq!(caps.signal_mode(), SignalMode::AllowSameSandbox);
    }
}
