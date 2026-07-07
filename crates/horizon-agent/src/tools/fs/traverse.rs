use std::path::Path;

use ignore::{DirEntry, WalkBuilder};

/// Directory names pruned unconditionally during `fs.glob`/`fs.grep`
/// traversal, regardless of what (if anything) `.gitignore` says: version-
/// control metadata and dependency/build-output directories that are
/// typically large, rarely relevant to a code search, and often full of
/// binary or generated content. `.git` must never be walked at all;
/// `target`/`node_modules` are kept as a fallback so traversal stays exactly
/// as narrow as before in repositories that don't list them in
/// `.gitignore` (or aren't a git repository at all, where `.gitignore` isn't
/// consulted — see `walk`'s doc comment).
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
        && entry
            .file_type()
            .is_some_and(|file_type| file_type.is_dir())
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| SKIPPED_DIR_NAMES.contains(&name))
}

/// A walk over `base` built on the `ignore` crate — the same walker
/// ripgrep uses — so `fs.glob`/`fs.grep` respect `.gitignore`, `.ignore`,
/// and the user's global gitignore the way any other tool working in the
/// repository would. Per `WalkBuilder`'s default `require_git`, the git-
/// flavored ignore files only take effect when `base` is inside an actual
/// git repository; outside one, only `SKIPPED_DIR_NAMES` below applies.
///
/// `hidden(false)` opts out of the crate's other default (skipping all
/// dotfiles/dotdirs): this migration is scoped to adding `.gitignore`
/// support, not to newly hiding e.g. `.github/` from a search, so plain
/// dotfiles keep being walked exactly as they were under the old `walkdir`-
/// based traversal. `SKIPPED_DIR_NAMES` is still pruned unconditionally on
/// top of the gitignore rules, so behavior is never more permissive than
/// before — a repository with no `.gitignore` entry for `target`/
/// `node_modules` (or no `.gitignore`/git repo at all) still skips them.
///
/// Callers are responsible for enforcing the traversal file-count cap
/// (`agent::config::FsToolConfig::traversal_max_files`, and for `fs.grep` a
/// byte budget too) since only they know how to report which cap tripped.
pub(super) fn walk(base: &Path) -> impl Iterator<Item = DirEntry> {
    WalkBuilder::new(base)
        .hidden(false)
        .filter_entry(|entry| !is_skipped_dir(entry))
        .build()
        .filter_map(Result::ok)
}
