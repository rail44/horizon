//! Builds a nono `CapabilitySet` from a `SandboxPolicy`
//! (`docs/agent-approval-design.md`'s "Sandbox architecture"). Pure
//! capability-set construction -- no process spawning or sandbox
//! application here. Shared verbatim between both OS backends
//! (`docs/roadmap.md`'s backlog-60 entry): both production backends rebuild
//! and apply the same mapping inside `horizon-sandbox-helper`; Linux retains
//! an unsandboxed supervisor parent while macOS self-applies Seatbelt and
//! execs directly.
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
use crate::policy::{
    FilesystemGrant, FilesystemGrantAccess, FilesystemGrantScope, NetworkPolicy, ReadableScope,
    SandboxPolicy,
};
use nono::{AccessMode, CapabilitySet, NetworkMode, SignalMode};
use std::net::{Ipv4Addr, SocketAddr};
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
#[cfg(test)]
pub(crate) fn build(policy: &SandboxPolicy) -> Result<CapabilitySet, SandboxError> {
    build_with_grants(policy, &[])
}

pub(crate) fn build_with_grants(
    policy: &SandboxPolicy,
    filesystem_grants: &[FilesystemGrant],
) -> Result<CapabilitySet, SandboxError> {
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

    for grant in filesystem_grants {
        validate_grant(grant)?;
        let access = match grant.access {
            FilesystemGrantAccess::Read => AccessMode::Read,
            FilesystemGrantAccess::ReadWrite => AccessMode::ReadWrite,
        };
        caps = match grant.scope {
            FilesystemGrantScope::File => caps.allow_file(&grant.path, access)?,
            FilesystemGrantScope::DirectoryTree => caps.allow_path(&grant.path, access)?,
        };
        let applied = caps
            .fs_capabilities()
            .last()
            .expect("adding a filesystem capability must append one entry");
        if applied.resolved != grant.path || applied.access != access {
            return Err(SandboxError::GrantChanged {
                approved: grant.path.clone(),
                resolved: applied.resolved.clone(),
            });
        }
        if applied.is_file != (grant.scope == FilesystemGrantScope::File) {
            return Err(SandboxError::GrantTypeChanged {
                path: grant.path.clone(),
                scope: grant.scope,
            });
        }
    }

    caps = match &policy.network {
        NetworkPolicy::Disabled => caps.set_network_mode(NetworkMode::Blocked),
        NetworkPolicy::Proxied { proxy_addr } => caps.set_network_mode(NetworkMode::ProxyOnly {
            port: validated_proxy_port(*proxy_addr)?,
            bind_ports: Vec::new(),
        }),
    };

    Ok(caps.set_signal_mode(SignalMode::AllowSameSandbox))
}

pub(crate) fn validate_grant(grant: &FilesystemGrant) -> Result<(), SandboxError> {
    if crate::grant::is_protected(&grant.path)
        || (grant.access == FilesystemGrantAccess::ReadWrite
            && grant.scope == FilesystemGrantScope::DirectoryTree
            && grant.path == Path::new("/"))
    {
        return Err(SandboxError::UnsupportedGrantTarget(grant.path.clone()));
    }
    let resolved = grant
        .path
        .canonicalize()
        .map_err(|source| SandboxError::InvalidRoot {
            path: grant.path.clone(),
            source,
        })?;
    if resolved != grant.path {
        return Err(SandboxError::GrantChanged {
            approved: grant.path.clone(),
            resolved,
        });
    }
    if crate::grant::is_protected(&resolved) {
        return Err(SandboxError::UnsupportedGrantTarget(grant.path.clone()));
    }
    let metadata = resolved
        .metadata()
        .map_err(|source| SandboxError::InvalidRoot {
            path: grant.path.clone(),
            source,
        })?;
    let type_matches = match grant.scope {
        FilesystemGrantScope::File => metadata.is_file(),
        FilesystemGrantScope::DirectoryTree => metadata.is_dir(),
    };
    if !type_matches {
        return Err(SandboxError::GrantTypeChanged {
            path: grant.path.clone(),
            scope: grant.scope,
        });
    }
    Ok(())
}

fn validated_proxy_port(proxy_addr: SocketAddr) -> Result<u16, SandboxError> {
    match proxy_addr {
        SocketAddr::V4(addr) if *addr.ip() == Ipv4Addr::LOCALHOST && addr.port() != 0 => {
            Ok(addr.port())
        }
        _ => Err(SandboxError::InvalidProxyEndpoint(proxy_addr)),
    }
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
    fn approved_grants_cannot_bypass_protected_or_root_write_rules() {
        let base = policy(vec![], NetworkPolicy::Disabled);
        let protected = FilesystemGrant {
            path: Path::new("/proc").to_path_buf(),
            access: FilesystemGrantAccess::Read,
            scope: FilesystemGrantScope::DirectoryTree,
        };
        assert!(matches!(
            build_with_grants(&base, &[protected]),
            Err(SandboxError::UnsupportedGrantTarget(path)) if path == Path::new("/proc")
        ));

        let root_write = FilesystemGrant {
            path: Path::new("/").to_path_buf(),
            access: FilesystemGrantAccess::ReadWrite,
            scope: FilesystemGrantScope::DirectoryTree,
        };
        assert!(matches!(
            build_with_grants(&base, &[root_write]),
            Err(SandboxError::UnsupportedGrantTarget(path)) if path == Path::new("/")
        ));
    }

    #[test]
    fn network_proxied_allows_only_the_proxy_port() {
        let caps = build(&policy(
            vec![],
            NetworkPolicy::Proxied {
                proxy_addr: "127.0.0.1:43123".parse().unwrap(),
            },
        ))
        .unwrap();
        assert_eq!(
            *caps.network_mode(),
            NetworkMode::ProxyOnly {
                port: 43123,
                bind_ports: Vec::new(),
            }
        );
    }

    #[test]
    fn network_proxied_rejects_non_exact_loopback_endpoints() {
        for addr in ["127.0.0.2:43123", "[::1]:43123", "127.0.0.1:0"] {
            let addr = addr.parse().unwrap();
            assert!(matches!(
                build(&policy(vec![], NetworkPolicy::Proxied { proxy_addr: addr })),
                Err(SandboxError::InvalidProxyEndpoint(rejected)) if rejected == addr
            ));
        }
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
