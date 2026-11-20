//! The files under a directory, as a finder lists them.
//!
//! What `.gitignore`, `.ignore` and git's own excludes leave out is left
//! out, and so is `.git` itself; files whose names start with a dot are
//! listed. Links are followed.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// How many files are handed over at a time.
const BATCH: usize = 512;

/// Whether `path` is one of the files wanted: any file when `types` is
/// empty, else one of one of them (`crate::media_type`).
fn wanted(path: &Path, types: &[String]) -> bool {
    types.is_empty() || crate::media_type::is_of(path, types)
}

/// The files under `root` of one of `types`, as paths from `root`
/// with `/` between their parts, handed to `found` in batches until
/// `stopped` says to stop.
pub(super) fn walk(
    root: &Path,
    types: &[String],
    stopped: &AtomicBool,
    found: impl Fn(Vec<String>),
) {
    let mut walk = ignore::WalkBuilder::new(root);
    walk.hidden(false)
        .follow_links(true)
        .filter_entry(|entry| entry.file_name() != ".git")
        .sort_by_file_name(Ord::cmp);
    let mut batch = Vec::new();
    for entry in walk.build().flatten() {
        if stopped.load(Ordering::Relaxed) {
            return;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(path) = entry.path().strip_prefix(root) else {
            continue;
        };
        if !wanted(path, types) {
            continue;
        }
        let parts: Vec<_> = (path.components())
            .map(|part| part.as_os_str().to_string_lossy())
            .collect();
        batch.push(parts.join("/"));
        if batch.len() == BATCH {
            found(std::mem::take(&mut batch));
        }
    }
    if !batch.is_empty() {
        found(batch);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use super::walk;

    fn listed(root: &std::path::Path, types: &[&str]) -> Vec<String> {
        let types: Vec<String> = types.iter().map(|text| text.to_string()).collect();
        let found = Mutex::new(Vec::new());
        walk(root, &types, &AtomicBool::new(false), |batch| {
            found.lock().unwrap().extend(batch)
        });
        found.into_inner().unwrap()
    }

    #[test]
    fn ignored_files_and_git_s_own_are_left_out_and_dot_files_are_listed() {
        let root = std::env::temp_dir().join(format!("hzb-listing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for directory in [".git", ".notes", "docs", "target"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        for (file, text) in [
            (".gitignore", "target/\n"),
            (".git/description.md", ""),
            (".notes/todo.md", ""),
            ("docs/a.MD", ""),
            ("docs/b.txt", ""),
            ("target/built.md", ""),
            ("README.md", ""),
        ] {
            std::fs::write(root.join(file), text).unwrap();
        }
        // (`.gitignore` is read where there is a repository.)
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(
            listed(&root, &["text/markdown"]),
            [".notes/todo.md", "README.md", "docs/a.MD"]
        );
        assert!(listed(&root, &[]).contains(&"docs/b.txt".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
