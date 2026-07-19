//! Bubblewrap argv composition: the namespace/bind-mount half of the Linux
//! backend's containment (`docs/agent-approval-design.md`). Pure
//! argument-vector construction plus the filesystem probing needed to
//! decide symlink-vs-directory for the baseline system paths -- no
//! process spawning here, so the shape is unit-testable without a kernel
//! that supports user namespaces (see `tests` below); the real end-to-end
//! behavior is covered by `linux::tests` spawning actual `bwrap`.

use crate::error::SandboxError;
use crate::policy::{NetworkPolicy, ReadableScope, SandboxPolicy};
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Directories every backend needs read+execute access to just to run
/// *anything* (dynamic linker, libc, coreutils, shell). Bound read-only
/// regardless of `ReadableScope`; missing entries (e.g. no separate
/// `/lib64` on some layouts) are skipped rather than erroring, mirroring
/// Landlock's own `path_beneath_rules()` behavior for missing paths.
const BASELINE_DIRS: [&str; 6] = ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"];

/// Builds the full `bwrap` argument vector for `policy`, ending in `--`
/// followed by `program` and `args` (the command bwrap will exec once its
/// namespace/mounts are set up).
pub(crate) fn build_args(
    policy: &SandboxPolicy,
    program: &OsStr,
    args: &[OsString],
) -> Result<Vec<OsString>, SandboxError> {
    let mut argv: Vec<OsString> = Vec::new();
    let push = |argv: &mut Vec<OsString>, s: &str| argv.push(OsString::from(s));

    // `--unshare-all` already unshares the network namespace; `--share-net`
    // undoes just that one unshare when the policy allows network.
    push(&mut argv, "--unshare-all");
    if policy.network == NetworkPolicy::Enabled {
        push(&mut argv, "--share-net");
    }
    push(&mut argv, "--die-with-parent");
    push(&mut argv, "--new-session");

    match &policy.readable_scope {
        ReadableScope::Full => {
            bind_ro(&mut argv, Path::new("/"))?;
        }
        ReadableScope::Roots(roots) => {
            for dir in BASELINE_DIRS {
                bind_baseline_if_present(&mut argv, Path::new(dir))?;
            }
            for root in roots {
                bind_ro(&mut argv, root)?;
            }
        }
    }

    push(&mut argv, "--proc");
    push(&mut argv, "/proc");
    push(&mut argv, "--dev");
    push(&mut argv, "/dev");
    push(&mut argv, "--tmpfs");
    push(&mut argv, "/tmp");

    // Writable roots are bound *after* the read-only setup above so they
    // punch a read-write hole through it at their own (more specific)
    // path, the same layering `--ro-bind / /` followed by `--bind <sub>
    // <sub>` gets in a hand-written bwrap invocation.
    for root in &policy.writable_roots {
        bind_rw(&mut argv, root)?;
    }

    push(&mut argv, "--");
    argv.push(program.to_os_string());
    argv.extend(args.iter().cloned());

    Ok(argv)
}

/// Binds a baseline system path read-only, recreating it as a `--symlink`
/// when the host itself has it as a symlink (the common usr-merged layout:
/// `/bin -> usr/bin`) so the sandbox's tree matches host layout exactly.
/// Silently skipped if absent -- these are best-effort "make exec work"
/// paths, not explicit policy inputs.
fn bind_baseline_if_present(argv: &mut Vec<OsString>, path: &Path) -> Result<(), SandboxError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path).map_err(|source| SandboxError::InvalidRoot {
            path: path.to_path_buf(),
            source,
        })?;
        argv.push(OsString::from("--symlink"));
        argv.push(target.into_os_string());
        argv.push(path.as_os_str().to_os_string());
    } else if meta.is_dir() {
        bind_ro(argv, path)?;
    }
    Ok(())
}

/// Binds an explicit policy path read-only. Unlike the baseline dirs
/// above, these are caller-specified policy inputs, so a missing path is a
/// hard error rather than a silent skip.
fn bind_ro(argv: &mut Vec<OsString>, path: &Path) -> Result<(), SandboxError> {
    require_exists(path)?;
    argv.push(OsString::from("--ro-bind"));
    argv.push(path.as_os_str().to_os_string());
    argv.push(path.as_os_str().to_os_string());
    Ok(())
}

/// Binds an explicit policy path read-write (a `writable_root`). A missing
/// path is a hard error, same rationale as `bind_ro`.
fn bind_rw(argv: &mut Vec<OsString>, path: &Path) -> Result<(), SandboxError> {
    require_exists(path)?;
    argv.push(OsString::from("--bind"));
    argv.push(path.as_os_str().to_os_string());
    argv.push(path.as_os_str().to_os_string());
    Ok(())
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

    fn contains_flag(argv: &[OsString], flag: &str) -> bool {
        argv.iter().any(|a| a == flag)
    }

    fn policy(writable_roots: Vec<std::path::PathBuf>, network: NetworkPolicy) -> SandboxPolicy {
        SandboxPolicy {
            writable_roots,
            readable_scope: ReadableScope::Full,
            network,
        }
    }

    #[test]
    fn network_disabled_has_no_share_net() {
        let policy = policy(vec![], NetworkPolicy::Disabled);
        let argv = build_args(&policy, OsStr::new("/bin/true"), &[]).unwrap();
        assert!(!contains_flag(&argv, "--share-net"));
        assert!(contains_flag(&argv, "--unshare-all"));
    }

    #[test]
    fn network_enabled_adds_share_net() {
        let policy = policy(vec![], NetworkPolicy::Enabled);
        let argv = build_args(&policy, OsStr::new("/bin/true"), &[]).unwrap();
        assert!(contains_flag(&argv, "--share-net"));
    }

    #[test]
    fn full_scope_binds_root_read_only() {
        let policy = policy(vec![], NetworkPolicy::Disabled);
        let argv = build_args(&policy, OsStr::new("/bin/true"), &[]).unwrap();
        let idx = argv.iter().position(|a| a == "--ro-bind").unwrap();
        assert_eq!(argv[idx + 1], OsString::from("/"));
        assert_eq!(argv[idx + 2], OsString::from("/"));
    }

    #[test]
    fn writable_root_is_bound_read_write() {
        let dir = std::env::temp_dir();
        let policy = policy(vec![dir.clone()], NetworkPolicy::Disabled);
        let argv = build_args(&policy, OsStr::new("/bin/true"), &[]).unwrap();
        let idx = argv.iter().position(|a| a == "--bind").unwrap();
        assert_eq!(argv[idx + 1], OsString::from(dir.as_os_str()));
        assert_eq!(argv[idx + 2], OsString::from(dir.as_os_str()));
    }

    #[test]
    fn missing_writable_root_is_a_typed_error() {
        let policy = policy(
            vec![std::path::PathBuf::from(
                "/definitely/does/not/exist/horizon-sandbox",
            )],
            NetworkPolicy::Disabled,
        );
        let err = build_args(&policy, OsStr::new("/bin/true"), &[]).unwrap_err();
        assert!(matches!(err, SandboxError::InvalidRoot { .. }));
    }

    #[test]
    fn program_and_args_trail_the_separator() {
        let policy = policy(vec![], NetworkPolicy::Disabled);
        let argv = build_args(&policy, OsStr::new("/bin/echo"), &[OsString::from("hi")]).unwrap();
        let idx = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(argv[idx + 1], OsString::from("/bin/echo"));
        assert_eq!(argv[idx + 2], OsString::from("hi"));
    }
}
