//! Resolution shared by the Linux supervisor and macOS Seatbelt helpers.

use crate::SandboxError;
use std::{path::PathBuf, sync::OnceLock};

pub(crate) const HELPER_BIN_NAME: &str = "horizon-sandbox-helper";

// Unreferenced under cfg(test): only the production spawn_with_grants
// (cfg(not(test)) in linux/mod.rs) calls this, but the module is compiled
// under test for the unit tests below.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn resolve() -> Result<PathBuf, SandboxError> {
    let cargo_var = "CARGO_BIN_EXE_horizon-sandbox-helper";
    if let Some(candidate) = std::env::var_os(cargo_var).map(PathBuf::from) {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let adjacent = dir.join(HELPER_BIN_NAME);
            if adjacent.is_file() {
                return Ok(adjacent);
            }
            // Cargo integration-test executables live in target/<profile>/deps,
            // while ordinary bin targets live one directory above.
            if dir.file_name().is_some_and(|name| name == "deps") {
                if let Some(profile_dir) = dir.parent() {
                    let cargo_adjacent = profile_dir.join(HELPER_BIN_NAME);
                    if cargo_adjacent.is_file() {
                        return Ok(cargo_adjacent);
                    }
                    // Deterministic fallback before the mtime-scan heuristic.
                    // When a unit-test process's `current_exe()` lives in the
                    // shared build-dir's `deps/` (because `.cargo/config.toml`
                    // redirects `build.build-dir` away from the worktree's own
                    // `target/`), the adjacency probes above miss: cargo uplifts
                    // the real bin into `<workspace>/target/<profile>/`, not the
                    // shared build-dir. Cargo sets `CARGO_MANIFEST_DIR` at
                    // runtime for test processes; walking up from it reaches the
                    // workspace root, where the uplifted copy lives. The
                    // `<profile>` is taken from `current_exe()`'s path (the
                    // component immediately above `deps`), so this stays correct
                    // for `debug`, `release`, and any custom profile.
                    if let Some(profile) = profile_dir.file_name() {
                        if let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR") {
                            if let Some(candidate) =
                                workspace_uplifted_helper(&PathBuf::from(manifest_dir), profile)
                            {
                                return Ok(candidate);
                            }
                        }
                    }
                }
                if let Some(candidate) = cargo_test_artifact(dir) {
                    return Ok(candidate);
                }
            }
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(HELPER_BIN_NAME);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(SandboxError::HelperNotFound)
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
        .get_or_init(|| {
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
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::workspace_uplifted_helper;
    use super::HELPER_BIN_NAME;
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
