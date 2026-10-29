//! One in-memory DuckDB database and one connection, through the C API the
//! `duckdb` crate links (`duckdb::ffi`). The crate's `Connection` does not
//! expose its raw handle, and every statement here must run on one connection:
//! DuckDB variables and temporary objects belong to the connection.

use std::ffi::{CStr, CString, c_char};
use std::ptr;

use duckdb::ffi;

/// A statement's result set: column names, whether each column is numeric
/// (the DuckDB CLI right-aligns those in markdown mode), and every value as
/// DuckDB renders it as text (`None` for NULL).
pub struct Table {
    pub names: Vec<String>,
    pub numeric: Vec<bool>,
    pub rows: Vec<Vec<Option<String>>>,
    /// An EXPLAIN result, which the CLI prints as the plan text alone.
    pub explain: bool,
}

pub struct Db {
    db: ffi::duckdb_database,
    con: ffi::duckdb_connection,
}

fn take_c_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

fn c_string(text: &str) -> Result<CString, String> {
    CString::new(text).map_err(|_| "SQL text contains a NUL byte".to_owned())
}

impl Db {
    /// An in-memory database that accepts unsigned extensions.
    pub fn open() -> Result<Db, String> {
        unsafe {
            let mut config: ffi::duckdb_config = ptr::null_mut();
            if ffi::duckdb_create_config(&mut config) != ffi::DuckDBSuccess {
                return Err("cannot create a DuckDB configuration".to_owned());
            }
            let name = c_string("allow_unsigned_extensions")?;
            let value = c_string("true")?;
            let set = ffi::duckdb_set_config(config, name.as_ptr(), value.as_ptr());
            let mut db: ffi::duckdb_database = ptr::null_mut();
            let mut error: *mut c_char = ptr::null_mut();
            let opened = if set == ffi::DuckDBSuccess {
                ffi::duckdb_open_ext(ptr::null(), &mut db, config, &mut error)
            } else {
                ffi::DuckDBError
            };
            ffi::duckdb_destroy_config(&mut config);
            if opened != ffi::DuckDBSuccess {
                let message = take_c_string(error);
                if !error.is_null() {
                    ffi::duckdb_free(error.cast());
                }
                return Err(format!("cannot open DuckDB: {message}"));
            }
            let mut con: ffi::duckdb_connection = ptr::null_mut();
            if ffi::duckdb_connect(db, &mut con) != ffi::DuckDBSuccess {
                ffi::duckdb_close(&mut db);
                return Err("cannot connect to DuckDB".to_owned());
            }
            Ok(Db { db, con })
        }
    }

    /// Run every statement of `sql` in order; `sink` receives the result set
    /// of each statement that returns rows. Stops at the first error.
    pub fn run(&self, sql: &str, sink: &mut dyn FnMut(Table)) -> Result<(), String> {
        let text = c_string(sql)?;
        unsafe {
            let mut extracted: ffi::duckdb_extracted_statements = ptr::null_mut();
            let count = ffi::duckdb_extract_statements(self.con, text.as_ptr(), &mut extracted);
            let outcome = if count == 0 {
                let message = take_c_string(ffi::duckdb_extract_statements_error(extracted));
                if message.is_empty() {
                    Ok(())
                } else {
                    Err(message)
                }
            } else {
                (0..count).try_for_each(|index| self.run_extracted(extracted, index, sink))
            };
            ffi::duckdb_destroy_extracted(&mut extracted);
            outcome
        }
    }

    unsafe fn run_extracted(
        &self,
        extracted: ffi::duckdb_extracted_statements,
        index: ffi::idx_t,
        sink: &mut dyn FnMut(Table),
    ) -> Result<(), String> {
        unsafe {
            let mut prepared: ffi::duckdb_prepared_statement = ptr::null_mut();
            if ffi::duckdb_prepare_extracted_statement(self.con, extracted, index, &mut prepared)
                != ffi::DuckDBSuccess
            {
                let message = take_c_string(ffi::duckdb_prepare_error(prepared));
                ffi::duckdb_destroy_prepare(&mut prepared);
                return Err(message);
            }
            if let Some(t) = execute(&mut prepared)? {
                sink(t);
            }
            Ok(())
        }
    }

    /// Run one statement and return its result set, every value rendered as
    /// DuckDB casts it to VARCHAR (which is what the DuckDB CLI prints). The
    /// C API's `duckdb_value_varchar` returns an empty string for nested
    /// types, so a SELECT runs wrapped in `SELECT CAST(COLUMNS(*) AS VARCHAR)`;
    /// its names and types come from the statement as written. Any other
    /// statement with a nested column is refused rather than printed empty.
    pub fn run_statement(&self, statement: &str) -> Result<Option<Table>, String> {
        let text = c_string(statement)?;
        let (kind, names, numeric, nested) = unsafe {
            let mut extracted: ffi::duckdb_extracted_statements = ptr::null_mut();
            let count = ffi::duckdb_extract_statements(self.con, text.as_ptr(), &mut extracted);
            if count != 1 {
                let message = take_c_string(ffi::duckdb_extract_statements_error(extracted));
                ffi::duckdb_destroy_extracted(&mut extracted);
                return match (count, message.is_empty()) {
                    (0, true) => Ok(None),
                    (0, false) => Err(message),
                    _ => Err(format!(
                        "code-db split a statement DuckDB reads as {count}: {statement}"
                    )),
                };
            }
            let mut prepared: ffi::duckdb_prepared_statement = ptr::null_mut();
            let state =
                ffi::duckdb_prepare_extracted_statement(self.con, extracted, 0, &mut prepared);
            ffi::duckdb_destroy_extracted(&mut extracted);
            if state != ffi::DuckDBSuccess {
                let message = take_c_string(ffi::duckdb_prepare_error(prepared));
                ffi::duckdb_destroy_prepare(&mut prepared);
                return Err(message);
            }
            let kind = ffi::duckdb_prepared_statement_type(prepared);
            let columns = ffi::duckdb_prepared_statement_column_count(prepared);
            let names: Vec<String> = (0..columns)
                .map(|c| {
                    let name = ffi::duckdb_prepared_statement_column_name(prepared, c);
                    let owned = take_c_string(name);
                    if !name.is_null() {
                        ffi::duckdb_free(name as *mut _);
                    }
                    owned
                })
                .collect();
            let numeric: Vec<bool> = (0..columns)
                .map(|c| is_numeric(ffi::duckdb_prepared_statement_column_type(prepared, c)))
                .collect();
            let nested = (0..columns).any(|c| {
                matches!(
                    ffi::duckdb_prepared_statement_column_type(prepared, c),
                    ffi::DUCKDB_TYPE_DUCKDB_TYPE_LIST
                        | ffi::DUCKDB_TYPE_DUCKDB_TYPE_STRUCT
                        | ffi::DUCKDB_TYPE_DUCKDB_TYPE_MAP
                        | ffi::DUCKDB_TYPE_DUCKDB_TYPE_ARRAY
                        | ffi::DUCKDB_TYPE_DUCKDB_TYPE_UNION
                        | ffi::DUCKDB_TYPE_DUCKDB_TYPE_VARIANT
                )
            });
            ffi::duckdb_destroy_prepare(&mut prepared);
            (kind, names, numeric, nested)
        };
        let mut table = None;
        // A statement DuckDB rewrites into a SELECT (PRAGMA version) cannot be
        // wrapped; it runs as written, like any other statement.
        let wrapped = (kind == ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_SELECT)
            .then(|| format!("SELECT CAST(COLUMNS(*) AS VARCHAR) FROM (\n{statement}\n)"))
            .and_then(|w| unsafe { self.prepare(&w) }.ok());
        if let Some(mut prepared) = wrapped {
            table = unsafe { execute(&mut prepared) }?;
            if let Some(t) = table.as_mut() {
                t.names = names;
                t.numeric = numeric;
            }
        } else if nested {
            return Err(format!(
                "code-db prints nested values of SELECT statements only; select the columns \
                 as VARCHAR, or use another FORMAT (the DuckDB CLI renders it): {statement}"
            ));
        } else {
            self.run(statement, &mut |t| table = Some(t))?;
            if let Some(t) = table.as_mut() {
                t.explain = kind == ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_EXPLAIN;
            }
        }
        Ok(table)
    }

    /// A prepared statement for SQL text holding one statement.
    unsafe fn prepare(&self, sql: &str) -> Result<ffi::duckdb_prepared_statement, String> {
        let text = c_string(sql)?;
        unsafe {
            let mut prepared: ffi::duckdb_prepared_statement = ptr::null_mut();
            if ffi::duckdb_prepare(self.con, text.as_ptr(), &mut prepared) != ffi::DuckDBSuccess {
                let message = take_c_string(ffi::duckdb_prepare_error(prepared));
                ffi::duckdb_destroy_prepare(&mut prepared);
                return Err(message);
            }
            Ok(prepared)
        }
    }

    /// The rows of one query, as text.
    pub fn query(&self, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
        let mut rows = Vec::new();
        self.run(sql, &mut |t| rows = t.rows)?;
        Ok(rows)
    }

    /// The first column of the first row of one query (`None` for NULL or no row).
    pub fn value(&self, sql: &str) -> Result<Option<String>, String> {
        Ok(self
            .query(sql)?
            .into_iter()
            .next()
            .and_then(|row| row.into_iter().next().flatten()))
    }
}

/// Execute and destroy a prepared statement; its result set, if it returns rows.
unsafe fn execute(prepared: &mut ffi::duckdb_prepared_statement) -> Result<Option<Table>, String> {
    unsafe {
        let mut result: ffi::duckdb_result = std::mem::zeroed();
        let state = ffi::duckdb_execute_prepared(*prepared, &mut result);
        ffi::duckdb_destroy_prepare(prepared);
        let outcome = if state != ffi::DuckDBSuccess {
            Err(take_c_string(ffi::duckdb_result_error(&mut result)))
        } else if ffi::duckdb_result_return_type(result)
            == ffi::duckdb_result_type_DUCKDB_RESULT_TYPE_QUERY_RESULT
        {
            Ok(Some(table(&mut result)))
        } else {
            Ok(None)
        };
        ffi::duckdb_destroy_result(&mut result);
        outcome
    }
}

fn is_numeric(column_type: ffi::duckdb_type) -> bool {
    matches!(
        column_type,
        ffi::DUCKDB_TYPE_DUCKDB_TYPE_TINYINT
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_SMALLINT
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_INTEGER
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_BIGINT
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_UTINYINT
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_USMALLINT
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_UINTEGER
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_UBIGINT
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_HUGEINT
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_UHUGEINT
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_FLOAT
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_DOUBLE
            | ffi::DUCKDB_TYPE_DUCKDB_TYPE_DECIMAL
    )
}

unsafe fn table(result: &mut ffi::duckdb_result) -> Table {
    unsafe {
        let columns = ffi::duckdb_column_count(result);
        let names = (0..columns)
            .map(|c| take_c_string(ffi::duckdb_column_name(result, c)))
            .collect();
        let numeric = (0..columns)
            .map(|c| is_numeric(ffi::duckdb_column_type(result, c)))
            .collect();
        let rows = (0..ffi::duckdb_row_count(result))
            .map(|r| {
                (0..columns)
                    .map(|c| {
                        if ffi::duckdb_value_is_null(result, c, r) {
                            return None;
                        }
                        let text = ffi::duckdb_value_varchar(result, c, r);
                        let value = take_c_string(text);
                        if !text.is_null() {
                            ffi::duckdb_free(text.cast());
                        }
                        Some(value)
                    })
                    .collect()
            })
            .collect();
        Table {
            names,
            numeric,
            rows,
            explain: false,
        }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        unsafe {
            ffi::duckdb_disconnect(&mut self.con);
            ffi::duckdb_close(&mut self.db);
        }
    }
}

/// `text` as a SQL string literal.
pub fn sql_string(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// `items` as a SQL `VARCHAR[]` literal.
pub fn sql_string_list(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| sql_string(s)).collect();
    format!("[{}]::VARCHAR[]", inner.join(", "))
}
