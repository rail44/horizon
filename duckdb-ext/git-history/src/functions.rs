//! The DuckDB table functions over [`git_history_core`].
//!
//! Every function materializes its rows once in the global init (so `DESCRIBE`
//! and binding stay cheap) and emits them in order, one chunk per call, from a
//! single thread so the output keeps `git log` order.

use std::sync::Mutex;

use duckdb_neo::{
    DuckDBType, Error, Result,
    bind_arguments::BindArgument,
    connection::Context,
    data_chunk::DataChunkRef,
    extension::Extension,
    signature::{
        FunctionDocumentationBuilder, FunctionParameter, FunctionParameterKind, SignatureBuilder,
    },
    table_function::{
        BindFunctionHandle, ExecColumnInfo, InitColumnData, TableFunctionBuilder,
        TableFunctionCallbacks,
    },
    types::TimestampValue,
    vector::{Unknown, Vector},
};

use git_history_core::{
    self as history, ChangedBlobRow, CommitRow, FileChangeRow, HunkRow, Range, TrackedFileRow,
    TreeRow,
};

/// Arguments shared by the three history functions, resolved at bind time.
pub(crate) struct Params {
    repo: String,
    range: Range,
}

/// One table function: its columns, how to compute its rows, and how to write a row.
trait GitTable: Send + Sync + 'static {
    type Row: Send + Sync + 'static;
    const NAME: &'static str;
    const DESCRIPTION: &'static str;

    fn columns(
        context: &Context,
    ) -> Result<Vec<(&'static str, duckdb_neo::logical_type::LogicalType)>>;
    fn rows(repo: &history::Repository, range: &Range) -> history::Result<Vec<Self::Row>>;
    fn write(rows: &[Self::Row], columns: [Vector<'_, Unknown>; 6]) -> Result<()>;
}

/// Rows of one scan and the index of the next row to emit.
pub(crate) struct Scan<R> {
    rows: Vec<R>,
    next: usize,
}

struct Function<T>(std::marker::PhantomData<T>);

fn git_error(error: history::Error) -> Error {
    Error::invalid_input(error.to_string())
}

impl<T: GitTable> TableFunctionCallbacks for Function<T> {
    type BindData = Params;
    type LocalState = ();
    type GlobalState = Mutex<Scan<T::Row>>;

    fn bind(
        &self,
        context: &Context,
        arguments: Vec<BindArgument>,
        bind: BindFunctionHandle<'_>,
    ) -> Result<Params> {
        let value = |index: usize| {
            arguments
                .get(index)
                .and_then(|argument| argument.value.as_ref())
        };
        let repo = value(0)
            .map(|value| value.get::<String>())
            .transpose()?
            .flatten()
            .ok_or_else(|| {
                Error::invalid_input(format!(
                    "{}: repo must be a constant, non-NULL path",
                    T::NAME
                ))
            })?;
        let since = value(1)
            .map(|value| value.get::<String>())
            .transpose()?
            .flatten();
        let include_merges = value(2)
            .map(|value| value.get::<bool>())
            .transpose()?
            .flatten()
            .unwrap_or(false);
        // Fail at bind time on a path outside any repository.
        history::open(&repo).map_err(git_error)?;
        for (name, logical_type) in T::columns(context)? {
            bind.add_result_column(name, &logical_type)?;
        }
        Ok(Params {
            repo,
            range: Range {
                since,
                include_merges,
            },
        })
    }

    fn init_global_state(
        &self,
        params: &Params,
        _context: &Context,
        _columns: InitColumnData<'_>,
    ) -> Result<(Option<Self::GlobalState>, Option<usize>)> {
        let repo = history::open(&params.repo).map_err(git_error)?;
        let rows = T::rows(&repo, &params.range).map_err(git_error)?;
        Ok((Some(Mutex::new(Scan { rows, next: 0 })), Some(1)))
    }

    fn exec(
        &self,
        _params: &Params,
        global: Option<&Self::GlobalState>,
        _local: Option<&mut ()>,
        _context: &Context,
        mut output: DataChunkRef<'_>,
        _columns: ExecColumnInfo<'_>,
    ) -> Result<()> {
        let mut scan = global
            .ok_or_else(|| Error::internal("scan state missing"))?
            .lock()
            .map_err(|_| Error::internal("scan state poisoned"))?;
        let start = scan.next;
        let end = scan.rows.len().min(start + output.capacity()?);
        scan.next = end;
        let rows = &scan.rows[start..end];
        if output.col_count() != 6 {
            return Err(Error::internal("expected six columns"));
        }
        T::write(rows, output.get_vector_array_mut([0, 1, 2, 3, 4, 5])?)
    }
}

fn strings<'a, 'r>(
    column: Vector<'a, Unknown>,
    len: usize,
    value: impl Fn(usize) -> Option<&'r str>,
) -> Result<()> {
    let mut column = column.cast::<String>()?;
    column.set_size(len)?;
    let mut writer = column.writer()?;
    for index in 0..len {
        writer.set(index, value(index))?;
    }
    Ok(())
}

fn ints<'a>(
    column: Vector<'a, Unknown>,
    len: usize,
    mut value: impl FnMut(usize) -> Option<i32>,
) -> Result<()> {
    let mut column = column.cast::<i32>()?;
    column.set_size(len)?;
    let mut writer = column.writer()?;
    for index in 0..len {
        writer.set(index, value(index))?;
    }
    Ok(())
}

fn bools<'a>(
    column: Vector<'a, Unknown>,
    len: usize,
    mut value: impl FnMut(usize) -> bool,
) -> Result<()> {
    let mut column = column.cast::<bool>()?;
    column.set_size(len)?;
    let mut writer = column.writer()?;
    for index in 0..len {
        writer.set(index, Some(value(index)))?;
    }
    Ok(())
}

fn timestamps<'a>(
    column: Vector<'a, Unknown>,
    len: usize,
    mut value: impl FnMut(usize) -> i64,
) -> Result<()> {
    let mut column = column.cast::<TimestampValue>()?;
    column.set_size(len)?;
    let mut writer = column.writer()?;
    for index in 0..len {
        writer.set(index, Some(TimestampValue(value(index) * 1_000_000)))?;
    }
    Ok(())
}

fn varchar(context: &Context) -> Result<duckdb_neo::logical_type::LogicalType> {
    String::logical_type(context)
}

fn integer(context: &Context) -> Result<duckdb_neo::logical_type::LogicalType> {
    i32::logical_type(context)
}

struct Hunks;

impl GitTable for Hunks {
    type Row = HunkRow;
    const NAME: &'static str = "git_hunks";
    const DESCRIPTION: &'static str = "One row per `@@ -old_start,old_lines +new_start,new_lines @@` header of \
        `git log -p -U0 -M` over [since..]HEAD, for every file, computed with gitoxide; numbers are 1-based \
        as in the header.";

    fn columns(
        context: &Context,
    ) -> Result<Vec<(&'static str, duckdb_neo::logical_type::LogicalType)>> {
        Ok(vec![
            ("sha", varchar(context)?),
            ("path", varchar(context)?),
            ("old_start", integer(context)?),
            ("old_lines", integer(context)?),
            ("new_start", integer(context)?),
            ("new_lines", integer(context)?),
        ])
    }

    fn rows(repo: &history::Repository, range: &Range) -> history::Result<Vec<HunkRow>> {
        history::hunks(repo, range)
    }

    fn write(rows: &[HunkRow], columns: [Vector<'_, Unknown>; 6]) -> Result<()> {
        let [sha, path, old_start, old_lines, new_start, new_lines] = columns;
        let n = rows.len();
        strings(sha, n, |i| Some(rows[i].sha.as_str()))?;
        strings(path, n, |i| Some(rows[i].path.as_str()))?;
        ints(old_start, n, |i| Some(rows[i].old_start))?;
        ints(old_lines, n, |i| Some(rows[i].old_lines))?;
        ints(new_start, n, |i| Some(rows[i].new_start))?;
        ints(new_lines, n, |i| Some(rows[i].new_lines))
    }
}

struct Commits;

impl GitTable for Commits {
    type Row = CommitRow;
    const NAME: &'static str = "git_commits";
    const DESCRIPTION: &'static str =
        "The commits `git log` lists for [since..]HEAD, in `git log` order; author_time is UTC.";

    fn columns(
        context: &Context,
    ) -> Result<Vec<(&'static str, duckdb_neo::logical_type::LogicalType)>> {
        Ok(vec![
            ("sha", varchar(context)?),
            ("author_time", TimestampValue::logical_type(context)?),
            ("author_name", varchar(context)?),
            ("subject", varchar(context)?),
            ("parent_count", integer(context)?),
            ("is_merge", bool::logical_type(context)?),
        ])
    }

    fn rows(repo: &history::Repository, range: &Range) -> history::Result<Vec<CommitRow>> {
        history::commits(repo, range)
    }

    fn write(rows: &[CommitRow], columns: [Vector<'_, Unknown>; 6]) -> Result<()> {
        let [
            sha,
            author_time,
            author_name,
            subject,
            parent_count,
            is_merge,
        ] = columns;
        let n = rows.len();
        strings(sha, n, |i| Some(rows[i].sha.as_str()))?;
        timestamps(author_time, n, |i| rows[i].author_time)?;
        strings(author_name, n, |i| Some(rows[i].author_name.as_str()))?;
        strings(subject, n, |i| Some(rows[i].subject.as_str()))?;
        ints(parent_count, n, |i| Some(rows[i].parent_count))?;
        bools(is_merge, n, |i| rows[i].is_merge)
    }
}

struct FileChanges;

impl GitTable for FileChanges {
    type Row = FileChangeRow;
    const NAME: &'static str = "git_file_changes";
    const DESCRIPTION: &'static str = "One row per file changed by each commit of `git log --raw --numstat -M` \
        over [since..]HEAD; old_path is set for renames, added/deleted are NULL for binary files \
        and submodules.";

    fn columns(
        context: &Context,
    ) -> Result<Vec<(&'static str, duckdb_neo::logical_type::LogicalType)>> {
        Ok(vec![
            ("sha", varchar(context)?),
            ("path", varchar(context)?),
            ("old_path", varchar(context)?),
            ("status", varchar(context)?),
            ("added", integer(context)?),
            ("deleted", integer(context)?),
        ])
    }

    fn rows(repo: &history::Repository, range: &Range) -> history::Result<Vec<FileChangeRow>> {
        history::file_changes(repo, range)
    }

    fn write(rows: &[FileChangeRow], columns: [Vector<'_, Unknown>; 6]) -> Result<()> {
        let [sha, path, old_path, status, added, deleted] = columns;
        let n = rows.len();
        strings(sha, n, |i| Some(rows[i].sha.as_str()))?;
        strings(path, n, |i| Some(rows[i].path.as_str()))?;
        strings(old_path, n, |i| rows[i].old_path.as_deref())?;
        strings(status, n, |i| Some(rows[i].status.as_str()))?;
        ints(added, n, |i| rows[i].added)?;
        ints(deleted, n, |i| rows[i].deleted)
    }
}

fn register_one<T: GitTable>(extension: &Extension) -> Result<()> {
    let signature = SignatureBuilder::new()
        .param(FunctionParameter::new::<String>("repo"))
        .param(
            FunctionParameter::with_default::<Option<String>>("since", None)
                .kind(FunctionParameterKind::Named),
        )
        .param(
            FunctionParameter::with_default("include_merges", false)
                .kind(FunctionParameterKind::Named),
        )
        .documentation(
            FunctionDocumentationBuilder::new()
                .add_category("git")
                .description(T::DESCRIPTION)
                .add_example("all history", format!("SELECT * FROM {}('.')", T::NAME))
                .add_example(
                    "a range with merges",
                    format!(
                        "SELECT * FROM {}('.', since := 'v1.0', include_merges := true)",
                        T::NAME
                    ),
                ),
        );
    TableFunctionBuilder::new(T::NAME, signature, Function::<T>(std::marker::PhantomData))
        .register(extension)
}

pub(crate) fn register(extension: &Extension) -> Result<()> {
    register_one::<Hunks>(extension)?;
    register_one::<Commits>(extension)?;
    register_one::<FileChanges>(extension)?;
    register_tracked_files(extension)?;
    register_changed_blobs(extension)?;
    register_tree(extension)
}

/// Arguments of `git_tracked_files`, resolved at bind time.
pub(crate) struct TrackedParams {
    repo: String,
    include_untracked: bool,
}

/// `git_tracked_files`: the worktree's files now, not a history range, so it
/// has its own arguments instead of being a [`GitTable`].
struct TrackedFiles;

const TRACKED_FILES: &str = "git_tracked_files";

impl TableFunctionCallbacks for TrackedFiles {
    type BindData = TrackedParams;
    type LocalState = ();
    type GlobalState = Mutex<Scan<TrackedFileRow>>;

    fn bind(
        &self,
        context: &Context,
        arguments: Vec<BindArgument>,
        bind: BindFunctionHandle<'_>,
    ) -> Result<TrackedParams> {
        let value = |index: usize| {
            arguments
                .get(index)
                .and_then(|argument| argument.value.as_ref())
        };
        let repo = value(0)
            .map(|value| value.get::<String>())
            .transpose()?
            .flatten()
            .ok_or_else(|| {
                Error::invalid_input(format!(
                    "{TRACKED_FILES}: repo must be a constant, non-NULL path"
                ))
            })?;
        let include_untracked = value(1)
            .map(|value| value.get::<bool>())
            .transpose()?
            .flatten()
            .unwrap_or(false);
        history::open(&repo).map_err(git_error)?;
        for (name, logical_type) in [
            ("path", varchar(context)?),
            ("mode", varchar(context)?),
            ("blob_id", varchar(context)?),
            ("is_tracked", bool::logical_type(context)?),
            ("is_modified", bool::logical_type(context)?),
        ] {
            bind.add_result_column(name, &logical_type)?;
        }
        Ok(TrackedParams {
            repo,
            include_untracked,
        })
    }

    fn init_global_state(
        &self,
        params: &TrackedParams,
        _context: &Context,
        _columns: InitColumnData<'_>,
    ) -> Result<(Option<Self::GlobalState>, Option<usize>)> {
        let repo = history::open(&params.repo).map_err(git_error)?;
        let rows = history::tracked_files(&repo, params.include_untracked).map_err(git_error)?;
        Ok((Some(Mutex::new(Scan { rows, next: 0 })), Some(1)))
    }

    fn exec(
        &self,
        _params: &TrackedParams,
        global: Option<&Self::GlobalState>,
        _local: Option<&mut ()>,
        _context: &Context,
        mut output: DataChunkRef<'_>,
        _columns: ExecColumnInfo<'_>,
    ) -> Result<()> {
        let mut scan = global
            .ok_or_else(|| Error::internal("scan state missing"))?
            .lock()
            .map_err(|_| Error::internal("scan state poisoned"))?;
        let start = scan.next;
        let end = scan.rows.len().min(start + output.capacity()?);
        scan.next = end;
        let rows = &scan.rows[start..end];
        if output.col_count() != 5 {
            return Err(Error::internal("expected five columns"));
        }
        let [path, mode, blob_id, is_tracked, is_modified] =
            output.get_vector_array_mut([0, 1, 2, 3, 4])?;
        let n = rows.len();
        strings(path, n, |i| Some(rows[i].path.as_str()))?;
        strings(mode, n, |i| Some(rows[i].mode.as_str()))?;
        strings(blob_id, n, |i| Some(rows[i].blob_id.as_str()))?;
        bools(is_tracked, n, |i| rows[i].is_tracked)?;
        bools(is_modified, n, |i| rows[i].is_modified)
    }
}

fn register_tracked_files(extension: &Extension) -> Result<()> {
    let signature = SignatureBuilder::new()
        .param(FunctionParameter::new::<String>("repo"))
        .param(
            FunctionParameter::with_default("include_untracked", false)
                .kind(FunctionParameterKind::Named),
        )
        .documentation(
            FunctionDocumentationBuilder::new()
                .add_category("git")
                .description(
                    "One row per file of the worktree now, as `git ls-files` lists it, with the id of \
                     its current content (the index id when unchanged, else what `git hash-object` \
                     gives); include_untracked adds the files of `git ls-files -o --exclude-standard`.",
                )
                .add_example("tracked files", format!("SELECT * FROM {TRACKED_FILES}('.')"))
                .add_example(
                    "with untracked files",
                    format!("SELECT * FROM {TRACKED_FILES}('.', include_untracked := true)"),
                ),
        );
    TableFunctionBuilder::new(TRACKED_FILES, signature, TrackedFiles).register(extension)
}

/// Arguments of `git_changed_blobs`, resolved at bind time.
pub(crate) struct BlobParams {
    repo: String,
    dir: String,
    range: Range,
    extensions: Option<Vec<String>>,
}

/// `git_changed_blobs`: writes files as well as returning rows, so it has its
/// own arguments instead of being a [`GitTable`].
struct ChangedBlobs;

const CHANGED_BLOBS: &str = "git_changed_blobs";

impl TableFunctionCallbacks for ChangedBlobs {
    type BindData = BlobParams;
    type LocalState = ();
    type GlobalState = Mutex<Scan<ChangedBlobRow>>;

    fn bind(
        &self,
        context: &Context,
        arguments: Vec<BindArgument>,
        bind: BindFunctionHandle<'_>,
    ) -> Result<BlobParams> {
        let value = |index: usize| {
            arguments
                .get(index)
                .and_then(|argument| argument.value.as_ref())
        };
        let required = |index: usize, name: &str| {
            value(index)
                .map(|value| value.get::<String>())
                .transpose()?
                .flatten()
                .ok_or_else(|| {
                    Error::invalid_input(format!(
                        "{CHANGED_BLOBS}: {name} must be a constant, non-NULL path"
                    ))
                })
        };
        let repo = required(0, "repo")?;
        let dir = required(1, "dir")?;
        let since = value(2)
            .map(|value| value.get::<String>())
            .transpose()?
            .flatten();
        let include_merges = value(3)
            .map(|value| value.get::<bool>())
            .transpose()?
            .flatten()
            .unwrap_or(false);
        // NULL elements of the list match no path.
        let extensions = value(4)
            .map(|value| value.get::<Vec<Option<String>>>())
            .transpose()?
            .flatten()
            .map(|list| list.into_iter().flatten().collect());
        history::open(&repo).map_err(git_error)?;
        if !std::path::Path::new(&dir).is_absolute() {
            return Err(Error::invalid_input(format!(
                "{CHANGED_BLOBS}: dir must be an absolute path, got '{dir}'"
            )));
        }
        for name in ["sha", "path", "old_path", "side", "blob_id", "mode", "file"] {
            bind.add_result_column(name, &varchar(context)?)?;
        }
        Ok(BlobParams {
            repo,
            dir,
            range: Range {
                since,
                include_merges,
            },
            extensions,
        })
    }

    fn init_global_state(
        &self,
        params: &BlobParams,
        _context: &Context,
        _columns: InitColumnData<'_>,
    ) -> Result<(Option<Self::GlobalState>, Option<usize>)> {
        let repo = history::open(&params.repo).map_err(git_error)?;
        let (rows, _stats) = history::changed_blobs(
            &repo,
            &params.range,
            std::path::Path::new(&params.dir),
            params.extensions.as_deref(),
        )
        .map_err(git_error)?;
        Ok((Some(Mutex::new(Scan { rows, next: 0 })), Some(1)))
    }

    fn exec(
        &self,
        _params: &BlobParams,
        global: Option<&Self::GlobalState>,
        _local: Option<&mut ()>,
        _context: &Context,
        mut output: DataChunkRef<'_>,
        _columns: ExecColumnInfo<'_>,
    ) -> Result<()> {
        let mut scan = global
            .ok_or_else(|| Error::internal("scan state missing"))?
            .lock()
            .map_err(|_| Error::internal("scan state poisoned"))?;
        let start = scan.next;
        let end = scan.rows.len().min(start + output.capacity()?);
        scan.next = end;
        let rows = &scan.rows[start..end];
        if output.col_count() != 7 {
            return Err(Error::internal("expected seven columns"));
        }
        let [sha, path, old_path, side, blob_id, mode, file] =
            output.get_vector_array_mut([0, 1, 2, 3, 4, 5, 6])?;
        let n = rows.len();
        strings(sha, n, |i| Some(rows[i].sha.as_str()))?;
        strings(path, n, |i| Some(rows[i].path.as_str()))?;
        strings(old_path, n, |i| rows[i].old_path.as_deref())?;
        strings(side, n, |i| Some(rows[i].side))?;
        strings(blob_id, n, |i| Some(rows[i].blob_id.as_str()))?;
        strings(mode, n, |i| Some(rows[i].mode))?;
        strings(file, n, |i| Some(rows[i].file.as_str()))
    }
}

fn register_changed_blobs(extension: &Extension) -> Result<()> {
    let signature = SignatureBuilder::new()
        .param(FunctionParameter::new::<String>("repo"))
        .param(FunctionParameter::new::<String>("dir"))
        .param(
            FunctionParameter::with_default::<Option<String>>("since", None)
                .kind(FunctionParameterKind::Named),
        )
        .param(
            FunctionParameter::with_default("include_merges", false)
                .kind(FunctionParameterKind::Named),
        )
        .param(
            FunctionParameter::with_default::<Option<Vec<String>>>("extensions", None)
                .kind(FunctionParameterKind::Named),
        )
        .documentation(
            FunctionDocumentationBuilder::new()
                .add_category("git")
                .description(
                    "Writes the pre- and post-image blobs of the regular, non-binary files that \
                     git_file_changes lists over [since..]HEAD to dir (absolute; created if missing) \
                     as <blob_id>.<ext>, each once, and returns one row per written side ('old' or \
                     'new') with the file's absolute path; extensions limits the paths considered.",
                )
                .add_example(
                    "Rust blobs of all history",
                    format!(
                        "SELECT * FROM {CHANGED_BLOBS}('.', '/tmp/blobs', extensions := ['rs'])"
                    ),
                ),
        );
    TableFunctionBuilder::new(CHANGED_BLOBS, signature, ChangedBlobs).register(extension)
}

/// Arguments of `git_tree`, resolved at bind time.
pub(crate) struct TreeParams {
    repo: String,
    /// The tree `rev` named at bind time, so the scan lists that tree even if
    /// a reference moves in between.
    tree: history::ObjectId,
}

/// `git_tree`: one revision's tree, not a history range, so it has its own
/// arguments instead of being a [`GitTable`].
struct Tree;

const TREE: &str = "git_tree";

impl TableFunctionCallbacks for Tree {
    type BindData = TreeParams;
    type LocalState = ();
    type GlobalState = Mutex<Scan<TreeRow>>;

    fn bind(
        &self,
        context: &Context,
        arguments: Vec<BindArgument>,
        bind: BindFunctionHandle<'_>,
    ) -> Result<TreeParams> {
        let value = |index: usize| {
            arguments
                .get(index)
                .and_then(|argument| argument.value.as_ref())
        };
        let required = |index: usize, what: &str| {
            value(index)
                .map(|value| value.get::<String>())
                .transpose()?
                .flatten()
                .ok_or_else(|| {
                    Error::invalid_input(format!("{TREE}: {what} must be a constant, non-NULL"))
                })
        };
        let repo = required(0, "repo path")?;
        let rev = required(1, "rev")?;
        let opened = history::open(&repo).map_err(git_error)?;
        let tree = history::resolve_tree(&opened, &rev)
            .map_err(|error| Error::invalid_input(format!("{TREE}: {error}")))?;
        for name in ["path", "mode", "blob_id"] {
            bind.add_result_column(name, &varchar(context)?)?;
        }
        Ok(TreeParams { repo, tree })
    }

    fn init_global_state(
        &self,
        params: &TreeParams,
        _context: &Context,
        _columns: InitColumnData<'_>,
    ) -> Result<(Option<Self::GlobalState>, Option<usize>)> {
        let repo = history::open(&params.repo).map_err(git_error)?;
        let rows = history::tree_entries(&repo, params.tree).map_err(git_error)?;
        Ok((Some(Mutex::new(Scan { rows, next: 0 })), Some(1)))
    }

    fn exec(
        &self,
        _params: &TreeParams,
        global: Option<&Self::GlobalState>,
        _local: Option<&mut ()>,
        _context: &Context,
        mut output: DataChunkRef<'_>,
        _columns: ExecColumnInfo<'_>,
    ) -> Result<()> {
        let mut scan = global
            .ok_or_else(|| Error::internal("scan state missing"))?
            .lock()
            .map_err(|_| Error::internal("scan state poisoned"))?;
        let start = scan.next;
        let end = scan.rows.len().min(start + output.capacity()?);
        scan.next = end;
        let rows = &scan.rows[start..end];
        if output.col_count() != 3 {
            return Err(Error::internal("expected three columns"));
        }
        let [path, mode, blob_id] = output.get_vector_array_mut([0, 1, 2])?;
        let n = rows.len();
        strings(path, n, |i| Some(rows[i].path.as_str()))?;
        strings(mode, n, |i| Some(rows[i].mode))?;
        strings(blob_id, n, |i| Some(rows[i].blob_id.as_str()))
    }
}

fn register_tree(extension: &Extension) -> Result<()> {
    let signature = SignatureBuilder::new()
        .param(FunctionParameter::new::<String>("repo"))
        .param(
            FunctionParameter::with_default("rev", "HEAD".to_string())
                .kind(FunctionParameterKind::Named),
        )
        .documentation(
            FunctionDocumentationBuilder::new()
                .add_category("git")
                .description(
                    "One row per file, symlink and gitlink of the tree rev names (a commit-ish or a \
                     tree id), recursively, as `git ls-tree -r` lists them, sorted by path: the \
                     mode and the blob id (the commit id for a gitlink).",
                )
                .add_example("the tree of HEAD", format!("SELECT * FROM {TREE}('.')"))
                .add_example(
                    "the tree of a tag",
                    format!("SELECT * FROM {TREE}('.', rev := 'v1.0')"),
                ),
        );
    TableFunctionBuilder::new(TREE, signature, Tree).register(extension)
}
