//! `git_history`: DuckDB table functions over a repository's git history.
//!
//! See `duckdb-ext/README.md` for the functions, their semantics and the
//! build route.

mod functions;

use duckdb_loadable_macros::duckdb_entrypoint_c_api_v2;
use duckdb_neo::{Result, extension::Extension};

#[duckdb_entrypoint_c_api_v2(extension_name = "git_history", api_version = "v2.0.0")]
pub fn entrypoint(extension: &Extension) -> Result<()> {
    functions::register(extension)
}
