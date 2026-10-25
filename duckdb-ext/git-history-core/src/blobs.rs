//! The pre- and post-image blobs of every changed file in a history range,
//! written to a directory as plain files so that a file reader (`read_ast`)
//! can parse historical content the way it parses current files.
//!
//! The changes are those of [`crate::file_changes`] (same walk, same tree
//! diff with rename detection); this module only decides which sides of
//! them have a blob worth writing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::diff::blob::ResourceKind;
use gix::diff::blob::platform::resource::Data;
use gix::objs::tree::EntryKind;

use crate::{Change, Range, Repository, Result, for_each_change};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChangedBlobRow {
    pub sha: String,
    /// The file's path in the commit (the new path of a rename).
    pub path: String,
    /// The pre-image path of a rename, else `None`.
    pub old_path: Option<String>,
    /// `old` (the pre-image) or `new` (the post-image).
    pub side: &'static str,
    pub blob_id: String,
    /// git's mode string of that side: `100644` or `100755`.
    pub mode: &'static str,
    /// The absolute path of the written file.
    pub file: String,
}

/// What one [`changed_blobs`] call did to the directory. A file referenced by
/// several rows counts once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlobWriteStats {
    /// Files created, or replaced because they had another size.
    pub written: usize,
    /// Files that already existed with the blob's size and were left alone.
    pub skipped: usize,
    /// The bytes of the written files.
    pub bytes_written: u64,
}

/// The part of `path`'s last component after its last `.`, if not empty.
fn extension(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, ext)| ext)
        .filter(|ext| !ext.is_empty())
}

fn regular_mode(kind: EntryKind) -> Option<&'static str> {
    match kind {
        EntryKind::Blob => Some("100644"),
        EntryKind::BlobExecutable => Some("100755"),
        EntryKind::Link | EntryKind::Commit | EntryKind::Tree => None,
    }
}

/// Distinguishes temporary files of concurrent calls within one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes each blob once as `<dir>/<blob_id>[.<ext>]`.
struct BlobDir {
    dir: PathBuf,
    /// File names already handled by this call.
    seen: HashSet<String>,
    stats: BlobWriteStats,
}

impl BlobDir {
    /// The written file for `id` at a path with `ext`, writing it if this call
    /// has not yet and it is not already there with the blob's size.
    fn file(&mut self, repo: &Repository, id: ObjectId, ext: Option<&str>) -> Result<String> {
        let name = match ext {
            Some(ext) => format!("{id}.{ext}"),
            None => id.to_string(),
        };
        let file = self.dir.join(&name);
        if self.seen.insert(name.clone()) {
            let size = repo.find_header(id)?.size();
            match std::fs::symlink_metadata(&file) {
                Ok(meta) if meta.is_file() && meta.len() == size => self.stats.skipped += 1,
                _ => {
                    let blob = repo.find_blob(id)?;
                    let temp = self.dir.join(format!(
                        ".{name}.{}-{}.tmp",
                        std::process::id(),
                        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
                    ));
                    std::fs::write(&temp, &blob.data)?;
                    if let Err(error) = std::fs::rename(&temp, &file) {
                        let _ = std::fs::remove_file(&temp);
                        return Err(error.into());
                    }
                    self.stats.written += 1;
                    self.stats.bytes_written += blob.data.len() as u64;
                }
            }
        }
        Ok(file.to_string_lossy().into_owned())
    }
}

/// The blobs of the changes of [`crate::file_changes`] for `range`, written to
/// `dir` (an absolute path, created if missing): one row per side of a change
/// that is a regular, non-binary file, `old` before `new`, in the order of
/// `file_changes`.
///
/// A side has no row when it does not exist (`old` of an addition, `new` of a
/// deletion), is a symlink or a gitlink, is binary by gix's rule (the `diff`
/// and `binary` attributes, else a NUL byte in the content, or the big-file
/// threshold), or its path's extension is not in `extensions`. A change whose
/// two sides are regular files with the same blob (a rename without content
/// change, an executable bit flip) has no rows. Each blob is read from the
/// object database and written with its exact bytes.
pub fn changed_blobs(
    repo: &Repository,
    range: &Range,
    dir: &Path,
    extensions: Option<&[String]>,
) -> Result<(Vec<ChangedBlobRow>, BlobWriteStats)> {
    if !dir.is_absolute() {
        return Err(format!(
            "git_changed_blobs: dir must be an absolute path, got '{}'",
            dir.display()
        )
        .into());
    }
    std::fs::create_dir_all(dir)?;
    let mut out = BlobDir {
        dir: dir.to_path_buf(),
        seen: HashSet::new(),
        stats: BlobWriteStats::default(),
    };
    let mut rows = Vec::new();
    for_each_change(repo, range, |sha, change: &Change, cache| {
        let same_content = change.old.0 == change.new.0
            && regular_mode(change.old.1.kind()).is_some()
            && regular_mode(change.new.1.kind()).is_some();
        if same_content {
            return Ok(());
        }
        let old_path = matches!(change.status, "R" | "C").then(|| change.old_path.clone());
        let sides = [
            (
                ResourceKind::OldOrSource,
                "old",
                &change.old_path,
                change.old,
            ),
            (
                ResourceKind::NewOrDestination,
                "new",
                &change.path,
                change.new,
            ),
        ];
        for (kind, side, path, (id, mode)) in sides {
            if id.is_null() {
                continue;
            }
            let Some(mode_str) = regular_mode(mode.kind()) else {
                continue;
            };
            let ext = extension(path);
            if let Some(list) = extensions
                && !ext.is_some_and(|ext| list.iter().any(|e| e == ext))
            {
                continue;
            }
            cache.set_resource(
                id,
                mode.kind(),
                path.as_bytes().as_bstr(),
                kind,
                &repo.objects,
            )?;
            if matches!(
                cache.resource(kind).map(|r| r.data),
                None | Some(Data::Binary { .. })
            ) {
                continue;
            }
            let file = out.file(repo, id, ext)?;
            rows.push(ChangedBlobRow {
                sha: sha.to_string(),
                path: change.path.clone(),
                old_path: old_path.clone(),
                side,
                blob_id: id.to_string(),
                mode: mode_str,
                file,
            });
        }
        Ok(())
    })?;
    Ok((rows, out.stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_is_after_the_last_dot_of_the_last_component() {
        assert_eq!(extension("src/lib.rs"), Some("rs"));
        assert_eq!(extension("a.tar.gz"), Some("gz"));
        assert_eq!(extension("dir.d/Makefile"), None);
        assert_eq!(extension(".gitignore"), Some("gitignore"));
        assert_eq!(extension("trailing."), None);
        assert_eq!(extension("plain"), None);
    }
}
