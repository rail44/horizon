//! Resolution and revalidation for approval-derived filesystem grants,
//! plus the generic shaping that turns one call's set of mediated attempts
//! into the grant an approver is actually offered
//! (`docs/containment-denial-narrow-grants-design.md`'s 2026-07-26
//! project-scoped-tree-grants decision).
//!
//! Everything here is generic over path shape. There is deliberately no
//! table mapping a command or language to the directories it needs -- the
//! only inputs are the attempted paths, the session's workspace root, and
//! `$HOME`.

use crate::{
    FilesystemDenial, FilesystemGrant, FilesystemGrantAccess, FilesystemGrantScope, SandboxError,
};
use std::path::{Component, Path, PathBuf};

const PROTECTED_PREFIXES: [&str; 3] = ["/proc", "/sys", "/dev"];

/// Directories that must never be handed out as a writable tree, however a
/// caller arrived at them -- an ancestor-rule result, a `[grants]` entry in
/// the config file, or a stored session grant being revalidated. Matched
/// *exactly*: `/usr` is refused, `/usr/local/share/some-project` is not.
/// Granting one of these as a tree would put the sandbox's whole reason for
/// existing inside the grant.
const SYSTEM_ROOTS: &[&str] = &[
    "/",
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/home",
    "/lib",
    "/lib32",
    "/lib64",
    "/libexec",
    "/mnt",
    "/opt",
    "/proc",
    "/root",
    "/run",
    "/sbin",
    "/srv",
    "/sys",
    "/tmp",
    "/usr",
    "/var",
    // macOS' own top level.
    "/Applications",
    "/Library",
    "/Network",
    "/System",
    "/Users",
    "/Volumes",
    "/private",
    "/cores",
];

/// Whether `path` is too broad to ever be a writable directory-tree grant:
/// the filesystem root, one of [`SYSTEM_ROOTS`], or `home` itself. A
/// relative path is refused too -- only canonical absolute paths are
/// enforceable.
///
/// `home` is threaded in rather than read from the environment here so the
/// rule stays a pure function (its callers in `horizon-config` validate a
/// file the user may be editing for a *different* account's paths, and
/// tests must not depend on the developer's real `$HOME`).
pub fn is_overbroad_tree(path: &Path, home: Option<&Path>) -> bool {
    if !path.is_absolute() {
        return true;
    }
    if home.is_some_and(|home| !home.as_os_str().is_empty() && path == home) {
        return true;
    }
    SYSTEM_ROOTS.iter().any(|root| path == Path::new(root))
}

/// This process's `$HOME`, for the callers of [`is_overbroad_tree`] that
/// legitimately mean "the account this daemon runs as" -- revalidating a
/// stored grant, and shaping a suggestion for a live session.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Re-checks a stored or proposed grant against the live filesystem:
/// still absolute, still canonical (nothing was swapped underneath it),
/// still the file-vs-directory kind it was approved as, still not
/// protected, and -- for a writable tree -- still not over-broad. Applied
/// to every session grant before it is merged into a fresh sandbox policy,
/// so a grant that stops being enforceable simply disappears instead of
/// silently widening.
pub fn revalidate_grant(grant: &FilesystemGrant) -> Result<(), SandboxError> {
    if !grant.path.is_absolute() || is_protected(&grant.path) {
        return Err(SandboxError::UnsupportedGrantTarget(grant.path.clone()));
    }
    if is_writable_tree(grant) && is_overbroad_tree(&grant.path, home_dir().as_deref()) {
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
    if is_protected(&resolved) {
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

pub(crate) fn is_writable_tree(grant: &FilesystemGrant) -> bool {
    grant.access == FilesystemGrantAccess::ReadWrite
        && grant.scope == FilesystemGrantScope::DirectoryTree
}

/// Shapes one call's mediated attempts into the grants to offer an
/// approver.
///
/// Attempts that landed *inside* the session's own workspace keep their
/// individually resolved grant -- the workspace is already writable, so
/// such an attempt is an oddity rather than the toolchain-reaching-outward
/// pattern this rule exists for. Every attempt *outside* the workspace is
/// generalized together into a single [`FilesystemGrantScope::DirectoryTree`]
/// at the narrowest common ancestor of their parent directories: a single
/// attempt therefore suggests its own parent directory, and several
/// attempts suggest the shallowest directory that contains all of them.
///
/// The result is clamped by [`is_overbroad_tree`]. When the common
/// ancestor lands on `$HOME`, `/`, or a system root -- which is what
/// happens when the attempts have nothing meaningful in common -- there is
/// no honest tree to offer, so the per-attempt grants are returned instead.
///
/// This is deliberately shape-only. `~/.cargo/.package-cache-mutate` and
/// `~/.cargo/horizon-build-dir/debug/.cargo-build-lock` generalize to
/// `~/.cargo` because that is their common ancestor, not because anything
/// here knows what Cargo is.
pub fn suggest_grants(
    denials: &[FilesystemDenial],
    workspace_root: Option<&Path>,
    home: Option<&Path>,
) -> Vec<FilesystemGrant> {
    let (outside, inside): (Vec<_>, Vec<_>) = denials.iter().partition(|denial| {
        workspace_root.is_none_or(|root| !denial.attempted_path.starts_with(root))
    });

    let mut grants = Vec::new();
    if let Some(shared) = shared_tree_grant(&outside, home) {
        grants.push(shared);
    } else {
        grants.extend(outside.iter().map(|denial| denial.grant.clone()));
    }
    grants.extend(inside.iter().map(|denial| denial.grant.clone()));
    grants.dedup();
    grants
}

/// The single tree grant covering every attempt in `denials`, or `None` if
/// no enforceable, non-over-broad ancestor exists (in which case the caller
/// falls back to the per-attempt grants).
fn shared_tree_grant(
    denials: &[&FilesystemDenial],
    home: Option<&Path>,
) -> Option<FilesystemGrant> {
    if denials.is_empty() {
        return None;
    }
    let parents = denials
        .iter()
        .map(|denial| nearest_existing_dir(denial.attempted_path.parent()?))
        .collect::<Option<Vec<_>>>()?;
    let ancestor = common_ancestor(&parents)?;
    if is_overbroad_tree(&ancestor, home) || is_protected(&ancestor) {
        return None;
    }
    let access = if denials
        .iter()
        .any(|denial| denial.grant.access == FilesystemGrantAccess::ReadWrite)
    {
        FilesystemGrantAccess::ReadWrite
    } else {
        FilesystemGrantAccess::Read
    };
    Some(FilesystemGrant {
        path: ancestor,
        access,
        scope: FilesystemGrantScope::DirectoryTree,
    })
}

/// Walks up from `path` to the first component that exists and is a
/// directory, canonicalized. A grant has to name something real, and an
/// attempt's own leaf usually does not exist yet (that is why it was
/// attempted).
fn nearest_existing_dir(path: &Path) -> Option<PathBuf> {
    let mut current = path.to_path_buf();
    loop {
        if let Ok(canonical) = current.canonicalize() {
            if canonical.is_dir() {
                return Some(canonical);
            }
        }
        if !current.pop() || current.as_os_str().is_empty() {
            return None;
        }
    }
}

/// The longest path prefix shared by every entry, component by component.
/// Purely lexical -- every input is already canonical, so there are no `..`
/// or symlink components left to reason about.
fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths.iter();
    let mut shared = iter.next()?.components().collect::<Vec<_>>();
    for path in iter {
        let candidate = path.components().collect::<Vec<_>>();
        let shared_len = shared
            .iter()
            .zip(candidate.iter())
            .take_while(|(left, right)| left == right)
            .count();
        shared.truncate(shared_len);
    }
    if shared.is_empty() {
        return None;
    }
    Some(shared.iter().collect())
}

pub(crate) fn resolve_denial(
    attempted_path: PathBuf,
    access: FilesystemGrantAccess,
) -> Result<FilesystemDenial, SandboxError> {
    if !attempted_path.is_absolute() || is_protected(&attempted_path) {
        return Err(SandboxError::UnsupportedGrantTarget(attempted_path));
    }
    let (path, scope) = match std::fs::symlink_metadata(&attempted_path) {
        Ok(_) => resolve_existing(&attempted_path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            resolve_missing_parent(&attempted_path, access)?
        }
        Err(source) => {
            return Err(SandboxError::InvalidRoot {
                path: attempted_path,
                source,
            });
        }
    };
    if is_protected(&path) {
        return Err(SandboxError::UnsupportedGrantTarget(attempted_path));
    }
    Ok(FilesystemDenial {
        attempted_path,
        grant: FilesystemGrant {
            path,
            access,
            scope,
        },
    })
}

pub fn revalidate_denial(denial: &FilesystemDenial) -> Result<(), SandboxError> {
    let current = resolve_denial(denial.attempted_path.clone(), denial.grant.access)?;
    if current == *denial {
        Ok(())
    } else {
        Err(SandboxError::GrantProposalChanged {
            attempted: denial.attempted_path.clone(),
        })
    }
}

fn resolve_existing(path: &Path) -> Result<(PathBuf, FilesystemGrantScope), SandboxError> {
    let canonical = path
        .canonicalize()
        .map_err(|source| SandboxError::InvalidRoot {
            path: path.to_path_buf(),
            source,
        })?;
    let metadata = canonical
        .metadata()
        .map_err(|source| SandboxError::InvalidRoot {
            path: canonical.clone(),
            source,
        })?;
    if metadata.is_file() {
        Ok((canonical, FilesystemGrantScope::File))
    } else if metadata.is_dir() {
        Ok((canonical, FilesystemGrantScope::DirectoryTree))
    } else {
        Err(SandboxError::UnsupportedGrantTarget(path.to_path_buf()))
    }
}

fn resolve_missing_parent(
    attempted: &Path,
    access: FilesystemGrantAccess,
) -> Result<(PathBuf, FilesystemGrantScope), SandboxError> {
    let mut current = attempted.to_path_buf();
    loop {
        let Some(name) = current.file_name() else {
            return Err(SandboxError::UnsupportedGrantTarget(
                attempted.to_path_buf(),
            ));
        };
        if !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        ) {
            return Err(SandboxError::UnsupportedGrantTarget(
                attempted.to_path_buf(),
            ));
        }
        if !current.pop() {
            return Err(SandboxError::UnsupportedGrantTarget(
                attempted.to_path_buf(),
            ));
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                let canonical =
                    current
                        .canonicalize()
                        .map_err(|source| SandboxError::InvalidRoot {
                            path: current.clone(),
                            source,
                        })?;
                let target_is_dir = if metadata.file_type().is_symlink() {
                    canonical.is_dir()
                } else {
                    metadata.is_dir()
                };
                if !target_is_dir
                    || (access == FilesystemGrantAccess::ReadWrite && canonical == Path::new("/"))
                {
                    return Err(SandboxError::UnsupportedGrantTarget(
                        attempted.to_path_buf(),
                    ));
                }
                return Ok((canonical, FilesystemGrantScope::DirectoryTree));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SandboxError::InvalidRoot {
                    path: current,
                    source,
                });
            }
        }
    }
}

pub(crate) fn is_protected(path: &Path) -> bool {
    PROTECTED_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn existing_regular_file_proposes_only_that_file() {
        let root = test_dir("existing-file");
        let target = root.join("target.txt");
        fs::write(&target, "before").expect("create target");

        let denial = resolve_denial(target.clone(), FilesystemGrantAccess::ReadWrite)
            .expect("resolve existing file");

        assert_eq!(denial.attempted_path, target);
        assert_eq!(
            denial.grant.path,
            denial.attempted_path.canonicalize().unwrap()
        );
        assert_eq!(denial.grant.scope, FilesystemGrantScope::File);
        assert_eq!(denial.grant.access, FilesystemGrantAccess::ReadWrite);
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn missing_leaf_proposes_nearest_existing_parent_tree() {
        let root = test_dir("missing-leaf");
        let existing = root.join("existing");
        fs::create_dir(&existing).expect("create existing parent");
        let attempted = existing.join("new").join("leaf.txt");

        let denial = resolve_denial(attempted.clone(), FilesystemGrantAccess::ReadWrite)
            .expect("resolve missing leaf");

        assert_eq!(denial.attempted_path, attempted);
        assert_eq!(denial.grant.path, existing.canonicalize().unwrap());
        assert_eq!(denial.grant.scope, FilesystemGrantScope::DirectoryTree);
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn missing_path_with_parent_component_is_not_grantable() {
        let root = test_dir("parent-component");
        let attempted = root.join("missing").join("..").join("leaf");

        assert!(matches!(
            resolve_denial(attempted.clone(), FilesystemGrantAccess::ReadWrite),
            Err(SandboxError::UnsupportedGrantTarget(path)) if path == attempted
        ));
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn writable_root_directory_is_never_proposed() {
        let attempted = PathBuf::from(format!(
            "/horizon-missing-grant-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        assert!(!attempted.exists());
        assert!(matches!(
            resolve_denial(attempted.clone(), FilesystemGrantAccess::ReadWrite),
            Err(SandboxError::UnsupportedGrantTarget(path)) if path == attempted
        ));
    }

    #[cfg(unix)]
    #[test]
    fn protected_symlink_target_is_not_grantable() {
        use std::os::unix::fs::symlink;

        let root = test_dir("protected-symlink");
        let attempted = root.join("proc-link");
        symlink("/proc", &attempted).expect("create symlink");

        assert!(matches!(
            resolve_denial(attempted.clone(), FilesystemGrantAccess::Read),
            Err(SandboxError::UnsupportedGrantTarget(path)) if path == attempted
        ));
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[cfg(unix)]
    #[test]
    fn approval_revalidation_detects_symlink_retargeting() {
        use std::os::unix::fs::symlink;

        let root = test_dir("retarget");
        let first = root.join("first.txt");
        let second = root.join("second.txt");
        let attempted = root.join("current.txt");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        symlink(&first, &attempted).unwrap();
        let denial = resolve_denial(attempted.clone(), FilesystemGrantAccess::ReadWrite).unwrap();

        fs::remove_file(&attempted).unwrap();
        symlink(&second, &attempted).unwrap();

        assert!(matches!(
            revalidate_denial(&denial),
            Err(SandboxError::GrantProposalChanged { attempted: path }) if path == attempted
        ));
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[cfg(unix)]
    #[test]
    fn approval_revalidation_detects_new_symlink_in_missing_suffix() {
        use std::os::unix::fs::symlink;

        let root = test_dir("new-symlink");
        let outside = test_dir("new-symlink-target");
        let link = root.join("future");
        let attempted = link.join("leaf.txt");
        let denial = resolve_denial(attempted.clone(), FilesystemGrantAccess::ReadWrite).unwrap();
        assert_eq!(denial.grant.path, root.canonicalize().unwrap());

        symlink(&outside, &link).unwrap();

        assert!(matches!(
            revalidate_denial(&denial),
            Err(SandboxError::GrantProposalChanged { attempted: path }) if path == attempted
        ));
        fs::remove_dir_all(root).expect("remove test directory");
        fs::remove_dir_all(outside).expect("remove test directory");
    }

    // --- the over-broad-tree clamp ----------------------------------------

    #[test]
    fn the_filesystem_root_home_and_system_roots_are_never_writable_trees() {
        let home = PathBuf::from("/home/someone");
        for refused in [
            "/",
            "/home/someone",
            "/usr",
            "/etc",
            "/var",
            "/home",
            "/tmp",
            "/Users",
            "/Library",
        ] {
            assert!(
                is_overbroad_tree(Path::new(refused), Some(&home)),
                "{refused} must be refused as a writable tree"
            );
        }
    }

    #[test]
    fn a_directory_below_a_system_root_or_home_is_grantable() {
        let home = PathBuf::from("/home/someone");
        for allowed in [
            "/home/someone/.cargo",
            "/home/someone/src/project",
            "/usr/local/share/project",
            "/tmp/horizon-scratch",
        ] {
            assert!(
                !is_overbroad_tree(Path::new(allowed), Some(&home)),
                "{allowed} must stay grantable"
            );
        }
    }

    #[test]
    fn a_relative_path_is_never_grantable_as_a_tree() {
        assert!(is_overbroad_tree(Path::new("relative/dir"), None));
    }

    #[test]
    fn an_absent_home_does_not_accidentally_match_an_empty_path() {
        assert!(!is_overbroad_tree(Path::new("/home/someone"), None));
        assert!(!is_overbroad_tree(
            Path::new("/home/someone"),
            Some(Path::new(""))
        ));
    }

    // --- grant revalidation ------------------------------------------------

    #[test]
    fn revalidating_a_live_directory_tree_grant_succeeds() {
        let root = test_dir("revalidate-tree");
        let grant = FilesystemGrant {
            path: root.canonicalize().unwrap(),
            access: FilesystemGrantAccess::ReadWrite,
            scope: FilesystemGrantScope::DirectoryTree,
        };
        assert!(revalidate_grant(&grant).is_ok());
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn revalidating_drops_a_grant_whose_target_disappeared() {
        let root = test_dir("revalidate-missing");
        let grant = FilesystemGrant {
            path: root.canonicalize().unwrap(),
            access: FilesystemGrantAccess::ReadWrite,
            scope: FilesystemGrantScope::DirectoryTree,
        };
        fs::remove_dir_all(&root).expect("remove test directory");
        assert!(matches!(
            revalidate_grant(&grant),
            Err(SandboxError::InvalidRoot { .. })
        ));
    }

    #[test]
    fn revalidating_drops_a_grant_whose_target_changed_kind() {
        let root = test_dir("revalidate-kind");
        let target = root.join("target");
        fs::create_dir(&target).expect("create target directory");
        let grant = FilesystemGrant {
            path: target.canonicalize().unwrap(),
            access: FilesystemGrantAccess::ReadWrite,
            scope: FilesystemGrantScope::DirectoryTree,
        };
        assert!(revalidate_grant(&grant).is_ok());

        fs::remove_dir(&target).expect("remove target directory");
        fs::write(&target, "now a file").expect("replace with a file");

        assert!(matches!(
            revalidate_grant(&grant),
            Err(SandboxError::GrantTypeChanged { .. })
        ));
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn revalidating_refuses_an_overbroad_writable_tree() {
        let grant = FilesystemGrant {
            path: PathBuf::from("/usr"),
            access: FilesystemGrantAccess::ReadWrite,
            scope: FilesystemGrantScope::DirectoryTree,
        };
        assert!(matches!(
            revalidate_grant(&grant),
            Err(SandboxError::UnsupportedGrantTarget(path)) if path == Path::new("/usr")
        ));
    }

    // --- the ancestor rule -------------------------------------------------

    #[test]
    fn a_single_outside_attempt_suggests_its_parent_directory() {
        let root = test_dir("ancestor-single");
        let cache = root.join("cache");
        fs::create_dir(&cache).expect("create cache directory");
        let attempted = cache.join("lock-file");

        let grants = suggest_grants(
            &[denial(&attempted)],
            Some(Path::new("/workspace-that-does-not-contain-it")),
            None,
        );

        assert_eq!(
            grants,
            vec![FilesystemGrant {
                path: cache.canonicalize().unwrap(),
                access: FilesystemGrantAccess::ReadWrite,
                scope: FilesystemGrantScope::DirectoryTree,
            }]
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn several_outside_attempts_generalize_to_their_narrowest_common_ancestor() {
        let root = test_dir("ancestor-multi");
        let cache = root.join("cache");
        let nested = cache.join("build").join("debug");
        fs::create_dir_all(&nested).expect("create nested directories");

        // Deliberately the issue-009 shape: one lock beside the cache root
        // and one buried several levels down under it.
        let grants = suggest_grants(
            &[
                denial(&cache.join(".package-cache-mutate")),
                denial(&nested.join(".build-lock")),
            ],
            Some(Path::new("/workspace-that-does-not-contain-it")),
            None,
        );

        assert_eq!(
            grants,
            vec![FilesystemGrant {
                path: cache.canonicalize().unwrap(),
                access: FilesystemGrantAccess::ReadWrite,
                scope: FilesystemGrantScope::DirectoryTree,
            }],
            "two attempts under one cache directory must collapse to that directory"
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn an_ancestor_landing_on_home_falls_back_to_per_attempt_grants() {
        let home = test_dir("ancestor-home");
        let first = home.join("alpha");
        let second = home.join("beta");
        fs::create_dir(&first).expect("create first directory");
        fs::create_dir(&second).expect("create second directory");
        let attempts = [first.join("leaf"), second.join("leaf")];
        let denials = attempts.iter().map(|path| denial(path)).collect::<Vec<_>>();

        let grants = suggest_grants(&denials, None, Some(&home.canonicalize().unwrap()));

        assert_eq!(
            grants,
            denials
                .iter()
                .map(|denial| denial.grant.clone())
                .collect::<Vec<_>>(),
            "a common ancestor at $HOME is not an honest offer"
        );
        fs::remove_dir_all(home).expect("remove test directory");
    }

    #[test]
    fn attempts_with_nothing_in_common_fall_back_to_per_attempt_grants() {
        let first = test_dir("ancestor-disjoint-a");
        let second = test_dir("ancestor-disjoint-b");
        // The only shared ancestor of two temp-dir siblings is the temp
        // directory's own parent chain, which bottoms out in a system root.
        let denials = vec![denial(&first.join("leaf")), denial(&second.join("leaf"))];

        let grants = suggest_grants(&denials, None, None);

        assert!(
            grants.len() >= 2,
            "no shared tree exists, so each attempt keeps its own grant: {grants:?}"
        );
        assert!(grants
            .iter()
            .all(|grant| !is_overbroad_tree(&grant.path, None)));
        fs::remove_dir_all(first).expect("remove test directory");
        fs::remove_dir_all(second).expect("remove test directory");
    }

    #[test]
    fn attempts_inside_the_workspace_keep_their_own_resolved_grant() {
        let workspace = test_dir("ancestor-inside");
        let inside = workspace.join("inside.txt");
        fs::write(&inside, "content").expect("create in-workspace file");

        let grants = suggest_grants(
            &[denial(&inside)],
            Some(&workspace.canonicalize().unwrap()),
            None,
        );

        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].scope, FilesystemGrantScope::File);
        assert_eq!(grants[0].path, inside.canonicalize().unwrap());
        fs::remove_dir_all(workspace).expect("remove test directory");
    }

    #[test]
    fn no_denials_suggest_nothing() {
        assert!(suggest_grants(&[], None, None).is_empty());
    }

    fn denial(attempted: &Path) -> FilesystemDenial {
        resolve_denial(attempted.to_path_buf(), FilesystemGrantAccess::ReadWrite)
            .expect("resolve test denial")
    }

    fn test_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "horizon-grant-{label}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir(&path).expect("create test directory");
        path
    }

    fn unique_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    }
}
