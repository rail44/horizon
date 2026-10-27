//! The leaf entries of one revision's tree: `git ls-tree -r <rev>`.

use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;
use gix::traverse::tree::Recorder;

use crate::{Repository, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TreeRow {
    pub path: String,
    /// git's octal mode string (`100644`, `100755`, `120000`, `160000`).
    pub mode: &'static str,
    /// The blob id (a symlink's blob holds its target path), or the
    /// recorded commit for a gitlink.
    pub blob_id: String,
}

/// The id of the tree `rev` names: a commit-ish is peeled to its tree, a
/// tree id is taken as is. An unknown revision, or one that names a blob, is
/// an error naming `rev`.
pub fn resolve_tree(repo: &Repository, rev: &str) -> Result<ObjectId> {
    let object = repo
        .rev_parse_single(rev)
        .map_err(|error| format!("cannot resolve revision '{rev}': {error}"))?
        .object()
        .map_err(|error| format!("cannot resolve revision '{rev}': {error}"))?;
    let tree = object
        .peel_to_tree()
        .map_err(|error| format!("revision '{rev}' does not name a tree: {error}"))?;
    Ok(tree.id)
}

/// Every non-tree entry reachable from `tree`, recursively, sorted by path
/// bytes: regular files, symlinks and gitlinks (submodules are not entered).
pub fn tree_entries(repo: &Repository, tree: ObjectId) -> Result<Vec<TreeRow>> {
    let mut recorder = Recorder::default();
    repo.find_tree(tree)?
        .traverse()
        .breadthfirst(&mut recorder)?;
    let mut entries: Vec<_> = recorder
        .records
        .into_iter()
        .filter_map(|entry| {
            let mode = match entry.mode.kind() {
                EntryKind::Tree => return None,
                EntryKind::Blob => "100644",
                EntryKind::BlobExecutable => "100755",
                EntryKind::Link => "120000",
                EntryKind::Commit => "160000",
            };
            Some((entry.filepath, mode, entry.oid))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries
        .into_iter()
        .map(|(path, mode, id)| TreeRow {
            path: path.to_str_lossy().into_owned(),
            mode,
            blob_id: id.to_string(),
        })
        .collect())
}

/// `git ls-tree -r <rev>`: [`resolve_tree`] then [`tree_entries`].
pub fn tree(repo: &Repository, rev: &str) -> Result<Vec<TreeRow>> {
    tree_entries(repo, resolve_tree(repo, rev)?)
}
