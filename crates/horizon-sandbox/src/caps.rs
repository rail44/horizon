//! Builds a nono `CapabilitySet` from a `SandboxPolicy`
//! (`docs/agent-approval-design.md`'s "Sandbox architecture"). Pure
//! capability-set construction -- no process spawning or sandbox
//! application here. Shared verbatim between both OS backends
//! (`docs/roadmap.md`'s backlog-60 entry): `linux::spawn` applies the
//! built set directly to its spawning thread; the macOS backend applies
//! the same mapping inside a tiny exec helper (`src/bin/
//! horizon-sandbox-helper.rs`), since nono's macOS `Sandbox::apply_auto`
//! restricts the *whole calling process* rather than a single thread (see
//! `macos/mod.rs`'s module doc for why that forces a separate-process
//! design there).
//!
//! nono grants nothing implicitly on either OS: every path a sandboxed
//! command needs, including the baseline system directories required
//! just to exec anything at all, must be requested explicitly (unlike
//! bwrap, which bind-mounted a fresh namespace and so needed those grants
//! only for `ReadableScope::Roots`). `BASELINE_DIRS` differs per OS --
//! see each cfg'd definition below for its source of truth -- and is only
//! consulted for `ReadableScope::Roots`; a `Full`-scoped policy's single
//! `/` grant already subsumes all of it.

use crate::error::SandboxError;
use crate::policy::{NetworkPolicy, ReadableScope, SandboxPolicy};
#[cfg(target_os = "macos")]
use nono::UnixSocketMode;
use nono::{AccessMode, CapabilitySet, NetworkMode, SignalMode};
use std::path::Path;

/// Directories every command needs read (or, for `/proc`/`/dev`, read)
/// access to just to run *anything* (dynamic linker, libc, coreutils,
/// shell, procfs/devfs). This is the same list the old `linux::bwrap`
/// module's bind-mount composition used, extended with `/dev`/`/proc`
/// (which bwrap instead gave via its own `--dev`/`--proc` fresh-mount
/// flags, unconditionally). Missing entries (e.g. no separate `/lib64` on
/// some layouts) are skipped rather than erroring, mirroring bwrap's own
/// best-effort handling of the same list.
#[cfg(target_os = "linux")]
const BASELINE_DIRS: [&str; 8] = [
    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/dev", "/proc",
];

/// macOS's equivalent baseline: standard system binary/library trees,
/// `/System` and `/Library` (frameworks, app-level resources, system
/// preferences), `/private/etc` (macOS's real `/etc` -- the top-level
/// `/etc` is itself a symlink to it), and `/dev`. Sourced from what the
/// old vendored SBPL templates (`restricted_read_only_platform_defaults.sbpl`,
/// deleted with the sandbox-exec backend -- see `docs/roadmap.md`'s
/// backlog-60 entry) granted read access to. macOS has no top-level
/// `/lib`; `/usr/lib` is the real location.
#[cfg(target_os = "macos")]
const BASELINE_DIRS: [&str; 8] = [
    "/usr",
    "/bin",
    "/sbin",
    "/usr/lib",
    "/System",
    "/Library",
    "/private/etc",
    "/dev",
];

/// Builds the `CapabilitySet` for `policy`: readable scope, writable
/// roots, network mode, and signal scoping. `SignalMode::AllowSameSandbox`
/// scopes `kill(2)` to the sandbox's own process tree on both OS backends
/// (`docs/roadmap.md`'s backlog-60 entry), denying a sandboxed command
/// from signaling an external same-uid process.
pub(crate) fn build(policy: &SandboxPolicy) -> Result<CapabilitySet, SandboxError> {
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
            let caps = caps.set_network_mode(NetworkMode::Blocked);
            grant_proxy_bridge_socket(caps, bridge_socket)?
        }
    };

    Ok(caps.set_signal_mode(SignalMode::AllowSameSandbox))
}

/// Grants the sandboxed command's only egress path under `Proxied`: the
/// bridge socket to the allowlist proxy. Best-effort (silently skipped if
/// the socket's parent directory doesn't exist yet -- some callers build
/// the policy before creating the proxy socket); `Full` scope already
/// covers it either way.
///
/// Directory Read access is enough on Linux (spike-confirmed,
/// `experiments/nono-spike`'s Q2) -- nono's `allow_unix_socket*` API is
/// inert on the Landlock path there, so a generic filesystem grant is
/// what actually permits `connect(2)`.
#[cfg(target_os = "linux")]
fn grant_proxy_bridge_socket(
    caps: CapabilitySet,
    bridge_socket: &Path,
) -> Result<CapabilitySet, SandboxError> {
    match bridge_socket.parent() {
        Some(parent) => allow_dir_if_present(caps, parent, AccessMode::Read),
        None => Ok(caps),
    }
}

/// macOS needs the opposite of Linux's grant here: nono's Seatbelt backend
/// treats a generic `FsCapability` as granting no network access at all
/// (`emit_unix_socket_rules` in `nono-0.68.0/src/sandbox/macos.rs` only
/// emits `network-outbound`/`network-bind` rules for explicit
/// `UnixSocketCapability` grants -- upstream issue #696). A directory-scoped
/// `Connect`-mode grant on the socket's parent mirrors Linux's
/// "parent-directory, best-effort" shape without requiring the socket file
/// to already exist at policy-build time (a `File`-scoped `Connect` grant
/// would, per nono's own `UnixSocketCapability::new_file` doc).
#[cfg(target_os = "macos")]
fn grant_proxy_bridge_socket(
    caps: CapabilitySet,
    bridge_socket: &Path,
) -> Result<CapabilitySet, SandboxError> {
    let Some(parent) = bridge_socket.parent() else {
        return Ok(caps);
    };
    if std::fs::metadata(parent).is_err() {
        return Ok(caps);
    }
    Ok(caps.allow_unix_socket_dir(parent, UnixSocketMode::Connect)?)
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
    fn network_proxied_blocks_network() {
        let caps = build(&policy(
            vec![],
            NetworkPolicy::Proxied {
                bridge_socket: std::env::temp_dir().join("bridge.sock"),
            },
        ))
        .unwrap();
        assert_eq!(*caps.network_mode(), NetworkMode::Blocked);
    }

    /// Linux-specific expectation: the bridge socket's parent directory is
    /// granted as a plain filesystem Read capability (see
    /// `grant_proxy_bridge_socket`'s Linux arm doc).
    #[cfg(target_os = "linux")]
    #[test]
    fn network_proxied_grants_bridge_dir_read_on_linux() {
        let socket_dir = std::env::temp_dir();
        let policy = policy(
            vec![],
            NetworkPolicy::Proxied {
                bridge_socket: socket_dir.join("bridge.sock"),
            },
        );
        let caps = build(&policy).unwrap();
        assert!(
            caps.fs_capabilities()
                .iter()
                .any(|cap| cap.resolved == socket_dir.canonicalize().unwrap()),
            "the bridge socket's parent directory should be granted Read"
        );
    }

    /// macOS-specific expectation: the bridge socket's parent directory is
    /// granted as a directory-scoped `UnixSocketCapability` in `Connect`
    /// mode (see `grant_proxy_bridge_socket`'s macOS arm doc). Cannot run
    /// on this (Linux) development host -- compile-checked only via
    /// `cargo check --target x86_64-apple-darwin --tests`.
    #[cfg(target_os = "macos")]
    #[test]
    fn network_proxied_grants_bridge_dir_as_unix_socket_connect_on_macos() {
        let socket_dir = std::env::temp_dir();
        let policy = policy(
            vec![],
            NetworkPolicy::Proxied {
                bridge_socket: socket_dir.join("bridge.sock"),
            },
        );
        let caps = build(&policy).unwrap();
        assert!(
            caps.unix_socket_capabilities().iter().any(|cap| {
                cap.resolved == socket_dir.canonicalize().unwrap()
                    && cap.mode == UnixSocketMode::Connect
            }),
            "the bridge socket's parent directory should be granted a Connect-mode unix socket capability"
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

    /// Roots scope grants every baseline dir that exists on this host, in
    /// addition to the caller's own explicit roots. Only exercises the
    /// Linux baseline list (this test's own host) -- see the macOS-cfg'd
    /// sibling below for why the macOS list can't be checked the same way
    /// here.
    #[cfg(target_os = "linux")]
    #[test]
    fn roots_scope_grants_linux_baseline_dirs_that_exist() {
        let mut p = policy(vec![], NetworkPolicy::Disabled);
        p.readable_scope = ReadableScope::Roots(vec!["/opt".into()]);
        let caps = build(&p).unwrap();
        let resolved: Vec<_> = caps.fs_capabilities().iter().map(|c| &c.resolved).collect();
        for dir in ["/usr", "/bin", "/etc"] {
            assert!(
                resolved.contains(&&Path::new(dir).canonicalize().unwrap()),
                "expected baseline dir {dir} to be granted, got {resolved:?}"
            );
        }
    }

    /// The macOS baseline list can't be existence-checked from this Linux
    /// host (none of its paths exist here), so this only asserts the list
    /// contents directly rather than building real capabilities from it --
    /// still compile-checked cross-target, still catches a typo'd path.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_baseline_dirs_are_the_documented_set() {
        assert_eq!(
            BASELINE_DIRS,
            [
                "/usr",
                "/bin",
                "/sbin",
                "/usr/lib",
                "/System",
                "/Library",
                "/private/etc",
                "/dev",
            ]
        );
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
