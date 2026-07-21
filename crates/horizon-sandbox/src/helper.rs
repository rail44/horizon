//! Resolution shared by the Linux supervisor and macOS Seatbelt helpers.

use crate::SandboxError;
use std::{path::PathBuf, sync::OnceLock};

pub(crate) const HELPER_BIN_NAME: &str = "horizon-sandbox-helper";

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

/// Cargo builds an integration-test dependency's binary as a hashed file in
/// `deps`, but does not always materialize the ordinary adjacent binary.
/// The same directory can also contain a same-name Rust test harness, so a
/// filename match is insufficient. The real entry point embeds a versioned
/// protocol marker; choose the newest matching executable and cache it.
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
