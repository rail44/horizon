//! Commits, file changes and `-U0` hunks read through libgit2.
//!
//! Each function reproduces one `git log` invocation of the code database
//! spike (`scripts/code-db/history.py`); the doc comment of each names it.
//! libgit2 supplies the object access, the tree diff and the line diff
//! (its xdiff is git's); the steps where git's diff machinery differs from
//! libgit2's are reimplemented here, each naming the git behavior it copies.

use std::borrow::Cow;
use std::path::Path;

use crate::{renames, revlist};
use git2::{
    AttrCheckFlags, AttrValue, Commit, Delta, DiffOptions, FileMode, Oid, Patch, Repository, Tree,
};

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

/// Open the repository containing `path` (any directory inside a worktree).
pub fn open(path: &str) -> Result<Repository, git2::Error> {
    Repository::discover(Path::new(path))
}

fn walk(repo: &Repository, range: &Range) -> Result<Vec<Oid>, git2::Error> {
    let since = match &range.since {
        Some(since) => Some(repo.revparse_single(since)?.peel_to_commit()?.id()),
        None => None,
    };
    revlist::rev_list(repo, since)
}

/// git's `%s`: the first paragraph after leading blank lines, its lines
/// stripped of trailing whitespace and joined with single spaces.
fn subject(message: &[u8]) -> String {
    let mut out = Vec::new();
    let mut started = false;
    for line in message.split(|&b| b == b'\n') {
        let trimmed = trim_end(line);
        if trimmed.is_empty() {
            if started {
                break;
            }
            continue;
        }
        if started {
            out.push(b' ');
        }
        out.extend_from_slice(trimmed);
        started = true;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn trim_end(line: &[u8]) -> &[u8] {
    let end = line
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    &line[..end]
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `git log [since..]HEAD --format=%H%at%an%P%s`, in `git log` order.
pub fn commits(repo: &Repository, range: &Range) -> Result<Vec<CommitRow>, git2::Error> {
    let mut rows = Vec::new();
    for oid in walk(repo, range)? {
        let commit = repo.find_commit(oid)?;
        let parent_count = commit.parent_count() as i32;
        rows.push(CommitRow {
            sha: commit.id().to_string(),
            author_time: commit.author().when().seconds(),
            author_name: lossy(commit.author().name_bytes()),
            subject: subject(commit.message_raw_bytes()),
            parent_count,
            is_merge: parent_count > 1,
        });
    }
    Ok(rows)
}

/// The tree a commit is diffed against, or `None` when it produces no diff:
/// merges without `include_merges` (`--diff-merges=off`). Root commits diff
/// against the empty tree (`log.showRoot`).
fn base_tree<'r>(
    commit: &Commit<'r>,
    range: &Range,
) -> Result<Option<Option<Tree<'r>>>, git2::Error> {
    match commit.parent_count() {
        0 => Ok(Some(None)),
        1 => Ok(Some(Some(commit.parent(0)?.tree()?))),
        _ if range.include_merges => Ok(Some(Some(commit.parent(0)?.tree()?))),
        _ => Ok(None),
    }
}

/// One side of a changed file; `id` is zero when the side does not exist.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Side {
    pub(crate) id: Oid,
    pub(crate) mode: FileMode,
}

/// One changed file of a commit, as `--raw` lists it.
#[derive(Debug, Clone)]
pub(crate) struct Change {
    pub(crate) status: Delta,
    /// The pre-image path (equal to `path` unless renamed or copied).
    pub(crate) old_path: String,
    /// The post-image path (the pre-image path for deletions).
    pub(crate) path: String,
    pub(crate) old: Side,
    pub(crate) new: Side,
}

/// The files one commit changes, with `-M` rename detection, or `None`
/// (see [`base_tree`]).
fn commit_changes(
    repo: &Repository,
    commit: &Commit<'_>,
    range: &Range,
) -> Result<Option<Vec<Change>>, git2::Error> {
    let Some(base) = base_tree(commit, range)? else {
        return Ok(None);
    };
    let mut options = DiffOptions::new();
    options.include_typechange(true);
    let diff = repo.diff_tree_to_tree(base.as_ref(), Some(&commit.tree()?), Some(&mut options))?;
    let side = |file: git2::DiffFile<'_>| Side {
        id: file.id(),
        mode: file.mode(),
    };
    let mut changes: Vec<Change> = diff
        .deltas()
        .map(|delta| {
            let old_path = lossy(delta.old_file().path_bytes().unwrap_or_default());
            let path = if delta.status() == Delta::Deleted {
                old_path.clone()
            } else {
                lossy(delta.new_file().path_bytes().unwrap_or_default())
            };
            Change {
                status: delta.status(),
                old_path,
                path,
                old: side(delta.old_file()),
                new: side(delta.new_file()),
            }
        })
        .collect();
    renames::detect(repo, &mut changes)?;
    Ok(Some(changes))
}

/// The bytes git diffs for one side: the blob, or for a submodule the line
/// git synthesizes for it (`diff.c`, `diff_populate_gitlink`).
fn content<'r>(repo: &'r Repository, side: Side) -> Result<Cow<'r, [u8]>, git2::Error> {
    if side.id.is_zero() {
        return Ok(Cow::Borrowed(&[]));
    }
    if side.mode == FileMode::Commit {
        return Ok(Cow::Owned(
            format!("Subproject commit {}\n", side.id).into_bytes(),
        ));
    }
    Ok(Cow::Owned(repo.find_blob(side.id)?.content().to_vec()))
}

/// git's `diff_filespec_is_binary` without textconv: the `diff` attribute
/// when set (`-diff`, or the `binary` macro), else a NUL byte in the first
/// 8000 bytes (`buffer_is_binary`).
pub(crate) fn is_binary(repo: &Repository, path: &str, data: &[u8]) -> bool {
    let attribute = repo
        .get_attr_bytes(Path::new(path), "diff", AttrCheckFlags::FILE_THEN_INDEX)
        .ok()
        .flatten();
    match AttrValue::from_bytes(attribute) {
        AttrValue::False => true,
        AttrValue::True => false,
        _ => data[..data.len().min(8000)].contains(&0),
    }
}

/// git's `trim_common_tail` (`xdiff-interface.c`), applied by `xdi_diff`
/// whenever the diff has no context lines (`-U0`, and `--numstat`): drop
/// the common tail in whole 1024-byte blocks, then give back the partial
/// line at the cut. It bounds how far xdiff can slide a change downwards,
/// so without it `-U0` hunk positions differ from git's.
fn trim_common_tail<'a>(a: &'a [u8], b: &'a [u8]) -> (&'a [u8], &'a [u8]) {
    const BLOCK: usize = 1024;
    let smaller = a.len().min(b.len());
    let mut trimmed = 0;
    while BLOCK + trimmed <= smaller
        && a[a.len() - trimmed - BLOCK..a.len() - trimmed]
            == b[b.len() - trimmed - BLOCK..b.len() - trimmed]
    {
        trimmed += BLOCK;
    }
    let tail = &a[a.len() - trimmed..];
    let recovered = match tail.iter().position(|&c| c == b'\n') {
        Some(index) => index + 1,
        None => trimmed,
    };
    let cut = trimmed - recovered;
    (&a[..a.len() - cut], &b[..b.len() - cut])
}

/// The `-U0` line diff of one file pair: `@@` ranges and added/deleted counts.
#[derive(Debug, Default)]
struct LineDiff {
    hunks: Vec<[i32; 4]>,
    added: i32,
    deleted: i32,
}

/// `trim` applies [`trim_common_tail`]: git does for `-U0` patches, but not
/// for `--numstat`, which runs with the default three context lines.
fn line_diff(old: &[u8], new: &[u8], trim: bool) -> Result<LineDiff, git2::Error> {
    let (old, new) = if trim {
        trim_common_tail(old, new)
    } else {
        (old, new)
    };
    let mut options = DiffOptions::new();
    // git's defaults for `git log -p`: Myers, diff.indentHeuristic=true.
    options
        .context_lines(0)
        .interhunk_lines(0)
        .indent_heuristic(true)
        .force_text(true);
    let patch = Patch::from_buffers(old, None, new, None, Some(&mut options))?;
    let mut diff = LineDiff::default();
    for index in 0..patch.num_hunks() {
        let (hunk, _) = patch.hunk(index)?;
        diff.hunks.push([
            hunk.old_start() as i32,
            hunk.old_lines() as i32,
            hunk.new_start() as i32,
            hunk.new_lines() as i32,
        ]);
    }
    let (_, added, deleted) = patch.line_stats()?;
    diff.added = added as i32;
    diff.deleted = deleted as i32;
    Ok(diff)
}

/// The line diff of one change, or `None` for a binary file.
fn change_diff(
    repo: &Repository,
    change: &Change,
    trim: bool,
) -> Result<Option<LineDiff>, git2::Error> {
    let old = content(repo, change.old)?;
    let new = content(repo, change.new)?;
    if is_binary(repo, &change.old_path, &old) || is_binary(repo, &change.path, &new) {
        return Ok(None);
    }
    if old == new {
        // Pure renames and mode changes (`may_differ` in `builtin_diffstat`).
        return Ok(Some(LineDiff::default()));
    }
    Ok(Some(line_diff(&old, &new, trim)?))
}

fn status_letter(status: Delta) -> &'static str {
    match status {
        Delta::Added => "A",
        Delta::Deleted => "D",
        Delta::Modified => "M",
        Delta::Renamed => "R",
        Delta::Copied => "C",
        Delta::Typechange => "T",
        Delta::Unmodified => " ",
        Delta::Ignored => "!",
        Delta::Untracked => "?",
        Delta::Unreadable => "X",
        Delta::Conflicted => "U",
    }
}

fn for_each_change(
    repo: &Repository,
    range: &Range,
    mut each: impl FnMut(&str, &Change) -> Result<(), git2::Error>,
) -> Result<(), git2::Error> {
    for oid in walk(repo, range)? {
        let commit = repo.find_commit(oid)?;
        let sha = oid.to_string();
        for change in commit_changes(repo, &commit, range)?.unwrap_or_default() {
            each(&sha, &change)?;
        }
    }
    Ok(())
}

/// `git log --raw --numstat -M`: one row per changed file, NULL counts for binary files.
pub fn file_changes(repo: &Repository, range: &Range) -> Result<Vec<FileChangeRow>, git2::Error> {
    let mut rows = Vec::new();
    for_each_change(repo, range, |sha, change| {
        let counts = change_diff(repo, change, false)?.map(|diff| (diff.added, diff.deleted));
        rows.push(FileChangeRow {
            sha: sha.to_string(),
            path: change.path.clone(),
            old_path: matches!(change.status, Delta::Renamed | Delta::Copied)
                .then(|| change.old_path.clone()),
            status: status_letter(change.status).to_string(),
            added: counts.map(|(added, _)| added),
            deleted: counts.map(|(_, deleted)| deleted),
        });
        Ok(())
    })?;
    Ok(rows)
}

/// `git log -p -U0 -M`: one row per `@@ -a,b +c,d @@` header, for every file.
pub fn hunks(repo: &Repository, range: &Range) -> Result<Vec<HunkRow>, git2::Error> {
    let mut rows = Vec::new();
    for_each_change(repo, range, |sha, change| {
        let diff = if change.status == Delta::Typechange {
            // A file/symlink change is printed as a deletion followed by a
            // creation (`diff.c`, `run_diff`), so its hunks are those of the halves.
            let absent = Side {
                id: Oid::zero(),
                mode: FileMode::Unreadable,
            };
            let deletion = Change {
                new: absent,
                ..change.clone()
            };
            let creation = Change {
                old: absent,
                ..change.clone()
            };
            match (
                change_diff(repo, &deletion, true)?,
                change_diff(repo, &creation, true)?,
            ) {
                (Some(mut first), Some(second)) => {
                    first.hunks.extend(second.hunks);
                    Some(first)
                }
                (first, second) => first.or(second),
            }
        } else {
            change_diff(repo, change, true)?
        };
        let Some(diff) = diff else {
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
    use super::{subject, trim_common_tail};

    #[test]
    fn subject_joins_the_first_paragraph() {
        assert_eq!(
            subject(b"\n\nfirst  \n  second\n\nbody\n"),
            "first   second"
        );
        assert_eq!(subject(b"only"), "only");
        assert_eq!(subject(b""), "");
    }

    #[test]
    fn trim_common_tail_cuts_whole_blocks_and_keeps_the_cut_line() {
        let tail = "x\n".repeat(1024);
        let a = format!("a\n{tail}");
        let b = format!("b\nb\n{tail}");
        // Two 1024-byte blocks are common; the first line of the cut is given back.
        assert_eq!(
            trim_common_tail(a.as_bytes(), b.as_bytes()),
            (&b"a\nx\n"[..], &b"b\nb\nx\n"[..])
        );
        // Short files are left alone.
        assert_eq!(
            trim_common_tail(b"a\nz\n", b"b\nz\n"),
            (&b"a\nz\n"[..], &b"b\nz\n"[..])
        );
    }
}
