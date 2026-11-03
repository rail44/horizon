//! The saved loading: the database the view files and refresh.sql leave
//! behind, kept as a file per state of everything that shapes it, so that a
//! later command attaches it instead of defining the views again.
//!
//! Defining the views live costs seconds on every command (DuckDB binds each
//! view through every view below it), while the same definitions over the
//! cached layers bind in milliseconds. A command therefore computes the key
//! of the loading (`key_sql`), builds `load-<key>.duckdb` in the cache
//! directory when it does not exist (`build`), and attaches it read-only with
//! private copies of the layer caches it reads (`Saved::attach_sql`). Its own
//! in-memory database still runs the prelude and 00_load.sql: their variables
//! and macros belong to the connection, and the key is made from them.
//!
//! Read-only attachments are what let several commands query at once: a
//! DuckDB file opened for writing locks out every other process, readers
//! included, and a reader locks out writers. Readers therefore never open a
//! file a build writes: the build writes refresh.sql's shared cache files
//! (under `load.lock`), then copies them beside the saved database, and
//! renames the saved database into place last, which is what makes a key
//! present.

use std::path::{Path, PathBuf};

use crate::db::{Db, sql_string, sql_string_list};

/// The files of one saved loading in the cache directory.
pub struct Saved {
    /// The database the loading left, its views pointing at `cache` and
    /// `ast_cache`.
    pub file: PathBuf,
    /// Copies of refresh.sql's cache file and syntax-tree file it was built
    /// with, attached under those names.
    pub cache: PathBuf,
    pub ast: PathBuf,
}

impl Saved {
    pub fn new(cache_dir: &Path, key: &str) -> Saved {
        Saved {
            file: cache_dir.join(format!("load-{key}.duckdb")),
            cache: cache_dir.join(format!("load-{key}.cache.duckdb")),
            ast: cache_dir.join(format!("load-{key}.ast.duckdb")),
        }
    }

    pub fn exists(&self) -> bool {
        self.file.is_file()
    }

    /// Attach the saved loading read-only, as `saved`, with the copies of the
    /// caches its views read by the names `cache` and `ast_cache`: the saved
    /// views bind inside their own database and reach those by name.
    pub fn attach_sql(&self) -> String {
        format!(
            "ATTACH {} AS cache (READ_ONLY);\nATTACH {} AS ast_cache (READ_ONLY);\n\
             ATTACH {} AS saved (READ_ONLY);\n",
            sql_string(&self.cache.display().to_string()),
            sql_string(&self.ast.display().to_string()),
            sql_string(&self.file.display().to_string()),
        )
    }
}

/// The saved database's schemas other than main (the display language's,
/// `rust`), once it is attached.
pub const SCHEMAS_SQL: &str = "SELECT schema_name FROM duckdb_schemas() \
     WHERE database_name = 'saved' AND schema_name NOT IN ('main', 'information_schema', 'pg_catalog') \
     ORDER BY schema_name";

/// Put the saved database's schemas on the search path after the
/// connection's own (`memory`), so that unqualified names and the display
/// language's schema (`rust.units`) resolve to it, while what a command
/// creates goes to the connection's database. SCHEMAS: as SCHEMAS_SQL.
pub fn search_path_sql(schemas: &[String]) -> String {
    let mut path = vec!["memory.main".to_owned(), "saved.main".to_owned()];
    path.extend(schemas.iter().map(|s| format!("saved.{s}")));
    format!("SET search_path = {};\n", sql_string(&path.join(",")))
}

/// The variables the loading files read (`getvariable('name')`), in order of
/// first appearance.
pub fn read_variables(texts: &[String]) -> Vec<String> {
    const OPEN: &str = "getvariable('";
    let mut names: Vec<String> = Vec::new();
    for text in texts {
        let mut rest = text.as_str();
        while let Some(at) = rest.find(OPEN) {
            rest = &rest[at + OPEN.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if rest[name.len()..].starts_with('\'') && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// The key of a loading, as one SQL value, read after the prelude and
/// 00_load.sql on the connection that will attach it. It covers whatever
/// shapes what the loading leaves:
/// - the text of every loading file (FILES, relative to the Horizon root);
/// - the values of the variables those files read (VARIABLES), the limits
///   (EXCLUDED) aside: the repository, the file snapshot with each file's
///   content id, the settings' patterns, `lang`, `languages`, the paths of
///   the indexes and reports;
/// - the content of each SCIP index and jscpd report, and of this
///   workspace's own extensions (EXTENSIONS);
/// - the loaded sitting_duck version (its grammars make the syntax trees);
/// - SETTINGS, the settings file's layers as the prelude passes them.
///
/// A variable only a query reads is not in it: the saved views read
/// variables when they are queried.
pub fn key_sql(
    files: &[String],
    variables: &[String],
    excluded: &[&str],
    extensions: &[String],
    settings: &str,
) -> String {
    let names: Vec<String> = variables
        .iter()
        .filter(|v| !excluded.contains(&v.as_str()))
        .cloned()
        .collect();
    format!(
        "SELECT md5(concat_ws(chr(10), 'code-db load 1',
    (SELECT string_agg(filename || ' ' || md5(content), chr(10) ORDER BY filename)
     FROM read_text({files})),
    (SELECT string_agg(name || '=' || coalesce(value::VARCHAR, 'NULL'), chr(10) ORDER BY name)
     FROM duckdb_variables() WHERE list_contains({names}, name)),
    (SELECT string_agg(filename || ' ' || md5(content), chr(10) ORDER BY filename)
     FROM read_blob(getvariable('scip_indexes') || getvariable('clone_reports') || {extensions})),
    (SELECT 'sitting_duck ' || extension_version FROM duckdb_extensions()
     WHERE extension_name = 'sitting_duck' AND loaded),
    {settings}))",
        files = sql_string_list(files),
        names = sql_string_list(&names),
        extensions = sql_string_list(extensions),
        settings = sql_string(settings),
    )
}

/// Build the saved loading SAVED unless another command did meanwhile: LOAD
/// runs the loading into a database at a temporary path in the cache
/// directory, then the build closes it, copies refresh.sql's cache files
/// beside it and renames it into place. `load.lock` keeps two builds from
/// writing the shared cache files at once.
pub fn build(
    cache_dir: &Path,
    saved: &Saved,
    load: impl FnOnce(&Db) -> Result<(), String>,
) -> Result<(), String> {
    let lock_path = cache_dir.join("load.lock");
    let lock =
        std::fs::File::create(&lock_path).map_err(|e| format!("{}: {e}", lock_path.display()))?;
    lock.lock()
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    if saved.exists() {
        return Ok(());
    }
    let building = saved.file.with_extension("duckdb.building");
    remove_database(&building);
    let (cache_file, ast_file) = {
        let db = Db::open_at(Some(&building))?;
        load(&db)?;
        let variable = |name: &str| -> Result<PathBuf, String> {
            db.value(&format!("SELECT getvariable('{name}')"))?
                .map(PathBuf::from)
                .ok_or_else(|| format!("refresh.sql left no {name}"))
        };
        let files = (variable("refresh_file")?, variable("ast_file")?);
        db.run("DETACH cache; DETACH ast_cache;", &mut |_| {})?;
        files
    };
    for (from, to) in [(&cache_file, &saved.cache), (&ast_file, &saved.ast)] {
        let part = to.with_extension("duckdb.building");
        std::fs::copy(from, &part).map_err(|e| format!("{}: {e}", from.display()))?;
        std::fs::rename(&part, to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    std::fs::rename(&building, &saved.file)
        .map_err(|e| format!("{}: {e}", saved.file.display()))?;
    Ok(())
}

/// Remove a database file and its write-ahead log.
pub fn remove_database(path: &Path) {
    let _ = std::fs::remove_file(path);
    let mut wal = path.as_os_str().to_owned();
    wal.push(".wal");
    let _ = std::fs::remove_file(PathBuf::from(wal));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_are_read_once_in_order() {
        let texts = vec![
            "SELECT getvariable('repo') || getvariable('lang');".to_owned(),
            "-- getvariable('repo') again, getvariable(name) not a literal\n\
             SET VARIABLE x = getvariable('source_files');"
                .to_owned(),
        ];
        assert_eq!(read_variables(&texts), vec!["repo", "lang", "source_files"]);
    }
}
