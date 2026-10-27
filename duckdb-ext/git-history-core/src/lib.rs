//! The rows of the `git_history` table functions, computed with gitoxide (`gix`).
//!
//! Each history function corresponds to one `git log` invocation of the code
//! database spike (`scripts/code-db/history.py`); its doc comment names it.
//! [`tracked_files`] is the worktree's current files (`git ls-files`).
//! [`changed_blobs`] writes the blobs of [`file_changes`]'s changes to a
//! directory. [`tree`] is one revision's tree (`git ls-tree -r`). The
//! rows follow gix's behavior; `duckdb-ext/README.md` lists where that
//! differs from git.

use std::ops::Range as Lines;

pub use gix::ObjectId;
pub use gix::Repository;
use gix::bstr::{BStr, ByteSlice};
use gix::diff::Rewrites;
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{Algorithm, Diff, InternedInput, ResourceKind, UnifiedDiff};
use gix::diff::tree_with_rewrites::{Action, ChangeRef};
use gix::objs::tree::{EntryKind, EntryMode};
use gix::traverse::commit::simple::CommitTimeOrder;

mod blobs;
mod tracked;
mod tree;
pub use blobs::{BlobWriteStats, ChangedBlobRow, changed_blobs};
pub use tracked::{TrackedFileRow, tracked_files};
pub use tree::{TreeRow, resolve_tree, tree, tree_entries};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// The revision range and merge handling shared by every function.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Range {
    /// History is `since..HEAD` when set, else everything reachable from HEAD.
    pub since: Option<String>,
    /// Diff merge commits against their first parent (`--diff-merges=first-parent`)
    /// instead of producing no changes for them (`--diff-merges=off`).
    pub include_merges: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommitRow {
    pub sha: String,
    /// Seconds since the Unix epoch (UTC).
    pub author_time: i64,
    pub author_name: String,
    pub subject: String,
    pub parent_count: i32,
    pub is_merge: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileChangeRow {
    pub sha: String,
    pub path: String,
    pub old_path: Option<String>,
    pub status: String,
    pub added: Option<i32>,
    pub deleted: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HunkRow {
    pub sha: String,
    pub path: String,
    pub old_start: i32,
    pub old_lines: i32,
    pub new_start: i32,
    pub new_lines: i32,
}

/// Open the repository containing `path` (any directory inside a worktree,
/// or a bare repository).
pub fn open(path: &str) -> Result<Repository> {
    let mut repo = gix::discover(path)?;
    repo.object_cache_size_if_unset(64 * 1024 * 1024);
    Ok(repo)
}

/// The commits of `[since..]HEAD`, newest committer time first.
fn walk(repo: &Repository, range: &Range) -> Result<Vec<ObjectId>> {
    let head = repo.head_id()?.detach();
    let since = match &range.since {
        Some(rev) => Some(
            repo.rev_parse_single(rev.as_str())?
                .object()?
                .peel_to_commit()?
                .id,
        ),
        None => None,
    };
    let walk = repo
        .rev_walk([head])
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            CommitTimeOrder::NewestFirst,
        ))
        .with_hidden(since)
        .all()?;
    let mut out = Vec::new();
    for info in walk {
        out.push(info?.id);
    }
    Ok(out)
}

/// `git log [since..]HEAD --format=%H%at%an%P%s`.
pub fn commits(repo: &Repository, range: &Range) -> Result<Vec<CommitRow>> {
    let mut rows = Vec::new();
    for id in walk(repo, range)? {
        let commit = repo.find_commit(id)?;
        let author = commit.author()?;
        let parent_count = commit.parent_ids().count() as i32;
        rows.push(CommitRow {
            sha: id.to_string(),
            author_time: author.time()?.seconds,
            author_name: author.name.to_str_lossy().into_owned(),
            subject: subject(commit.message_raw()?),
            parent_count,
            is_merge: parent_count > 1,
        });
    }
    Ok(rows)
}

/// The first paragraph of a commit message with its lines joined (gix's
/// message summary).
fn subject(message: &BStr) -> String {
    gix::objs::commit::MessageRef::from_bytes(message)
        .summary()
        .to_str_lossy()
        .into_owned()
}

/// One non-tree change of a commit, as gix's tree diff with rewrites reports it.
struct Change {
    status: &'static str,
    path: String,
    /// The pre-image path (equal to `path` unless renamed).
    old_path: String,
    old: (ObjectId, EntryMode),
    new: (ObjectId, EntryMode),
}

fn lossy(path: &BStr) -> String {
    path.to_str_lossy().into_owned()
}

/// `T` when the kind of entry changes (file, symlink, submodule), `M` otherwise;
/// a change of the executable bit alone is `M`.
fn modification_status(old: EntryKind, new: EntryKind) -> &'static str {
    let class = |kind: EntryKind| match kind {
        EntryKind::Blob | EntryKind::BlobExecutable => 0,
        EntryKind::Link => 1,
        EntryKind::Commit => 2,
        EntryKind::Tree => 3,
    };
    if class(old) == class(new) { "M" } else { "T" }
}

/// The files one commit changes, with rename detection, or `None` for a merge
/// without `include_merges`. Root commits diff against the empty tree, merges
/// against their first parent.
fn commit_changes(
    repo: &Repository,
    cache: &mut gix::diff::blob::Platform,
    id: ObjectId,
    range: &Range,
) -> Result<Option<Vec<Change>>> {
    let commit = repo.find_commit(id)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
    let base = match parents.len() {
        0 => repo.empty_tree(),
        1 => repo.find_commit(parents[0])?.tree()?,
        _ if range.include_merges => repo.find_commit(parents[0])?.tree()?,
        _ => return Ok(None),
    };
    let tree = commit.tree()?;
    let null = ObjectId::null(repo.object_hash());
    let mut changes = Vec::new();
    // Renames only, at 50 % similarity, with up to 1000 candidates: `-M` at
    // git's defaults.
    let rewrites = Rewrites {
        copies: None,
        percentage: Some(0.5),
        limit: 1000,
        track_empty: false,
    };
    gix::diff::tree_with_rewrites(
        gix::objs::TreeRefIter::from_bytes(&base.data, repo.object_hash()),
        gix::objs::TreeRefIter::from_bytes(&tree.data, repo.object_hash()),
        cache,
        &mut Default::default(),
        &repo.objects,
        |change: ChangeRef<'_>| -> gix::error::Result<Action> {
            let change = match change {
                ChangeRef::Addition {
                    location,
                    entry_mode,
                    id,
                    ..
                } => Change {
                    status: "A",
                    path: lossy(location),
                    old_path: lossy(location),
                    old: (null, entry_mode),
                    new: (id, entry_mode),
                },
                ChangeRef::Deletion {
                    location,
                    entry_mode,
                    id,
                    ..
                } => Change {
                    status: "D",
                    path: lossy(location),
                    old_path: lossy(location),
                    old: (id, entry_mode),
                    new: (null, entry_mode),
                },
                ChangeRef::Modification {
                    location,
                    previous_entry_mode,
                    previous_id,
                    entry_mode,
                    id,
                } => Change {
                    status: modification_status(previous_entry_mode.kind(), entry_mode.kind()),
                    path: lossy(location),
                    old_path: lossy(location),
                    old: (previous_id, previous_entry_mode),
                    new: (id, entry_mode),
                },
                ChangeRef::Rewrite {
                    source_location,
                    source_entry_mode,
                    source_id,
                    entry_mode,
                    location,
                    id,
                    copy,
                    ..
                } => Change {
                    status: if copy { "C" } else { "R" },
                    path: lossy(location),
                    old_path: lossy(source_location),
                    old: (source_id, source_entry_mode),
                    new: (id, entry_mode),
                },
            };
            if !(change.old.1.is_tree() || change.new.1.is_tree()) {
                changes.push(change);
            }
            Ok(std::ops::ControlFlow::Continue(()))
        },
        gix::diff::tree_with_rewrites::Options {
            location: Some(gix::diff::tree::recorder::Location::Path),
            rewrites: Some(rewrites),
        },
    )?;
    Ok(Some(changes))
}

/// The line diff of one change: hunk headers and added/deleted counts.
#[derive(Debug, Default)]
struct LineDiff {
    hunks: Vec<[i32; 4]>,
    added: i32,
    deleted: i32,
}

/// Collects the `@@` numbers of `UnifiedDiff`'s hunks.
struct Headers(Vec<[i32; 4]>);

impl ConsumeHunk for Headers {
    type Out = Vec<[i32; 4]>;

    fn consume_hunk(
        &mut self,
        h: HunkHeader,
        _lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        self.0.push(empty_side_before([
            h.before_hunk_start as i32,
            h.before_hunk_len as i32,
            h.after_hunk_start as i32,
            h.after_hunk_len as i32,
        ]));
        Ok(())
    }

    fn finish(self) -> Self::Out {
        self.0
    }
}

/// `UnifiedDiff` numbers an empty side like a non-empty one; a unified diff
/// header names the line before it instead.
fn empty_side_before(mut header: [i32; 4]) -> [i32; 4] {
    for side in [0, 2] {
        if header[side + 1] == 0 {
            header[side] -= 1;
        }
    }
    header
}

/// The `@@` numbers of a zero-context hunk given its 0-based line ranges:
/// 1-based starts, and an empty side names the line before.
fn header(before: &Lines<u32>, after: &Lines<u32>) -> [i32; 4] {
    let start = |r: &Lines<u32>| if r.is_empty() { r.start } else { r.start + 1 };
    [
        start(before) as i32,
        before.len() as i32,
        start(after) as i32,
        after.len() as i32,
    ]
}

/// Run gix's blob platform on one change. `None` means gix declined to diff
/// it: one side is binary, or a side is a submodule (the blob platform
/// rejects gitlinks).
fn change_diff(
    repo: &Repository,
    cache: &mut gix::diff::blob::Platform,
    change: &Change,
    hunk_context: Option<u32>,
) -> Result<Option<LineDiff>> {
    if change.old.1.is_commit() || change.new.1.is_commit() {
        return Ok(None);
    }
    cache.set_resource(
        change.old.0,
        change.old.1.kind(),
        change.old_path.as_bytes().as_bstr(),
        ResourceKind::OldOrSource,
        &repo.objects,
    )?;
    cache.set_resource(
        change.new.0,
        change.new.1.kind(),
        change.path.as_bytes().as_bstr(),
        ResourceKind::NewOrDestination,
        &repo.objects,
    )?;
    let prep = cache.prepare_diff()?;
    let algorithm = match prep.operation {
        Operation::InternalDiff { algorithm } => algorithm,
        Operation::SourceOrDestinationIsBinary | Operation::ExternalCommand { .. } => {
            return Ok(None);
        }
    };
    let input = prep.interned_input();
    let mut diff = Diff::compute(algorithm, &input);
    // The counts do not depend on where the slider post-processing below
    // places the changes.
    let mut out = LineDiff {
        added: diff.count_additions() as i32,
        deleted: diff.count_removals() as i32,
        ..LineDiff::default()
    };
    if let Some(context) = hunk_context {
        diff.postprocess_lines(&input);
        out.hunks = hunk_headers(&diff, &input, context)?;
    }
    Ok(Some(out))
}

fn hunk_headers(diff: &Diff, input: &InternedInput<&[u8]>, context: u32) -> Result<Vec<[i32; 4]>> {
    if context == 0 {
        return Ok(diff.hunks().map(|h| header(&h.before, &h.after)).collect());
    }
    Ok(UnifiedDiff::new(
        diff,
        input,
        Headers(Vec::new()),
        ContextSize::symmetrical(context),
    )
    .consume()?)
}

fn for_each_change(
    repo: &Repository,
    range: &Range,
    mut each: impl FnMut(&str, &Change, &mut gix::diff::blob::Platform) -> Result<()>,
) -> Result<()> {
    let mut cache = repo.diff_resource_cache_for_tree_diff()?;
    cache.options.algorithm = Some(Algorithm::Myers);
    for id in walk(repo, range)? {
        let sha = id.to_string();
        for change in commit_changes(repo, &mut cache, id, range)?.unwrap_or_default() {
            each(&sha, &change, &mut cache)?;
        }
        cache.clear_resource_cache_keep_allocation();
    }
    Ok(())
}

/// `git log --raw --numstat -M`: one row per changed file; NULL counts for
/// binary files and submodules.
pub fn file_changes(repo: &Repository, range: &Range) -> Result<Vec<FileChangeRow>> {
    let mut rows = Vec::new();
    for_each_change(repo, range, |sha, change, cache| {
        let counts = change_diff(repo, cache, change, None)?;
        rows.push(FileChangeRow {
            sha: sha.to_string(),
            path: change.path.clone(),
            old_path: matches!(change.status, "R" | "C").then(|| change.old_path.clone()),
            status: change.status.to_string(),
            added: counts.as_ref().map(|c| c.added),
            deleted: counts.as_ref().map(|c| c.deleted),
        });
        Ok(())
    })?;
    Ok(rows)
}

/// `git log -p -U0 -M`: one row per `@@ -a,b +c,d @@` header, for every file.
pub fn hunks(repo: &Repository, range: &Range) -> Result<Vec<HunkRow>> {
    hunks_with_context(repo, range, 0)
}

/// [`hunks`] with `context` lines around each change (`-U<context>`); hunks
/// closer than that are merged as a unified diff merges them.
pub fn hunks_with_context(repo: &Repository, range: &Range, context: u32) -> Result<Vec<HunkRow>> {
    let mut rows = Vec::new();
    for_each_change(repo, range, |sha, change, cache| {
        let Some(diff) = change_diff(repo, cache, change, Some(context))? else {
            return Ok(());
        };
        for [old_start, old_lines, new_start, new_lines] in diff.hunks {
            rows.push(HunkRow {
                sha: sha.to_string(),
                path: change.path.clone(),
                old_start,
                old_lines,
                new_start,
                new_lines,
            });
        }
        Ok(())
    })?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_joins_the_first_paragraph() {
        assert_eq!(
            subject(b"first\nwraps here  \n\nbody\n".as_bstr()),
            "first wraps here"
        );
        assert_eq!(subject(b"only".as_bstr()), "only");
        assert_eq!(subject(b"".as_bstr()), "");
    }

    #[test]
    fn status_is_t_only_when_the_entry_kind_changes() {
        use EntryKind::*;
        assert_eq!(modification_status(Blob, Blob), "M");
        assert_eq!(modification_status(Blob, BlobExecutable), "M");
        assert_eq!(modification_status(Blob, Link), "T");
        assert_eq!(modification_status(Link, BlobExecutable), "T");
        assert_eq!(modification_status(Commit, Blob), "T");
    }

    #[test]
    fn zero_context_headers_are_one_based_and_empty_sides_name_the_line_before() {
        // A replaced line 2.
        assert_eq!(header(&(1..2), &(1..2)), [2, 1, 2, 1]);
        // Two lines inserted after line 1: `@@ -1,0 +2,2 @@`.
        assert_eq!(header(&(1..1), &(1..3)), [1, 0, 2, 2]);
        // A whole new file: `@@ -0,0 +1,10 @@`.
        assert_eq!(header(&(0..0), &(0..10)), [0, 0, 1, 10]);
        // Lines 5-6 deleted: `@@ -5,2 +4,0 @@`.
        assert_eq!(header(&(4..6), &(4..4)), [5, 2, 4, 0]);
    }

    #[test]
    fn unified_headers_move_an_empty_side_to_the_line_before() {
        assert_eq!(empty_side_before([1, 0, 1, 10]), [0, 0, 1, 10]);
        assert_eq!(empty_side_before([3, 4, 5, 0]), [3, 4, 4, 0]);
        assert_eq!(empty_side_before([3, 4, 5, 6]), [3, 4, 5, 6]);
    }
}
