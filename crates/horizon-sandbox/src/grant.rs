//! Filesystem grants: a tree or file a session's contained calls may also
//! read or write (`[[grants.project]] trees`, a session's own worktree
//! metadata), and the checks that keep one from widening into what the
//! sandbox exists to protect.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What a grant adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum FilesystemGrantAccess {
    Read,
    ReadWrite,
}

/// Whether a grant names one file or a directory and everything under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum FilesystemGrantScope {
    File,
    DirectoryTree,
}

/// A canonical filesystem grant. `excluded_subpaths` names subpaths within
/// a `DirectoryTree` grant that stay read-only though the tree is writable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct FilesystemGrant {
    pub path: PathBuf,
    pub access: FilesystemGrantAccess,
    pub scope: FilesystemGrantScope,
    #[serde(default)]
    pub excluded_subpaths: Vec<PathBuf>,
}

/// Why a grant cannot be applied as it stands.
#[derive(Debug, thiserror::Error)]
pub enum GrantError {
    #[error("{path} cannot be granted: {source}")]
    InvalidRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the grant for {approved} now resolves to {resolved}")]
    GrantChanged {
        approved: PathBuf,
        resolved: PathBuf,
    },
    #[error("{path} is no longer a {scope:?}")]
    GrantTypeChanged {
        path: PathBuf,
        scope: FilesystemGrantScope,
    },
    #[error("{0} is not a grantable target")]
    UnsupportedGrantTarget(PathBuf),
}

const PROTECTED_PREFIXES: [&str; 3] = ["/proc", "/sys", "/dev"];

/// Directories that must never be handed out as a writable tree, however a
/// caller arrived at them -- an ancestor-rule result, a `[grants]` entry in
/// the config file, or a stored session grant being revalidated. Matched
/// *exactly*: `/usr` is refused, `/usr/local/share/some-project` is not.
/// Granting one of these as a tree would put the sandbox's whole reason for
/// existing inside the grant. Each OS has its own list: the paths a
/// canonical grant can name differ (macOS' `/tmp` is `/private/tmp`).
#[cfg(not(target_os = "macos"))]
const SYSTEM_ROOTS: &[&str] = &[
    "/", "/bin", "/boot", "/dev", "/etc", "/home", "/lib", "/lib32", "/lib64", "/libexec", "/mnt",
    "/opt", "/proc", "/root", "/run", "/sbin", "/srv", "/sys", "/tmp", "/usr", "/var",
];

#[cfg(target_os = "macos")]
const SYSTEM_ROOTS: &[&str] = &[
    "/",
    "/Applications",
    "/Library",
    "/Network",
    "/System",
    "/Users",
    "/Volumes",
    "/bin",
    "/cores",
    "/dev",
    "/opt",
    "/sbin",
    "/usr",
    // The firmlinks to /private and their targets, which canonical paths name.
    "/etc",
    "/tmp",
    "/var",
    "/private",
    "/private/etc",
    "/private/tmp",
    "/private/var",
];

/// macOS' per-user temporary and cache directories,
/// `/private/var/folders/<a>/<b>/{T,C,0,X}`: each account's equivalent of
/// `/tmp`. The directory itself and its ancestors are over-broad; what lies
/// inside it is grantable, as below `/tmp`.
#[cfg(target_os = "macos")]
fn is_per_user_temp_root(path: &Path) -> bool {
    ["/private/var/folders", "/var/folders"].iter().any(|base| {
        path.starts_with(base) && path.components().count() <= base.matches('/').count() + 4
    })
}

/// Whether `path` is too broad to ever be a writable directory-tree grant:
/// the filesystem root, one of [`SYSTEM_ROOTS`] (on macOS also a per-user
/// temporary directory or one of its ancestors), or `home` itself. A
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
    #[cfg(target_os = "macos")]
    if is_per_user_temp_root(path) {
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
pub fn revalidate_grant(grant: &FilesystemGrant) -> Result<(), GrantError> {
    if !grant.path.is_absolute() {
        return Err(GrantError::UnsupportedGrantTarget(grant.path.clone()));
    }
    validate_grant(grant)
}

/// Shared identity, scope and breadth checks for retained and applied grants.
fn validate_grant(grant: &FilesystemGrant) -> Result<(), GrantError> {
    if is_protected(&grant.path) {
        return Err(GrantError::UnsupportedGrantTarget(grant.path.clone()));
    }
    if is_writable_tree(grant) && is_overbroad_tree(&grant.path, home_dir().as_deref()) {
        return Err(GrantError::UnsupportedGrantTarget(grant.path.clone()));
    }
    let resolved = grant
        .path
        .canonicalize()
        .map_err(|source| GrantError::InvalidRoot {
            path: grant.path.clone(),
            source,
        })?;
    if resolved != grant.path {
        return Err(GrantError::GrantChanged {
            approved: grant.path.clone(),
            resolved,
        });
    }
    if is_protected(&resolved) {
        return Err(GrantError::UnsupportedGrantTarget(grant.path.clone()));
    }
    let metadata = resolved
        .metadata()
        .map_err(|source| GrantError::InvalidRoot {
            path: grant.path.clone(),
            source,
        })?;
    let type_matches = match grant.scope {
        FilesystemGrantScope::File => metadata.is_file(),
        FilesystemGrantScope::DirectoryTree => metadata.is_dir(),
    };
    if !type_matches {
        return Err(GrantError::GrantTypeChanged {
            path: grant.path.clone(),
            scope: grant.scope,
        });
    }
    Ok(())
}

fn is_writable_tree(grant: &FilesystemGrant) -> bool {
    grant.access == FilesystemGrantAccess::ReadWrite
        && grant.scope == FilesystemGrantScope::DirectoryTree
}

fn is_protected(path: &Path) -> bool {
    PROTECTED_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // --- the over-broad-tree clamp ----------------------------------------

    #[test]
    fn the_filesystem_root_home_and_system_roots_are_never_writable_trees() {
        let home = PathBuf::from("/home/someone");
        #[cfg(not(target_os = "macos"))]
        let refused_here = ["/home", "/proc", "/run"];
        #[cfg(target_os = "macos")]
        let refused_here = ["/Users", "/Library", "/private/tmp"];
        for refused in ["/", "/home/someone", "/usr", "/etc", "/var", "/tmp"]
            .into_iter()
            .chain(refused_here)
        {
            assert!(
                is_overbroad_tree(Path::new(refused), Some(&home)),
                "{refused} must be refused as a writable tree"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_per_user_temp_directory_and_its_ancestors_are_never_writable_trees() {
        for refused in [
            "/private/var/folders",
            "/private/var/folders/jw",
            "/private/var/folders/jw/95t21_s11g5c2l_g360k4kf80000gn",
            "/private/var/folders/jw/95t21_s11g5c2l_g360k4kf80000gn/T",
            "/var/folders/jw/95t21_s11g5c2l_g360k4kf80000gn/T",
        ] {
            assert!(
                is_overbroad_tree(Path::new(refused), None),
                "{refused} must be refused as a writable tree"
            );
        }
        assert!(!is_overbroad_tree(
            Path::new("/private/var/folders/jw/95t21_s11g5c2l_g360k4kf80000gn/T/horizon-scratch"),
            None
        ));
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
            excluded_subpaths: Vec::new(),
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
            excluded_subpaths: Vec::new(),
        };
        fs::remove_dir_all(&root).expect("remove test directory");
        std::assert_matches!(
            revalidate_grant(&grant),
            Err(GrantError::InvalidRoot { .. })
        );
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
            excluded_subpaths: Vec::new(),
        };
        assert!(revalidate_grant(&grant).is_ok());

        fs::remove_dir(&target).expect("remove target directory");
        fs::write(&target, "now a file").expect("replace with a file");

        std::assert_matches!(
            revalidate_grant(&grant),
            Err(GrantError::GrantTypeChanged { .. })
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn revalidating_refuses_an_overbroad_writable_tree() {
        let grant = FilesystemGrant {
            path: PathBuf::from("/usr"),
            access: FilesystemGrantAccess::ReadWrite,
            scope: FilesystemGrantScope::DirectoryTree,
            excluded_subpaths: Vec::new(),
        };
        std::assert_matches!(
            revalidate_grant(&grant),
            Err(GrantError::UnsupportedGrantTarget(path)) if path == Path::new("/usr"));
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
