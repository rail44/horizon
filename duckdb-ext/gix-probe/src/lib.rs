//! Print the rows of `git_hunks`, `git_file_changes` and `git_commits` as TSV,
//! computed with gitoxide (`gix`) only, to measure how close gix comes to
//! `git log` without the git-derived logic of the `git_history` extension.
//!
//! Usage: gix-probe REPO hunks|file_changes|commits [options]
//!
//! Options:
//!   --since REV          history is REV..HEAD
//!   --include-merges     diff merges against their first parent
//!   --sorting MODE       commit walk: breadth-first, commit-time (default),
//!                        commit-time-oldest, topo-date, topo
//!   --slider MODE        hunk post-processing: indent (default, gix's
//!                        `diff_with_slider_heuristics`), none
//!                        (`postprocess_no_heuristic`), raw (no post-processing)
//!   --unified-header     take hunk numbers from gix's `UnifiedDiff` with zero
//!                        context instead of the hunk ranges
//!   --context N          take hunk numbers from `UnifiedDiff` with N context
//!                        lines, an empty side numbered the way git does
//!   --no-commit-graph    `rev_walk(..).use_commit_graph(false)` (the simple
//!                        walks use the commit-graph when present by default)
//!   --track-empty        `Rewrites::track_empty = true`
//!   --stats              print the total time to stderr

use gix::bstr::{BStr, ByteSlice};
use gix::diff::Rewrites;
use gix::diff::blob::platform::prepare_diff::Operation;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{Algorithm, Diff, InternedInput, ResourceKind, UnifiedDiff};
use gix::diff::tree_with_rewrites::{Action, ChangeRef};
use gix::objs::tree::{EntryKind, EntryMode};
use gix::traverse::commit::simple::CommitTimeOrder;
use gix::{ObjectId, Repository};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sorting {
    BreadthFirst,
    CommitTime,
    CommitTimeOldest,
    TopoDate,
    Topo,
}

impl Sorting {
    pub const ALL: [Sorting; 5] = [
        Sorting::BreadthFirst,
        Sorting::CommitTime,
        Sorting::CommitTimeOldest,
        Sorting::TopoDate,
        Sorting::Topo,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Sorting::BreadthFirst => "breadth-first",
            Sorting::CommitTime => "commit-time",
            Sorting::CommitTimeOldest => "commit-time-oldest",
            Sorting::TopoDate => "topo-date",
            Sorting::Topo => "topo",
        }
    }

    pub fn parse(name: &str) -> Result<Self> {
        Sorting::ALL
            .into_iter()
            .find(|s| s.name() == name)
            .ok_or_else(|| format!("unknown sorting {name}").into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slider {
    Indent,
    None,
    Raw,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub since: Option<String>,
    pub include_merges: bool,
    pub sorting: Sorting,
    pub slider: Slider,
    pub unified_header: bool,
    pub context: Option<u32>,
    pub track_empty: bool,
    pub no_commit_graph: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            since: None,
            include_merges: false,
            sorting: Sorting::CommitTime,
            slider: Slider::Indent,
            unified_header: false,
            context: None,
            track_empty: false,
            no_commit_graph: false,
        }
    }
}

pub fn open(path: &str) -> Result<Repository> {
    let mut repo = gix::discover(path)?;
    repo.object_cache_size_if_unset(64 * 1024 * 1024);
    Ok(repo)
}

/// The commits of `[since..]HEAD` in the order of the chosen gix walk.
pub fn walk(repo: &Repository, options: &Options) -> Result<Vec<ObjectId>> {
    let head = repo.head_id()?.detach();
    let since = match &options.since {
        Some(rev) => Some(
            repo.rev_parse_single(rev.as_str())?
                .object()?
                .peel_to_commit()?
                .id,
        ),
        None => None,
    };
    let mut out = Vec::new();
    match options.sorting {
        Sorting::TopoDate | Sorting::Topo => {
            let sorting = if options.sorting == Sorting::Topo {
                gix::traverse::commit::topo::Sorting::TopoOrder
            } else {
                gix::traverse::commit::topo::Sorting::DateOrder
            };
            let walk = gix::traverse::commit::topo::Builder::from_iters(
                &repo.objects,
                [head],
                since.map(|s| [s]),
            )
            .sorting(sorting)
            .build()?;
            for info in walk {
                out.push(info?.id);
            }
        }
        simple => {
            let sorting = match simple {
                Sorting::BreadthFirst => gix::revision::walk::Sorting::BreadthFirst,
                Sorting::CommitTime => {
                    gix::revision::walk::Sorting::ByCommitTime(CommitTimeOrder::NewestFirst)
                }
                _ => gix::revision::walk::Sorting::ByCommitTime(CommitTimeOrder::OldestFirst),
            };
            let mut walk = repo.rev_walk([head]).sorting(sorting).with_hidden(since);
            if options.no_commit_graph {
                walk = walk.use_commit_graph(false);
            }
            let walk = walk.all()?;
            for info in walk {
                out.push(info?.id);
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommitRow {
    pub sha: String,
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

pub fn commits(repo: &Repository, options: &Options) -> Result<Vec<CommitRow>> {
    let mut rows = Vec::new();
    for id in walk(repo, options)? {
        let commit = repo.find_commit(id)?;
        let author = commit.author()?;
        let parent_count = commit.parent_ids().count() as i32;
        rows.push(CommitRow {
            sha: id.to_string(),
            author_time: author.time()?.seconds,
            author_name: author.name.to_str_lossy().into_owned(),
            subject: commit.message()?.summary().to_str_lossy().into_owned(),
            parent_count,
            is_merge: parent_count > 1,
        });
    }
    Ok(rows)
}

/// One non-tree change of a commit, as gix's tree diff with rewrites reports it.
struct Change {
    status: &'static str,
    path: String,
    old_path: String,
    old: (ObjectId, EntryMode),
    new: (ObjectId, EntryMode),
}

fn lossy(path: &BStr) -> String {
    path.to_str_lossy().into_owned()
}

/// git prints `T` when the object type changes (file, symlink, gitlink), `M` otherwise.
fn modification_status(old: EntryMode, new: EntryMode) -> &'static str {
    let class = |mode: EntryMode| match mode.kind() {
        EntryKind::Blob | EntryKind::BlobExecutable => 0,
        EntryKind::Link => 1,
        EntryKind::Commit => 2,
        EntryKind::Tree => 3,
    };
    if class(old) == class(new) { "M" } else { "T" }
}

fn commit_changes(
    repo: &Repository,
    cache: &mut gix::diff::blob::Platform,
    id: ObjectId,
    options: &Options,
) -> Result<Option<Vec<Change>>> {
    let commit = repo.find_commit(id)?;
    let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
    let base = match parents.len() {
        0 => repo.empty_tree(),
        1 => repo.find_commit(parents[0])?.tree()?,
        _ if options.include_merges => repo.find_commit(parents[0])?.tree()?,
        _ => return Ok(None),
    };
    let tree = commit.tree()?;
    let null = ObjectId::null(repo.object_hash());
    let mut changes = Vec::new();
    let rewrites = Rewrites {
        copies: None,
        percentage: Some(0.5),
        limit: 1000,
        track_empty: options.track_empty,
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
                    status: modification_status(previous_entry_mode, entry_mode),
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

/// The line diff of one change: git-style `@@` numbers and added/deleted counts.
#[derive(Debug, Default)]
struct LineDiff {
    hunks: Vec<[i32; 4]>,
    added: i32,
    deleted: i32,
}

struct Headers(Vec<[i32; 4]>);

impl ConsumeHunk for Headers {
    type Out = Vec<[i32; 4]>;

    fn consume_hunk(
        &mut self,
        h: HunkHeader,
        _lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        self.0.push([
            h.before_hunk_start as i32,
            h.before_hunk_len as i32,
            h.after_hunk_start as i32,
            h.after_hunk_len as i32,
        ]);
        Ok(())
    }

    fn finish(self) -> Self::Out {
        self.0
    }
}

/// Run gix's blob platform on one change. `None` means gix declined to diff it:
/// one side is binary, or a side is a submodule (gix's blob platform refuses
/// gitlinks with `InvalidMode`).
fn change_diff(
    repo: &Repository,
    cache: &mut gix::diff::blob::Platform,
    change: &Change,
    options: &Options,
    want_hunks: bool,
) -> Result<Option<LineDiff>> {
    if change.old.1.is_commit() || change.new.1.is_commit() {
        return Ok(None);
    }
    let kind = |side: (ObjectId, EntryMode)| side.1.kind();
    cache.set_resource(
        change.old.0,
        kind(change.old),
        change.old_path.as_bytes().as_bstr(),
        ResourceKind::OldOrSource,
        &repo.objects,
    )?;
    cache.set_resource(
        change.new.0,
        kind(change.new),
        change.path.as_bytes().as_bstr(),
        ResourceKind::NewOrDestination,
        &repo.objects,
    )?;
    let prep = cache.prepare_diff()?;
    let algorithm = match prep.operation {
        Operation::InternalDiff { algorithm } => algorithm,
        Operation::SourceOrDestinationIsBinary => return Ok(None),
        Operation::ExternalCommand { .. } => return Ok(None),
    };
    let input = prep.interned_input();
    let mut diff = Diff::compute(algorithm, &input);
    let mut out = LineDiff {
        added: diff.count_additions() as i32,
        deleted: diff.count_removals() as i32,
        ..LineDiff::default()
    };
    if want_hunks {
        match options.slider {
            Slider::Indent => diff.postprocess_lines(&input),
            Slider::None => diff.postprocess_no_heuristic(&input),
            Slider::Raw => {}
        }
        out.hunks = hunk_numbers(&diff, &input, options)?;
    }
    Ok(Some(out))
}

fn hunk_numbers(
    diff: &Diff,
    input: &InternedInput<&[u8]>,
    options: &Options,
) -> Result<Vec<[i32; 4]>> {
    if options.unified_header {
        return Ok(UnifiedDiff::new(
            diff,
            input,
            Headers(Vec::new()),
            ContextSize::symmetrical(0),
        )
        .consume()?);
    }
    if let Some(context) = options.context {
        let mut headers = UnifiedDiff::new(
            diff,
            input,
            Headers(Vec::new()),
            ContextSize::symmetrical(context),
        )
        .consume()?;
        // `UnifiedDiff` numbers an empty side like a non-empty one; git names the line before.
        for h in &mut headers {
            for side in [0, 2] {
                if h[side + 1] == 0 {
                    h[side] -= 1;
                }
            }
        }
        return Ok(headers);
    }
    // git's `@@` numbering: 1-based starts, and an empty side names the line before.
    let start = |r: &std::ops::Range<u32>| if r.is_empty() { r.start } else { r.start + 1 };
    Ok(diff
        .hunks()
        .map(|h| {
            [
                start(&h.before) as i32,
                h.before.len() as i32,
                start(&h.after) as i32,
                h.after.len() as i32,
            ]
        })
        .collect())
}

fn for_each_change(
    repo: &Repository,
    options: &Options,
    mut each: impl FnMut(&str, &Change, &mut gix::diff::blob::Platform) -> Result<()>,
) -> Result<()> {
    let mut cache = repo.diff_resource_cache_for_tree_diff()?;
    cache.options.algorithm = Some(Algorithm::Myers);
    for id in walk(repo, options)? {
        let sha = id.to_string();
        for change in commit_changes(repo, &mut cache, id, options)?.unwrap_or_default() {
            each(&sha, &change, &mut cache)?;
        }
        cache.clear_resource_cache_keep_allocation();
    }
    Ok(())
}

pub fn file_changes(repo: &Repository, options: &Options) -> Result<Vec<FileChangeRow>> {
    let mut rows = Vec::new();
    for_each_change(repo, options, |sha, change, cache| {
        let counts = change_diff(repo, cache, change, options, false)?;
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

pub fn hunks(repo: &Repository, options: &Options) -> Result<Vec<HunkRow>> {
    let mut rows = Vec::new();
    for_each_change(repo, options, |sha, change, cache| {
        let Some(diff) = change_diff(repo, cache, change, options, true)? else {
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
