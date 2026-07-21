//! Resolution and revalidation for approval-derived filesystem grants.

use crate::{
    FilesystemDenial, FilesystemGrant, FilesystemGrantAccess, FilesystemGrantScope, SandboxError,
};
use std::path::{Component, Path, PathBuf};

const PROTECTED_PREFIXES: [&str; 3] = ["/proc", "/sys", "/dev"];

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
