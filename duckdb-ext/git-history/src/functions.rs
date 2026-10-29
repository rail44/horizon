//! The DuckDB table function over [`git_history_core`]: `git_tracked_files`.
//!
//! It materializes its rows once in the global init (so `DESCRIBE` and
//! binding stay cheap) and emits them in order, one chunk per call, from a
//! single thread.

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
    vector::{Unknown, Vector},
};

use git_history_core::{self as history, TrackedFileRow};

/// Rows of one scan and the index of the next row to emit.
pub(crate) struct Scan<R> {
    rows: Vec<R>,
    next: usize,
}

fn git_error(error: history::Error) -> Error {
    Error::invalid_input(error.to_string())
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

fn varchar(context: &Context) -> Result<duckdb_neo::logical_type::LogicalType> {
    String::logical_type(context)
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

pub(crate) fn register(extension: &Extension) -> Result<()> {
    register_tracked_files(extension)
}
