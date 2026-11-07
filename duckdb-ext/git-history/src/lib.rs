//! `git_history`: the DuckDB table function `git_tracked_files`, a
//! worktree's current files and the content id of each.
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
