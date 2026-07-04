use std::path::Path;

use walkdir::{DirEntry, WalkDir};

/// Directory names skipped by default during `fs.glob`/`fs.grep`
/// traversal: version-control metadata and dependency/build-output
/// directories that are typically large, rarely relevant to a code search,
/// and often full of binary or generated content.
pub(super) const SKIPPED_DIR_NAMES: &[&str] = &[".git", "target", "node_modules"];

/// Maximum number of files a single `fs.glob`/`fs.grep` traversal will
/// visit before stopping early, independent of how many results match or
/// are returned. Bounds worst-case latency when `base_path` is broader than
/// intended (e.g. a repo root instead of a subdirectory).
///
/// Shrunk under `cfg(test)` so the cap-tripping path can be exercised with a
/// handful of files instead of creating tens of thousands of them on disk.
#[cfg(not(test))]
pub(super) const MAX_VISITED_FILES: usize = 20_000;
#[cfg(test)]
pub(super) const MAX_VISITED_FILES: usize = 20;

/// The note surfaced in a tool result when a traversal cap (file count or,
/// for `fs.grep`, total bytes read) cut a scan short, so the model can
/// adapt — narrow `base_path`/`pattern` — instead of silently receiving
/// incomplete results with no explanation.
pub(super) fn scan_truncated_note(visited: usize) -> String {
    format!("scan truncated at {visited} files — narrow base_path or pattern")
}

/// True if `entry` is a directory that should be pruned from the walk: one
/// of `SKIPPED_DIR_NAMES`, but never the walk's own root (depth 0) — if a
/// caller explicitly points `base_path` at e.g. `node_modules`, that's an
/// intentional target, not something to skip.
fn is_skipped_dir(entry: &DirEntry) -> bool {
    entry.depth() > 0
        && entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| SKIPPED_DIR_NAMES.contains(&name))
}

/// A `WalkDir` over `base` that prunes `SKIPPED_DIR_NAMES` directories
/// (never descending into them at all). Callers are responsible for
/// enforcing `MAX_VISITED_FILES` (and, for `fs.grep`, a byte budget) since
/// only they know how to report which cap tripped.
pub(super) fn walk(base: &Path) -> impl Iterator<Item = DirEntry> {
    WalkDir::new(base)
        .into_iter()
        .filter_entry(|entry| !is_skipped_dir(entry))
        .filter_map(Result::ok)
}
