use std::path::Path;

use walkdir::{DirEntry, WalkDir};

/// Directory names skipped by default during `fs.glob`/`fs.grep`
/// traversal: version-control metadata and dependency/build-output
/// directories that are typically large, rarely relevant to a code search,
/// and often full of binary or generated content.
pub(super) const SKIPPED_DIR_NAMES: &[&str] = &[".git", "target", "node_modules"];

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
/// enforcing the traversal file-count cap (`agent::config::FsToolConfig::
/// traversal_max_files`, and for `fs.grep` a byte budget too) since only
/// they know how to report which cap tripped.
pub(super) fn walk(base: &Path) -> impl Iterator<Item = DirEntry> {
    WalkDir::new(base)
        .into_iter()
        .filter_entry(|entry| !is_skipped_dir(entry))
        .filter_map(Result::ok)
}
