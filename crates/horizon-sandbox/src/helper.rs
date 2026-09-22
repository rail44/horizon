//! Resolution shared by the Linux supervisor and macOS Seatbelt helpers.

use crate::SandboxError;
use std::{path::PathBuf, sync::OnceLock};

pub(crate) const HELPER_BIN_NAME: &str = "horizon-sandbox-helper";

// Unreferenced under cfg(test): only the production spawn_with_grants
// (cfg(not(test)) in linux/mod.rs) calls this, but the module is compiled
// under test for the unit tests below.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn resolve() -> Result<PathBuf, SandboxError> {
    resolve_from(
        std::env::var_os("CARGO_BIN_EXE_horizon-sandbox-helper").map(PathBuf::from),
        std::env::current_exe().ok(),
        std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from),
        std::env::var_os("PATH"),
    )
}

/// Environment capture is separate so precedence can be checked without
/// changing process-global variables. Probes stay lazy, especially the
/// cached scan of Cargo's hashed artifacts.
fn resolve_from(
    cargo_binary: Option<PathBuf>,
    executable: Option<PathBuf>,
    manifest_dir: Option<PathBuf>,
    search_path: Option<std::ffi::OsString>,
) -> Result<PathBuf, SandboxError> {
    cargo_binary
        .filter(|path| path.is_file())
        .or_else(|| adjacent_helper(executable.as_deref()?, manifest_dir.as_deref()))
        .or_else(|| {
            std::env::split_paths(search_path.as_ref()?)
                .map(|dir| dir.join(HELPER_BIN_NAME))
                .find(|path| path.is_file())
        })
        .ok_or(SandboxError::HelperNotFound)
}

fn adjacent_helper(
    executable: &std::path::Path,
    manifest_dir: Option<&std::path::Path>,
) -> Option<PathBuf> {
    let dir = executable.parent()?;
    let adjacent = dir.join(HELPER_BIN_NAME);
    if adjacent.is_file() {
        return Some(adjacent);
    }
    // Only Cargo's deps layout permits searching a parent profile directory
    // or scanning hashed artifacts. Installed binaries use adjacency/PATH.
    if dir.file_name().is_none_or(|name| name != "deps") {
        return None;
    }
    cargo_profile_helper(dir, manifest_dir).or_else(|| cargo_test_artifact(dir))
}

fn cargo_profile_helper(
    deps_dir: &std::path::Path,
    manifest_dir: Option<&std::path::Path>,
) -> Option<PathBuf> {
    let profile_dir = deps_dir.parent()?;
    let adjacent = profile_dir.join(HELPER_BIN_NAME);
    if adjacent.is_file() {
        return Some(adjacent);
    }
    // The repository no longer splits build/target directories, but an
    // externally configured Cargo build-dir can still put the test binary
    // away from the workspace's uplifted helper. Keep this before the
    // artifact scan, and use the executable's profile (including custom ones).
    workspace_uplifted_helper(manifest_dir?, profile_dir.file_name()?)
}

/// Walks up from `manifest_dir` looking for the workspace's uplifted helper
/// at `<ancestor>/target/<profile>/horizon-sandbox-helper`. Cargo uplifts final
/// bin targets into the workspace's own `target/<profile>/` even when
/// `build.build-dir` redirects intermediate artifacts to a shared cache, so
/// this resolves deterministically without scanning `deps/` by mtime.
fn workspace_uplifted_helper(
    manifest_dir: &std::path::Path,
    profile: &std::ffi::OsStr,
) -> Option<PathBuf> {
    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("target").join(profile).join(HELPER_BIN_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Cargo builds an integration-test dependency's binary as a hashed file in
/// `deps`, but does not always materialize the ordinary adjacent binary.
/// The same directory can also contain a same-name Rust test harness, so a
/// filename match is insufficient. The real entry point embeds a versioned
/// protocol marker (the `#[used]` static in `bin/horizon-sandbox-helper.rs`
/// is `#[cfg(not(test))]`-gated so the harness variant does not carry it);
/// choose the newest matching executable and cache it.
#[cfg_attr(test, allow(dead_code))]
fn cargo_test_artifact(deps_dir: &std::path::Path) -> Option<PathBuf> {
    static CACHED: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHED
        .get_or_init(|| find_cargo_test_artifact(deps_dir))
        .clone()
}

fn find_cargo_test_artifact(deps_dir: &std::path::Path) -> Option<PathBuf> {
    let prefix = HELPER_BIN_NAME.replace('-', "_") + "-";
    let mut candidates = std::fs::read_dir(deps_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with(&prefix) || !path.is_file() {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.0));
    let marker = crate::HELPER_PROTOCOL_MARKER.as_bytes();
    candidates.into_iter().find_map(|(_, path)| {
        let bytes = std::fs::read(&path).ok()?;
        bytes
            .windows(marker.len())
            .any(|window| window == marker)
            .then_some(path)
    })
}

#[cfg(test)]
mod tests {
    use super::HELPER_BIN_NAME;
    use super::{
        adjacent_helper, find_cargo_test_artifact, resolve_from, workspace_uplifted_helper,
    };
    use std::fs;
    use std::path::PathBuf;

    fn test_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "horizon-helper-{label}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&path).expect("create test directory");
        path
    }

    fn unique_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    }

    fn touch(path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, b"fake").expect("write fake binary");
    }

    #[test]
    fn override_then_adjacent_then_path_and_missing_candidates_are_skipped() {
        let root = test_dir("precedence");
        let override_binary = root.join("override");
        let executable = root.join("bin/application");
        let adjacent = root.join("bin").join(HELPER_BIN_NAME);
        let path_binary = root.join("path").join(HELPER_BIN_NAME);
        for path in [&override_binary, &adjacent, &path_binary] {
            touch(path);
        }
        let resolve = || {
            resolve_from(
                Some(override_binary.clone()),
                Some(executable.clone()),
                None,
                Some(std::env::join_paths([root.join("missing"), root.join("path")]).unwrap()),
            )
        };
        for expected in [&override_binary, &adjacent, &path_binary] {
            assert_eq!(&resolve().unwrap(), expected);
            fs::remove_file(expected).unwrap();
        }
        assert!(matches!(
            resolve(),
            Err(crate::SandboxError::HelperNotFound)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cargo_profile_precedes_uplifted_helper_and_only_deps_enables_that_search() {
        let root = test_dir("cargo-layout");
        let manifest = root.join("workspace/crates/pkg");
        fs::create_dir_all(&manifest).unwrap();
        let executable = root.join("build/custom-profile/deps/test");
        let adjacent = root.join("build/custom-profile").join(HELPER_BIN_NAME);
        let uplifted = root
            .join("workspace/target/custom-profile")
            .join(HELPER_BIN_NAME);
        touch(&adjacent);
        touch(&uplifted);
        assert_eq!(
            adjacent_helper(&executable, Some(&manifest)),
            Some(adjacent.clone())
        );
        fs::remove_file(adjacent).unwrap();
        assert_eq!(
            adjacent_helper(&executable, Some(&manifest)),
            Some(uplifted)
        );
        assert!(adjacent_helper(&root.join("bin/application"), Some(&manifest)).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn artifact_scan_selects_newest_protocol_match_and_skips_test_harnesses() {
        let root = test_dir("artifacts");
        for (name, seconds, bytes) in [
            (
                "horizon_sandbox_helper-old",
                1,
                crate::HELPER_PROTOCOL_MARKER.as_bytes(),
            ),
            (
                "horizon_sandbox_helper-new",
                2,
                crate::HELPER_PROTOCOL_MARKER.as_bytes(),
            ),
            (
                "horizon_sandbox_helper-harness",
                3,
                b"test harness".as_slice(),
            ),
            ("other_binary", 4, crate::HELPER_PROTOCOL_MARKER.as_bytes()),
        ] {
            let path = root.join(name);
            fs::write(&path, bytes).unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(
                    fs::FileTimes::new().set_modified(
                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
                    ),
                )
                .unwrap();
        }
        assert_eq!(
            find_cargo_test_artifact(&root),
            Some(root.join("horizon_sandbox_helper-new"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uplifted_helper_found_at_workspace_root() {
        let root = test_dir("found");
        let manifest_dir = root.join("crates").join("horizon-agent");
        fs::create_dir_all(&manifest_dir).expect("create manifest dir");
        let helper = root.join("target").join("debug").join(HELPER_BIN_NAME);
        touch(&helper);

        let found = workspace_uplifted_helper(&manifest_dir, std::ffi::OsStr::new("debug"));
        assert_eq!(found.as_ref(), Some(&helper));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn uplifted_helper_walks_up_multiple_ancestors() {
        let root = test_dir("walk-up");
        let manifest_dir = root.join("a").join("b").join("c").join("d");
        fs::create_dir_all(&manifest_dir).expect("create manifest dir");
        let helper = root.join("target").join("release").join(HELPER_BIN_NAME);
        touch(&helper);

        let found = workspace_uplifted_helper(&manifest_dir, std::ffi::OsStr::new("release"));
        assert_eq!(found.as_ref(), Some(&helper));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn uplifted_helper_returns_none_when_not_present() {
        let root = test_dir("none");
        let manifest_dir = root.join("crates").join("horizon-agent");
        fs::create_dir_all(&manifest_dir).expect("create manifest dir");

        // A profile name that cannot exist anywhere above temp_dir, so the
        // walk finds nothing regardless of what real target dirs sit above.
        let found =
            workspace_uplifted_helper(&manifest_dir, std::ffi::OsStr::new("no-such-profile-zzz"));
        assert!(found.is_none());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn uplifted_helper_prefers_nearest_ancestor() {
        let root = test_dir("nearest");
        let manifest_dir = root.join("workspace").join("crates").join("pkg");
        fs::create_dir_all(&manifest_dir).expect("create manifest dir");

        let near = root
            .join("workspace")
            .join("target")
            .join("debug")
            .join(HELPER_BIN_NAME);
        touch(&near);

        let far = root.join("target").join("debug").join(HELPER_BIN_NAME);
        touch(&far);

        let found = workspace_uplifted_helper(&manifest_dir, std::ffi::OsStr::new("debug"));
        assert_eq!(found.as_ref(), Some(&near));

        fs::remove_dir_all(root).expect("cleanup");
    }
}
