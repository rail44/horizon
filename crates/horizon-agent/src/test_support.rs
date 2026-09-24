//! Filesystem fixtures shared by instruction, skill and tool discovery tests.

use std::path::PathBuf;

/// This fixture must be outside every checkout, even when the host's default
/// temporary directory has a `.git` ancestor. Its owner cleans up on unwind too.
pub(crate) fn non_repository_test_dir() -> tempfile::TempDir {
    let base = [std::env::temp_dir(), PathBuf::from("/var/tmp")]
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .find(|path| {
            path.is_dir()
                && !path
                    .ancestors()
                    .any(|ancestor| ancestor.join(".git").exists())
        })
        .expect("repository-boundary tests need a temporary directory outside any checkout");
    tempfile::Builder::new()
        .prefix("horizon-non-repository-")
        .tempdir_in(base)
        .expect("create a directory outside the repository")
}
