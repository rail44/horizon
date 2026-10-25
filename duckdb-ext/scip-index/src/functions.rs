//! The DuckDB table functions over [`scip_index_core`].
//!
//! `read_scip` and `read_scip_symbols` stream: the global state holds the
//! core's iterator, which keeps one document of the index in memory, and each
//! call emits the next chunk of rows, from a single thread so rows keep file
//! order. `read_scip_metadata` reads the whole file in its global init and
//! emits its one row.

use std::path::Path;
use std::sync::Mutex;

use duckdb_neo::{
    DuckDBType, Error, Result,
    bind_arguments::BindArgument,
    connection::Context,
    data_chunk::DataChunkRef,
    extension::Extension,
    logical_type::LogicalType,
    signature::{FunctionDocumentationBuilder, FunctionParameter, SignatureBuilder},
    table_function::{
        BindFunctionHandle, ExecColumnInfo, InitColumnData, TableFunctionBuilder,
        TableFunctionCallbacks,
    },
    types::List,
    vector::{Unknown, Vector},
};

use scip_index_core::{
    self as scip, DEFINITION, FORWARD_DEFINITION, GENERATED, IMPORT, MetadataRow, OccurrenceRow,
    READ_ACCESS, SymbolRow, TEST, WRITE_ACCESS,
};

fn scip_error(error: scip::Error) -> Error {
    Error::invalid_input(error.to_string())
}

/// One table function: its columns, its rows, and how to write a chunk of them.
trait ScipTable: Send + Sync + 'static {
    type Row: Send + 'static;
    type Rows: Iterator<Item = scip::Result<Self::Row>> + Send + 'static;
    const NAME: &'static str;
    const DESCRIPTION: &'static str;

    fn columns(context: &Context) -> Result<Vec<(&'static str, LogicalType)>>;
    fn rows(path: &Path) -> scip::Result<Self::Rows>;
    fn write(rows: &[Self::Row], output: &mut DataChunkRef<'_>) -> Result<()>;
}

struct Function<T>(std::marker::PhantomData<T>);

/// The index path, resolved at bind time.
pub(crate) struct Params {
    path: String,
}

impl<T: ScipTable> TableFunctionCallbacks for Function<T> {
    type BindData = Params;
    type LocalState = ();
    type GlobalState = Mutex<T::Rows>;

    fn bind(
        &self,
        context: &Context,
        arguments: Vec<BindArgument>,
        bind: BindFunctionHandle<'_>,
    ) -> Result<Params> {
        let path = arguments
            .first()
            .and_then(|argument| argument.value.as_ref())
            .map(|value| value.get::<String>())
            .transpose()?
            .flatten()
            .ok_or_else(|| {
                Error::invalid_input(format!(
                    "{}: path must be a constant, non-NULL path",
                    T::NAME
                ))
            })?;
        // Fail at bind time on a missing or unreadable file.
        std::fs::File::open(&path)
            .map_err(|error| Error::invalid_input(format!("{}: {path}: {error}", T::NAME)))?;
        for (name, logical_type) in T::columns(context)? {
            bind.add_result_column(name, &logical_type)?;
        }
        Ok(Params { path })
    }

    fn init_global_state(
        &self,
        params: &Params,
        _context: &Context,
        _columns: InitColumnData<'_>,
    ) -> Result<(Option<Self::GlobalState>, Option<usize>)> {
        let rows = T::rows(Path::new(&params.path)).map_err(scip_error)?;
        Ok((Some(Mutex::new(rows)), Some(1)))
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
        let mut rows = global
            .ok_or_else(|| Error::internal("scan state missing"))?
            .lock()
            .map_err(|_| Error::internal("scan state poisoned"))?;
        let capacity = output.capacity()?;
        let chunk = rows
            .by_ref()
            .take(capacity)
            .collect::<scip::Result<Vec<_>>>()
            .map_err(scip_error)?;
        T::write(&chunk, &mut output)
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

fn string_lists<'a, 'r>(
    column: Vector<'a, Unknown>,
    len: usize,
    value: impl Fn(usize) -> &'r [String],
) -> Result<()> {
    let mut column = column.cast::<List<String>>()?;
    column.set_size(len)?;
    let mut writer = column.writer()?;
    for index in 0..len {
        let items = value(index)
            .iter()
            .map(|item| Some(item.as_str()))
            .collect();
        writer.set(index, Some(items))?;
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

fn bigints<'a>(
    column: Vector<'a, Unknown>,
    len: usize,
    mut value: impl FnMut(usize) -> i64,
) -> Result<()> {
    let mut column = column.cast::<i64>()?;
    column.set_size(len)?;
    let mut writer = column.writer()?;
    for index in 0..len {
        writer.set(index, Some(value(index)))?;
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

fn check_columns(output: &DataChunkRef<'_>, expected: usize) -> Result<()> {
    if output.col_count() != expected {
        return Err(Error::internal(format!(
            "expected {expected} columns, got {}",
            output.col_count()
        )));
    }
    Ok(())
}

fn varchar(context: &Context) -> Result<LogicalType> {
    String::logical_type(context)
}

fn integer(context: &Context) -> Result<LogicalType> {
    i32::logical_type(context)
}

fn boolean(context: &Context) -> Result<LogicalType> {
    bool::logical_type(context)
}

struct Occurrences;

/// The role columns of `read_scip`, after `roles`, with their bits.
const ROLE_COLUMNS: [(&str, i32); 7] = [
    ("is_definition", DEFINITION),
    ("is_import", IMPORT),
    ("is_write", WRITE_ACCESS),
    ("is_read", READ_ACCESS),
    ("is_generated", GENERATED),
    ("is_test", TEST),
    ("is_forward_definition", FORWARD_DEFINITION),
];

impl ScipTable for Occurrences {
    type Row = OccurrenceRow;
    type Rows = scip::Occurrences<std::io::BufReader<std::fs::File>>;
    const NAME: &'static str = "read_scip";
    const DESCRIPTION: &'static str = "One row per occurrence of a SCIP index, in file order: the document's \
        relative_path, the raw symbol (`local N` for document-local symbols), the 0-based half-open range, \
        the raw symbol_roles bitmask and one boolean per role, and the enclosing range (NULL when absent).";

    fn columns(context: &Context) -> Result<Vec<(&'static str, LogicalType)>> {
        let mut columns = vec![
            ("document", varchar(context)?),
            ("symbol", varchar(context)?),
        ];
        for name in ["start_line", "start_char", "end_line", "end_char", "roles"] {
            columns.push((name, integer(context)?));
        }
        for (name, _) in ROLE_COLUMNS {
            columns.push((name, boolean(context)?));
        }
        for name in [
            "enclosing_start_line",
            "enclosing_start_char",
            "enclosing_end_line",
            "enclosing_end_char",
        ] {
            columns.push((name, integer(context)?));
        }
        Ok(columns)
    }

    fn rows(path: &Path) -> scip::Result<Self::Rows> {
        scip::occurrences(path)
    }

    fn write(rows: &[OccurrenceRow], output: &mut DataChunkRef<'_>) -> Result<()> {
        check_columns(output, 18)?;
        let [
            document,
            symbol,
            start_line,
            start_char,
            end_line,
            end_char,
            roles,
            is_definition,
            is_import,
            is_write,
            is_read,
            is_generated,
            is_test,
            is_forward_definition,
            enclosing_start_line,
            enclosing_start_char,
            enclosing_end_line,
            enclosing_end_char,
        ] = output.get_vector_array_mut(std::array::from_fn(|index| index))?;
        let n = rows.len();
        strings(document, n, |i| Some(&*rows[i].document))?;
        strings(symbol, n, |i| Some(rows[i].symbol.as_str()))?;
        ints(start_line, n, |i| Some(rows[i].range.start_line))?;
        ints(start_char, n, |i| Some(rows[i].range.start_char))?;
        ints(end_line, n, |i| Some(rows[i].range.end_line))?;
        ints(end_char, n, |i| Some(rows[i].range.end_char))?;
        ints(roles, n, |i| Some(rows[i].roles))?;
        for (column, (_, role)) in [
            is_definition,
            is_import,
            is_write,
            is_read,
            is_generated,
            is_test,
            is_forward_definition,
        ]
        .into_iter()
        .zip(ROLE_COLUMNS)
        {
            bools(column, n, |i| rows[i].has_role(role))?;
        }
        ints(enclosing_start_line, n, |i| {
            rows[i].enclosing.map(|r| r.start_line)
        })?;
        ints(enclosing_start_char, n, |i| {
            rows[i].enclosing.map(|r| r.start_char)
        })?;
        ints(enclosing_end_line, n, |i| {
            rows[i].enclosing.map(|r| r.end_line)
        })?;
        ints(enclosing_end_char, n, |i| {
            rows[i].enclosing.map(|r| r.end_char)
        })
    }
}

struct Symbols;

impl ScipTable for Symbols {
    type Row = SymbolRow;
    type Rows = scip::Symbols<std::io::BufReader<std::fs::File>>;
    const NAME: &'static str = "read_scip_symbols";
    const DESCRIPTION: &'static str = "One row per SymbolInformation of a SCIP index, in file order: those of \
        each document (document is its relative_path) and the external symbols (document is NULL); kind is \
        the Kind enum's name.";

    fn columns(context: &Context) -> Result<Vec<(&'static str, LogicalType)>> {
        Ok(vec![
            ("document", varchar(context)?),
            ("symbol", varchar(context)?),
            ("kind", varchar(context)?),
            ("display_name", varchar(context)?),
            ("enclosing_symbol", varchar(context)?),
            ("documentation", Vec::<String>::logical_type(context)?),
            ("relationship_count", integer(context)?),
        ])
    }

    fn rows(path: &Path) -> scip::Result<Self::Rows> {
        scip::symbols(path)
    }

    fn write(rows: &[SymbolRow], output: &mut DataChunkRef<'_>) -> Result<()> {
        check_columns(output, 7)?;
        let [
            document,
            symbol,
            kind,
            display_name,
            enclosing_symbol,
            documentation,
            relationship_count,
        ] = output.get_vector_array_mut([0, 1, 2, 3, 4, 5, 6])?;
        let n = rows.len();
        let kinds: Vec<_> = rows.iter().map(|row| scip::kind_name(row.kind)).collect();
        strings(document, n, |i| rows[i].document.as_deref())?;
        strings(symbol, n, |i| Some(rows[i].symbol.as_str()))?;
        strings(kind, n, |i| Some(&*kinds[i]))?;
        strings(display_name, n, |i| Some(rows[i].display_name.as_str()))?;
        strings(enclosing_symbol, n, |i| {
            Some(rows[i].enclosing_symbol.as_str())
        })?;
        string_lists(documentation, n, |i| &rows[i].documentation)?;
        ints(relationship_count, n, |i| Some(rows[i].relationship_count))
    }
}

struct Metadata;

impl ScipTable for Metadata {
    type Row = MetadataRow;
    type Rows = std::iter::Once<scip::Result<MetadataRow>>;
    const NAME: &'static str = "read_scip_metadata";
    const DESCRIPTION: &'static str = "One row: the Metadata of a SCIP index (version is the raw \
        ProtocolVersion value, text_document_encoding the TextEncoding enum's name) and the counts of its \
        documents, occurrences and external symbols; reads the whole file.";

    fn columns(context: &Context) -> Result<Vec<(&'static str, LogicalType)>> {
        let bigint = || i64::logical_type(context);
        Ok(vec![
            ("version", integer(context)?),
            ("tool_name", varchar(context)?),
            ("tool_version", varchar(context)?),
            ("tool_arguments", Vec::<String>::logical_type(context)?),
            ("project_root", varchar(context)?),
            ("text_document_encoding", varchar(context)?),
            ("document_count", bigint()?),
            ("occurrence_count", bigint()?),
            ("external_symbol_count", bigint()?),
        ])
    }

    fn rows(path: &Path) -> scip::Result<Self::Rows> {
        Ok(std::iter::once(Ok(scip::metadata(path)?)))
    }

    fn write(rows: &[MetadataRow], output: &mut DataChunkRef<'_>) -> Result<()> {
        check_columns(output, 9)?;
        let [
            version,
            tool_name,
            tool_version,
            tool_arguments,
            project_root,
            text_document_encoding,
            document_count,
            occurrence_count,
            external_symbol_count,
        ] = output.get_vector_array_mut([0, 1, 2, 3, 4, 5, 6, 7, 8])?;
        let n = rows.len();
        let encodings: Vec<_> = rows
            .iter()
            .map(|row| scip::text_encoding_name(row.text_document_encoding))
            .collect();
        let count = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
        ints(version, n, |i| Some(rows[i].version))?;
        strings(tool_name, n, |i| Some(rows[i].tool_name.as_str()))?;
        strings(tool_version, n, |i| Some(rows[i].tool_version.as_str()))?;
        string_lists(tool_arguments, n, |i| &rows[i].tool_arguments)?;
        strings(project_root, n, |i| Some(rows[i].project_root.as_str()))?;
        strings(text_document_encoding, n, |i| Some(&*encodings[i]))?;
        bigints(document_count, n, |i| count(rows[i].document_count))?;
        bigints(occurrence_count, n, |i| count(rows[i].occurrence_count))?;
        bigints(external_symbol_count, n, |i| {
            count(rows[i].external_symbol_count)
        })
    }
}

fn register_one<T: ScipTable>(extension: &Extension) -> Result<()> {
    let signature = SignatureBuilder::new()
        .param(FunctionParameter::new::<String>("path"))
        .documentation(
            FunctionDocumentationBuilder::new()
                .add_category("scip")
                .description(T::DESCRIPTION)
                .add_example(
                    "an index written by `rust-analyzer scip`",
                    format!("SELECT * FROM {}('target/code-db/index.scip')", T::NAME),
                ),
        );
    TableFunctionBuilder::new(T::NAME, signature, Function::<T>(std::marker::PhantomData))
        .register(extension)
}

pub(crate) fn register(extension: &Extension) -> Result<()> {
    register_one::<Occurrences>(extension)?;
    register_one::<Symbols>(extension)?;
    register_one::<Metadata>(extension)
}
