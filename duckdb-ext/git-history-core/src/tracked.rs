//! The files a worktree has right now and the content id of each:
//! `git ls-files -s` for the index, `git ls-files -o --exclude-standard` for
//! untracked files, and `git hash-object` for content that differs from the
//! index, computed with gix's index-to-worktree status.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gix::ObjectId;
use gix::bstr::{BStr, BString, ByteSlice};
use gix::dir::entry::{Kind as DiskKind, Status as DirStatus};
use gix::dir::walk::EmissionMode;
use gix::index::entry::{Mode, Stage};
use gix::status::index_worktree::BuiltinSubmoduleStatus;
use gix::status::plumbing::index_as_worktree::traits::HashEq;
use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
use gix::status::plumbing::index_as_worktree_with_renames::{Entry, Recorder};

use crate::{Repository, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TrackedFileRow {
    pub path: String,
    /// git's octal mode string (`100644`, `100755`, `120000`, `160000`).
    pub mode: String,
    /// The id of the current worktree content (the recorded commit for a gitlink).
    pub blob_id: String,
    pub is_tracked: bool,
    pub is_modified: bool,
}

fn mode_string(mode: Mode) -> String {
    format!("{:06o}", mode.bits())
}

/// Hashes worktree files the way `git hash-object` does: through the clean
/// filters the repository's attributes and configuration select.
struct Hasher<'repo> {
    repo: &'repo Repository,
    workdir: &'repo Path,
    pipeline: gix::filter::Pipeline<'repo>,
    index: gix::worktree::IndexPersistedOrInMemory,
    caps: gix::fs::Capabilities,
    buf: Vec<u8>,
}

impl<'repo> Hasher<'repo> {
    fn new(repo: &'repo Repository, workdir: &'repo Path) -> Result<Self> {
        let (pipeline, index) = repo.filter_pipeline(None)?;
        Ok(Hasher {
            repo,
            workdir,
            pipeline,
            index,
            caps: repo.filesystem_options()?,
            buf: Vec::new(),
        })
    }

    /// The mode `git add` would record for the file at `rela_path`, or `None`
    /// if nothing is there any more.
    fn mode(&self, rela_path: &BStr) -> Result<Option<Mode>> {
        let path = self.workdir.join(gix::path::from_bstr(rela_path)?);
        let meta = match gix::index::fs::Metadata::from_path_no_follow(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(if meta.is_symlink() && self.caps.symlink {
            Mode::SYMLINK
        } else if meta.is_executable() && self.caps.executable_bit {
            Mode::FILE_EXECUTABLE
        } else {
            Mode::FILE
        }))
    }

    /// The blob id of the content at `rela_path`: a symlink's target path, or a
    /// file's bytes after the clean filters.
    fn blob_id(&mut self, rela_path: &BStr, mode: Mode) -> Result<ObjectId> {
        let rela = gix::path::from_bstr(rela_path)?;
        let path = self.workdir.join(&rela);
        self.buf.clear();
        if mode == Mode::SYMLINK {
            let target = std::fs::read_link(&path)?;
            self.buf
                .extend_from_slice(gix::path::into_bstr(target)?.as_ref());
        } else {
            let file = std::fs::File::open(&path)?;
            let mut stream = self.pipeline.convert_to_git(file, &rela, &self.index)?;
            stream.read_to_end(&mut self.buf)?;
        }
        Ok(gix::objs::compute_hash(
            self.repo.object_hash(),
            gix::objs::Kind::Blob,
            &self.buf,
        )?)
    }
}

/// `git ls-files -s` with each file's current content id, plus with
/// `include_untracked` the files of `git ls-files -o --exclude-standard`.
///
/// Rows are sorted by path bytes. Files deleted in the worktree have no row;
/// a gitlink keeps the commit the index records; an unmerged path has one
/// row for its worktree content.
pub fn tracked_files(repo: &Repository, include_untracked: bool) -> Result<Vec<TrackedFileRow>> {
    let workdir = repo
        .workdir()
        .ok_or("git_tracked_files needs a repository with a worktree")?;
    let index = repo.index_or_empty()?;
    let dirwalk_options = if include_untracked {
        Some(
            repo.dirwalk_options()?
                .emit_untracked(EmissionMode::Matching),
        )
    } else {
        None
    };
    let options = gix::status::index_worktree::Options {
        sorting: None,
        dirwalk_options,
        rewrites: None,
        thread_limit: None,
    };
    // Gitlinks keep the commit the index records, so submodules are never opened.
    let submodules = BuiltinSubmoduleStatus::new(
        repo.clone().into_sync(),
        gix::status::Submodule::Given {
            ignore: gix::submodule::config::Ignore::All,
            check_dirty: false,
        },
    )?;
    let mut recorder = Recorder {
        records: Vec::new(),
    };
    repo.index_worktree_status(
        &index,
        Vec::<BString>::new(),
        &mut recorder,
        HashEq,
        submodules,
        &mut gix::progress::Discard,
        &AtomicBool::new(false),
        options,
    )?;

    let mut statuses = HashMap::new();
    let mut untracked = Vec::new();
    for record in recorder.records {
        match record {
            Entry::Modification {
                entry_index,
                status,
                ..
            } => {
                statuses.insert(entry_index, status);
            }
            Entry::DirectoryContents { entry, .. } => {
                // Nested repositories and anything else that is not a file are skipped.
                if entry.status == DirStatus::Untracked
                    && matches!(entry.disk_kind, Some(DiskKind::File | DiskKind::Symlink))
                {
                    untracked.push(entry.rela_path);
                }
            }
            Entry::Rewrite { .. } => unreachable!("rewrite tracking is off"),
        }
    }

    let mut hasher = Hasher::new(repo, workdir)?;
    let mut rows: Vec<(BString, TrackedFileRow)> = Vec::new();
    let mut last_unmerged: Option<&BStr> = None;
    for (entry_index, entry) in index.entries().iter().enumerate() {
        let path = entry.path(&index);
        let tracked = |mode: Mode, id: ObjectId, is_modified: bool| {
            (
                path.to_owned(),
                TrackedFileRow {
                    path: path.to_str_lossy().into_owned(),
                    mode: mode_string(mode),
                    blob_id: id.to_string(),
                    is_tracked: true,
                    is_modified,
                },
            )
        };
        if entry.stage() != Stage::Unconflicted {
            if last_unmerged == Some(path) {
                continue;
            }
            last_unmerged = Some(path);
            if let Some(mode) = hasher.mode(path)? {
                let id = hasher.blob_id(path, mode)?;
                rows.push(tracked(mode, id, true));
            }
            continue;
        }
        if entry.mode == Mode::COMMIT {
            rows.push(tracked(entry.mode, entry.id, false));
            continue;
        }
        match statuses.remove(&entry_index) {
            None | Some(EntryStatus::NeedsUpdate(_)) => {
                rows.push(tracked(entry.mode, entry.id, false));
            }
            Some(EntryStatus::Change(Change::Removed)) => {}
            Some(EntryStatus::Change(Change::Modification {
                executable_bit_changed,
                content_change,
                ..
            })) => {
                let mode = match (executable_bit_changed, entry.mode) {
                    (true, Mode::FILE) => Mode::FILE_EXECUTABLE,
                    (true, Mode::FILE_EXECUTABLE) => Mode::FILE,
                    (_, mode) => mode,
                };
                rows.push(tracked(mode, content_change.unwrap_or(entry.id), true));
            }
            Some(EntryStatus::Change(Change::Type { worktree_mode })) => {
                let id = hasher.blob_id(path, worktree_mode)?;
                rows.push(tracked(worktree_mode, id, true));
            }
            Some(EntryStatus::IntentToAdd) => {
                if let Some(mode) = hasher.mode(path)? {
                    let id = hasher.blob_id(path, mode)?;
                    rows.push(tracked(mode, id, id != entry.id));
                }
            }
            // Unreachable with submodules ignored, and conflicts are handled above.
            Some(EntryStatus::Change(Change::SubmoduleModification(_)))
            | Some(EntryStatus::Conflict { .. }) => {
                rows.push(tracked(entry.mode, entry.id, false));
            }
        }
    }
    for path in untracked {
        let Some(mode) = hasher.mode(path.as_ref())? else {
            continue;
        };
        let id = hasher.blob_id(path.as_ref(), mode)?;
        let row = TrackedFileRow {
            path: path.to_str_lossy().into_owned(),
            mode: mode_string(mode),
            blob_id: id.to_string(),
            is_tracked: false,
            is_modified: false,
        };
        rows.push((path, row));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}
