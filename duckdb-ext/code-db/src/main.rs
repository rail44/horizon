//! `code-db`: the code database's entry point (duckdb-ext/sql/README.md,
//! Running). Everything a launcher decides lives here: the analysed
//! repository's settings file, DuckDB's resource limits, the extensions and
//! SQL files to read and their order, and the indexers. DuckDB receives the
//! results as variables and tables.

mod db;
mod index;
mod limits;
mod render;
mod script;
mod settings;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use db::{Db, sql_string, sql_string_list};
use script::Output;
use settings::Settings;

const USAGE: &str = "\
Usage: code-db COMMAND REPO [ARGS...]
  index REPO [tool_memory_max=SIZE]   write REPO/target/code-db/jscpd/{exact,normalized}
                                      (jscpd over the source files) and one SCIP index per
                                      Cargo workspace (rust-analyzer scip) and per Go module
                                      (scip-go index) in REPO/target/code-db/scip/, each
                                      tool under a memory cap (README.md, Running and Limits)
  query REPO QUERY [NAME=VALUE ...]   load the views and the persistent cache, then run
                                      queries/QUERY.sql and print the result
  sql REPO FILE.sql [NAME=VALUE ...]  the same loading, then FILE.sql
  shell REPO [NAME=VALUE ...]         the same loading, then an interactive session
  limits REPO [--json] [NAME=VALUE ...]
                                      print the limits the loading would set (memory_limit,
                                      threads, temp_limit override them)

NAME=VALUE sets a DuckDB variable before anything is read: a number, true, false or
NULL is passed as it is, anything else as a string. NAME:=EXPR passes EXPR as SQL text
(for lists and maps: 'exclude_patterns:=['\"'\"'src/*'\"'\"']'). Two names belong to this
command: FORMAT (a DuckDB CLI mode, default markdown; box, csv, json ...) and LIMIT (the
number of rows printed, by wrapping the query in a SELECT ... LIMIT; query only).
REPO/.code-db.toml, when it exists, holds the repository's settings (README.md,
Settings file); a NAME=VALUE argument overrides the variable it sets.

Names (README.md, Names) are the repository language's: package and module for Rust,
module and package for Go, in query names, parameters and results, and in the views of
the schema named after the language (go.units). A repository of several languages is
read one at a time: lang=NAME.

The views are read in README.md's order, then refresh.sql, which keeps the cache in
REPO/target/code-db/cache/. A query needs the SCIP indexes, and the jscpd reports when its
header says so. The limits line goes to stderr. Markdown is printed by this command; any
other FORMAT, and shell, hand the same loading to the DuckDB CLI.
Environment: DUCKDB_CLI (default ~/.local/bin/duckdb2), RUST_ANALYZER (default
rust-analyzer), SCIP_GO (default scip-go), JSCPD (default the refactor audit's pinned
jscpd).
";

/// The view files, in the order every client reads them (README.md, Running).
const VIEW_FILES: &[&str] = &[
    "00_load.sql",
    "views/files.sql",
    "views/scip.sql",
    "lang/rust.sql",
    "lang/go.sql",
    "views/terms.sql",
    "views/units.sql",
    "views/graph.sql",
    "views/clones.sql",
    "views/quality.sql",
];
/// The view files `index` reads to list the source files.
const FILE_VIEW_FILES: &[&str] = &["00_load.sql", "views/files.sql"];
/// The view files read to find the display language and its names (views/terms.sql).
const TERM_VIEW_FILES: &[&str] = &[
    "00_load.sql",
    "views/files.sql",
    "views/scip.sql",
    "lang/rust.sql",
    "lang/go.sql",
    "views/terms.sql",
];
/// What a query's header says when it needs an input `index` writes.
const NEEDS_INDEX: &str = "Needs the SCIP indexes";
const NEEDS_CLONES: &str = "Needs the jscpd reports";
const SQL_DIR: &str = "duckdb-ext/sql";
const EXTENSION_DIR: &str = "duckdb-ext/target/extension/v2.0.0/host";
/// The variables the limits are derived from; the derived values replace them.
const LIMIT_NAMES: &[&str] = &["memory_limit", "threads", "temp_limit"];

fn die(message: impl std::fmt::Display) -> ! {
    eprintln!("code-db: {message}");
    std::process::exit(1)
}

fn usage() -> ! {
    eprint!("{USAGE}");
    std::process::exit(2)
}

/// The root of the Horizon checkout this binary was built in: the SQL files
/// and the extensions are read from there, and 00_load.sql loads the
/// extensions by paths relative to it.
fn horizon_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|e| die(format!("cannot find the Horizon checkout: {e}")))
}

/// The arguments after the subcommand's own.
#[derive(Default)]
struct Arguments {
    /// (name, SQL expression) in the order given.
    variables: Vec<(String, String)>,
    /// The raw values of memory_limit, threads and temp_limit.
    overrides: limits::Overrides,
    format: Option<String>,
    limit: Option<u64>,
}

fn is_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// run.sh's rule: a number, true, false or NULL as it is, anything else as a string.
fn literal(value: &str) -> String {
    let number = {
        let v = value.strip_prefix('-').unwrap_or(value);
        let (int, frac) = v.split_once('.').unwrap_or((v, "0"));
        !int.is_empty()
            && !frac.is_empty()
            && int.chars().all(|c| c.is_ascii_digit())
            && frac.chars().all(|c| c.is_ascii_digit())
    };
    if number || matches!(value, "true" | "false" | "NULL" | "null") {
        value.to_owned()
    } else {
        sql_string(value)
    }
}

fn parse_arguments(args: &[String]) -> Arguments {
    let mut parsed = Arguments::default();
    for arg in args {
        let (name, value, expression) = if let Some((n, v)) = arg.split_once(":=") {
            (n, v, true)
        } else if let Some((n, v)) = arg.split_once('=') {
            (n, v, false)
        } else {
            die(format!("expected NAME=VALUE, got: {arg}"))
        };
        if !is_name(name) {
            die(format!("not a variable name: {name}"));
        }
        match name {
            "FORMAT" => parsed.format = Some(value.to_owned()),
            "LIMIT" => {
                parsed.limit = Some(
                    value
                        .parse()
                        .ok()
                        .filter(|_| value.chars().all(|c| c.is_ascii_digit()))
                        .unwrap_or_else(|| die(format!("LIMIT must be a number: {value}"))),
                )
            }
            _ if LIMIT_NAMES.contains(&name) => {
                if expression {
                    die(format!("{name} takes NAME=VALUE, not NAME:=EXPR"));
                }
                let value = Some(value.to_owned());
                match name {
                    "memory_limit" => parsed.overrides.memory_limit = value,
                    "threads" => parsed.overrides.threads = value,
                    _ => parsed.overrides.temp_limit = value,
                }
            }
            _ => parsed.variables.push((
                name.to_owned(),
                if expression {
                    value.to_owned()
                } else {
                    literal(value)
                },
            )),
        }
    }
    parsed
}

/// DuckDB's own defaults, read from a fresh in-memory database.
fn duckdb_defaults(db: &Db) -> limits::DuckDbDefaults {
    let setting = |name: &str| {
        db.value(&format!("SELECT current_setting('{name}')::VARCHAR"))
            .unwrap_or_else(|e| die(e))
            .unwrap_or_default()
    };
    let memory = setting("memory_limit");
    limits::DuckDbDefaults {
        memory_bytes: limits::setting_bytes(&memory)
            .unwrap_or_else(|| die(format!("cannot read DuckDB's memory_limit: {memory}"))),
        cpus: setting("threads")
            .parse()
            .unwrap_or_else(|e| die(format!("cannot read DuckDB's threads: {e}"))),
        max_temp: setting("max_temp_directory_size"),
    }
}

/// The limits for REPO, and the spill directory they assume.
fn derive_limits(db: &Db, repo: &Path, overrides: &limits::Overrides) -> (limits::Limits, PathBuf) {
    let temp_directory = repo.join("target/code-db/tmp");
    std::fs::create_dir_all(&temp_directory)
        .unwrap_or_else(|e| die(format!("{}: {e}", temp_directory.display())));
    let available = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| limits::mem_available(&m));
    let derived = limits::derive(
        &duckdb_defaults(db),
        available,
        limits::free_bytes(&temp_directory),
        overrides,
    )
    .unwrap_or_else(|e| die(e));
    (derived, temp_directory)
}

/// Everything set before the first SQL file is read: the repository, the
/// limits, the settings file's values, then the command line's variables.
fn prelude(
    repo: &Path,
    limits: &limits::Limits,
    temp_directory: &Path,
    settings: &Settings,
    variables: &[(String, String)],
) -> String {
    let mut sql = String::new();
    let mut set = |name: &str, value: String| {
        sql.push_str(&format!("SET VARIABLE {name} = {value};\n"));
    };
    set("repo", sql_string(&repo.display().to_string()));
    set("memory_limit", sql_string(&limits.memory_limit));
    set("threads", limits.threads.to_string());
    if let Some(temp) = &limits.temp_limit {
        set("temp_limit", sql_string(temp));
    }
    set(
        "temp_directory",
        sql_string(&temp_directory.display().to_string()),
    );
    set("limits", sql_string(&limits.line));
    let lists = [
        ("exclude_patterns", settings.exclude_patterns()),
        ("extra_test_paths", settings.test_paths()),
        ("extra_test_attributes", settings.test_attributes()),
    ];
    for (name, values) in lists {
        if !values.is_empty() {
            set(name, sql_string_list(&values));
        }
    }
    let table = |name: &str, columns: [&str; 2], rows: Vec<(String, String)>| {
        let mut sql = format!(
            "CREATE OR REPLACE TABLE {name} ({} VARCHAR, {} VARCHAR);\n",
            columns[0], columns[1]
        );
        if !rows.is_empty() {
            let values: Vec<String> = rows
                .iter()
                .map(|(a, b)| format!("({}, {})", sql_string(a), sql_string(b)))
                .collect();
            sql.push_str(&format!(
                "INSERT INTO {name} VALUES {};\n",
                values.join(", ")
            ));
        }
        sql
    };
    if !settings.layers.is_empty() {
        sql.push_str(&table(
            "config_layers",
            ["name", "namespace_pattern"],
            settings.layer_patterns(),
        ));
        sql.push_str(&table(
            "config_layer_deps",
            ["name", "may_depend_on"],
            settings.layer_deps(),
        ));
    }
    for (name, value) in variables {
        sql.push_str(&format!("SET VARIABLE {name} = {value};\n"));
    }
    sql
}

/// The SQL files to read, relative to the Horizon root.
fn load_files() -> Vec<String> {
    let mut files: Vec<&str> = VIEW_FILES.to_vec();
    files.push("refresh.sql");
    files.iter().map(|f| format!("{SQL_DIR}/{f}")).collect()
}

/// Whether FILE's text says it needs WHAT (NEEDS_INDEX, NEEDS_CLONES).
fn needs(file: &Path, what: &str) -> bool {
    std::fs::read_to_string(file).is_ok_and(|t| t.contains(what))
}

/// The display language's official names for the views' internal words, as
/// (internal, official) pairs (views/terms.sql, lang_terms): its two levels;
/// none without a display language.
#[derive(Default)]
struct Terms {
    pairs: Vec<(String, String)>,
}

/// TEXT with each word of PAIRS (from, to) replaced by its counterpart, all at
/// once (Go's names swap package and namespace), a word being a whole run
/// between non-alphanumeric characters (`_` included) or its plural, as
/// views/terms.sql's term_swap does.
fn swap_words(text: &str, pairs: &[(&str, &str)]) -> String {
    let bytes = text.as_bytes();
    let boundary = |i: usize| i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
    let ends = |i: usize| i >= bytes.len() || !bytes[i].is_ascii_alphanumeric();
    let mut out = String::new();
    let mut i = 0;
    'scan: while i < text.len() {
        if boundary(i) {
            for (from, to) in pairs {
                if text[i..].starts_with(from) {
                    let end = i + from.len();
                    if ends(end) {
                        out.push_str(to);
                        i = end;
                        continue 'scan;
                    }
                    if bytes.get(end) == Some(&b's') && ends(end + 1) {
                        out.push_str(to);
                        out.push('s');
                        i = end + 1;
                        continue 'scan;
                    }
                }
            }
        }
        let c = text[i..].chars().next().expect("in bounds");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

impl Terms {
    fn display(&self, text: &str) -> String {
        let pairs: Vec<(&str, &str)> = self
            .pairs
            .iter()
            .map(|(i, o)| (i.as_str(), o.as_str()))
            .collect();
        swap_words(text, &pairs)
    }

    fn internal(&self, text: &str) -> String {
        let pairs: Vec<(&str, &str)> = self
            .pairs
            .iter()
            .map(|(i, o)| (o.as_str(), i.as_str()))
            .collect();
        swap_words(text, &pairs)
    }
}

/// The variables whose values are vocabulary (views/terms.sql, display_column).
const VOCABULARY_VARIABLES: &[&str] = &["kind", "level", "unit_kind"];

struct Session {
    root: PathBuf,
    repo: PathBuf,
    out: PathBuf,
    settings: Settings,
    args: Arguments,
    terms: Terms,
}

impl Session {
    fn new(root: PathBuf, repo: PathBuf, args: &[String]) -> Session {
        let settings = Settings::load(&repo).unwrap_or_else(|e| die(e));
        let out = repo.join("target/code-db");
        Session {
            root,
            repo,
            out,
            settings,
            args: parse_arguments(args),
            terms: Terms::default(),
        }
    }

    /// Find the display language (views/terms.sql) by reading the views up to
    /// views/terms.sql, stop when the repository has several languages and
    /// `lang` names none, and turn the command line's variable names and
    /// vocabulary values into the views' internal names.
    fn translate(&mut self) {
        std::env::set_current_dir(&self.root).unwrap_or_else(|e| die(e));
        let db = Db::open().unwrap_or_else(|e| die(e));
        let mut sql = format!(
            "SET VARIABLE repo = {};\n",
            sql_string(&self.repo.display().to_string())
        );
        let excluded = self.settings.exclude_patterns();
        if !excluded.is_empty() {
            sql.push_str(&format!(
                "SET VARIABLE exclude_patterns = {};\n",
                sql_string_list(&excluded)
            ));
        }
        for (name, value) in &self.args.variables {
            sql.push_str(&format!("SET VARIABLE {name} = {value};\n"));
        }
        script::run_text(&db, &sql, &mut Output::Discard).unwrap_or_else(|e| die(e));
        for file in TERM_VIEW_FILES {
            script::run_file(
                &db,
                Path::new(&format!("{SQL_DIR}/{file}")),
                &mut Output::Discard,
            )
            .unwrap_or_else(|e| die(e));
        }
        let row = db
            .query(
                "SELECT display_language(), \
                        (SELECT string_agg(DISTINCT language, ', ' ORDER BY language) FROM files)",
            )
            .unwrap_or_else(|e| die(e))
            .into_iter()
            .next()
            .unwrap_or_default();
        let cell = |i: usize| row.get(i).cloned().flatten();
        let languages = cell(1).unwrap_or_default();
        if cell(0).is_none() && languages.contains(',') {
            die(format!(
                "{} holds several languages ({languages}): name the one to read with lang=NAME",
                self.repo.display()
            ));
        }
        self.terms.pairs = db
            .query(
                "SELECT internal, official FROM lang_terms \
                 WHERE language = display_language() AND official IS NOT NULL AND official <> internal \
                 ORDER BY length(official) DESC, internal",
            )
            .unwrap_or_else(|e| die(e))
            .into_iter()
            .filter_map(|r| {
                let mut r = r.into_iter();
                Some((r.next().flatten()?, r.next().flatten()?))
            })
            .collect();
        for (name, value) in &mut self.args.variables {
            *name = self.terms.internal(name);
            if VOCABULARY_VARIABLES.contains(&name.as_str()) {
                *value = self.terms.internal(value);
            }
        }
    }

    /// Whether `index` has written at least one SCIP index.
    fn indexed(&self) -> bool {
        std::fs::read_dir(self.out.join("scip")).is_ok_and(|d| {
            d.filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().ends_with(".scip"))
        })
    }

    fn check_tools(&self) {
        for ext in ["git_history", "scip_index"] {
            if !self
                .root
                .join(EXTENSION_DIR)
                .join(format!("{ext}.duckdb_extension"))
                .is_file()
            {
                die(format!(
                    "extension {ext} not built: run duckdb-ext/build.sh in {}",
                    self.root.display()
                ));
            }
        }
    }

    /// The inputs FILE's header says it needs (NEEDS_INDEX, NEEDS_CLONES) must
    /// exist; a query or SQL file that reads neither runs without them.
    fn check_inputs_for(&self, file: &Path, what: &str) {
        if needs(file, NEEDS_INDEX) && !self.indexed() {
            die(format!(
                "{what} needs the SCIP indexes in {}: run 'code-db index {}'",
                self.out.join("scip").display(),
                self.repo.display()
            ));
        }
        if needs(file, NEEDS_CLONES) {
            for mode in ["exact", "normalized"] {
                let report = self.out.join(format!("jscpd/{mode}/jscpd-report.json"));
                if !report.is_file() {
                    die(format!(
                        "{what} needs the jscpd report {}: run 'code-db index {}'",
                        report.display(),
                        self.repo.display()
                    ));
                }
            }
        }
    }

    /// Remove the cache files of other states of the inputs (refresh.sql writes
    /// one file per state, and one `ast-*.duckdb` file of syntax trees per
    /// syntax key), keeping the most recently modified one of each kind: the
    /// files the last read used.
    fn prune_cache(&self) {
        let Ok(dir) = std::fs::read_dir(self.out.join("cache")) else {
            return;
        };
        let mut files: Vec<(bool, std::time::SystemTime, PathBuf)> = dir
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".duckdb"))
            .filter_map(|e| {
                let trees = e.file_name().to_string_lossy().starts_with("ast-");
                Some((trees, e.metadata().ok()?.modified().ok()?, e.path()))
            })
            .collect();
        files.sort();
        let newest: Vec<usize> = (0..files.len())
            .filter(|&i| files.get(i + 1).is_none_or(|next| next.0 != files[i].0))
            .collect();
        for (i, (_, _, path)) in files.iter().enumerate() {
            if !newest.contains(&i) {
                let _ = std::fs::remove_file(path);
                let _ = std::fs::remove_file(path.with_extension("duckdb.wal"));
            }
        }
    }

    fn cli(&self) -> PathBuf {
        let cli = std::env::var_os("DUCKDB_CLI")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".local/bin/duckdb2")
            });
        if !cli.is_file() {
            die(format!(
                "DuckDB 2.0.0 CLI not found at {} (set DUCKDB_CLI)",
                cli.display()
            ));
        }
        cli
    }

    fn layered(&self) -> bool {
        !self.settings.layers.is_empty()
    }

    /// Open DuckDB, derive the limits and print them; the prelude SQL.
    fn start(&self) -> (Db, String) {
        std::fs::create_dir_all(self.out.join("cache"))
            .unwrap_or_else(|e| die(format!("{}: {e}", self.out.display())));
        std::env::set_current_dir(&self.root).unwrap_or_else(|e| die(e));
        let db = Db::open().unwrap_or_else(|e| die(e));
        let (limits, temp_directory) = derive_limits(&db, &self.repo, &self.args.overrides);
        eprintln!("{}", limits.line);
        let prelude = prelude(
            &self.repo,
            &limits,
            &temp_directory,
            &self.settings,
            &self.args.variables,
        );
        (db, prelude)
    }

    /// Load everything in this process.
    fn load(&self) -> Db {
        let (db, prelude) = self.start();
        script::run_text(&db, &prelude, &mut Output::Discard).unwrap_or_else(|e| die(e));
        for file in load_files() {
            script::run_file(&db, Path::new(&file), &mut Output::Discard)
                .unwrap_or_else(|e| die(self.terms.display(&e)));
        }
        if self.layered() {
            self.check_layers(&db);
        }
        let statements = db
            .query("SELECT statement FROM display_ddl ORDER BY ord")
            .unwrap_or_else(|e| die(e));
        for statement in statements
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
        {
            db.run_statement(&statement).unwrap_or_else(|e| die(e));
        }
        self.prune_cache();
        db
    }

    /// The source files, the workspaces and the Go modules to index
    /// (index::Inputs), from the views: 00_load.sql and views/files.sql read
    /// in this process.
    fn index_inputs(&self) -> index::Inputs {
        let (db, prelude) = self.start();
        script::run_text(&db, &prelude, &mut Output::Discard).unwrap_or_else(|e| die(e));
        for file in FILE_VIEW_FILES {
            script::run_file(
                &db,
                Path::new(&format!("{SQL_DIR}/{file}")),
                &mut Output::Discard,
            )
            .unwrap_or_else(|e| die(e));
        }
        let column = |sql: &str| -> Vec<String> {
            db.query(sql)
                .unwrap_or_else(|e| die(e))
                .into_iter()
                .filter_map(|r| r.into_iter().next().flatten())
                .collect()
        };
        let files = column("SELECT path FROM files ORDER BY path");
        let manifests = column(
            "SELECT path FROM git_tracked_files(getvariable('repo')) \
             WHERE path = 'Cargo.toml' OR path LIKE '%/Cargo.toml' ORDER BY path",
        );
        let workspaces = index::workspaces(&self.repo, &manifests, &files);
        let go_mods = column(
            "SELECT path FROM git_tracked_files(getvariable('repo')) \
             WHERE path = 'go.mod' OR path LIKE '%/go.mod' ORDER BY path",
        );
        let go_modules = index::go_modules(&go_mods, &files);
        let works: Vec<(String, String)> = column(
            "SELECT path FROM git_tracked_files(getvariable('repo')) \
             WHERE path = 'go.work' OR path LIKE '%/go.work' ORDER BY path",
        )
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(self.repo.join(&path)).ok()?;
            Some((path, text))
        })
        .collect();
        let go_indexes = index::go_indexes(&go_modules, &works);
        index::Inputs {
            files,
            workspaces,
            go_indexes,
        }
    }

    /// A namespace may belong to one layer only.
    fn check_layers(&self, db: &Db) {
        let rows = db
            .query("SELECT namespace, layer FROM namespace_layer_matches")
            .unwrap_or_else(|e| die(e));
        let matches: Vec<(String, String)> = rows
            .into_iter()
            .map(|r| {
                let mut r = r.into_iter();
                (
                    r.next().flatten().unwrap_or_default(),
                    r.next().flatten().unwrap_or_default(),
                )
            })
            .collect();
        let several = settings::namespaces_in_several_layers(&matches);
        if !several.is_empty() {
            let list: Vec<String> = several
                .iter()
                .map(|(m, layers)| format!("  {m}: {}", layers.join(", ")))
                .collect();
            die(format!(
                "{}: a {} matches the patterns of more than one layer:\n{}",
                self.repo.join(settings::FILE_NAME).display(),
                self.terms.display("namespace"),
                list.join("\n")
            ));
        }
    }

    /// A file under the output directory for SQL the CLI writes and reads
    /// back (`.output`, `.read`): the display statements.
    fn generated(&self, what: &str) -> PathBuf {
        self.out.join(format!("{what}-{}.sql", std::process::id()))
    }

    /// CLI lines that write SELECT's single text column to FILE, restore the
    /// output mode and read the file back as SQL.
    fn cli_generated(&self, select: &str, file: &Path) -> String {
        format!(
            ".mode list\n.headers off\n.output {}\n{select}\n.output\n.headers on\n.mode {}\n.read {}\n",
            file.display(),
            self.args.format.as_deref().unwrap_or("markdown"),
            file.display()
        )
    }

    /// The same loading as a DuckDB CLI script, its output dropped, with the
    /// display language's schema.
    fn cli_script(&self, prelude: &str) -> String {
        let mut script = String::from(prelude);
        script.push_str(".output /dev/null\n");
        for file in load_files() {
            script.push_str(&format!(".read {file}\n"));
        }
        if self.layered() {
            // namespace_layers fails when a namespace is in two layers.
            script.push_str("SELECT namespace, layer FROM namespace_layers;\n");
        }
        script.push_str(".output\n.maxrows -1\n.maxwidth 0\n");
        script.push_str(&self.cli_generated(
            "SELECT statement || ';' FROM display_ddl ORDER BY ord;",
            &self.generated("display"),
        ));
        script
    }

    /// Remove the files cli_generated wrote.
    fn remove_generated(&self) {
        for what in ["display", "query"] {
            let _ = std::fs::remove_file(self.generated(what));
        }
    }

    /// Run QUERY (one SELECT) after the loading and print its result in the
    /// display language (views/terms.sql, display_select).
    fn run_query(&self, query: &str) -> ! {
        let create = format!("CREATE OR REPLACE TEMP TABLE query_result AS\n{query}\n;\n");
        if self.args.format.as_deref().is_none_or(|f| f == "markdown") {
            let db = self.load();
            script::run_text(&db, &create, &mut Output::Discard)
                .unwrap_or_else(|e| die(self.terms.display(&e)));
            let select = db
                .value("SELECT display_select('query_result')")
                .unwrap_or_else(|e| die(e))
                .unwrap_or_else(|| die("the query has no columns"));
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let result = script::run_text(&db, &select, &mut Output::Markdown(&mut lock));
            let _ = lock.flush();
            drop(db);
            match result {
                Ok(()) => std::process::exit(0),
                Err(e) => die(self.terms.display(&e)),
            }
        }
        let text = format!(
            "{create}{}",
            self.cli_generated(
                "SELECT display_select('query_result') || ';';",
                &self.generated("query")
            )
        );
        self.run(&text)
    }

    /// Run TEXT (a query or a `.read`) after the loading: in this process for
    /// markdown, through the DuckDB CLI for any other FORMAT.
    fn run(&self, text: &str) -> ! {
        if self.args.format.as_deref().is_none_or(|f| f == "markdown") {
            let db = self.load();
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let result = script::run_text(&db, text, &mut Output::Markdown(&mut lock));
            let _ = lock.flush();
            drop(db);
            match result {
                Ok(()) => std::process::exit(0),
                Err(e) => die(self.terms.display(&e)),
            }
        }
        let cli = self.cli();
        let (db, prelude) = self.start();
        drop(db);
        let script = format!("{}{text}", self.cli_script(&prelude));
        let mut child = Command::new(&cli)
            .args(["-unsigned", "-bail", "-no-agent", "-batch"])
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| die(format!("{}: {e}", cli.display())));
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(script.as_bytes())
            .unwrap_or_else(|e| die(e));
        let status = child.wait().unwrap_or_else(|e| die(e));
        self.remove_generated();
        self.prune_cache();
        std::process::exit(status.code().unwrap_or(1))
    }
}

fn query(mut session: Session, rest: &[String]) -> ! {
    let Some(name) = rest.first() else { usage() };
    let name = name.strip_suffix(".sql").unwrap_or(name);
    session.translate();
    let queries = session.root.join(SQL_DIR).join("queries");
    let file = queries.join(format!("{}.sql", session.terms.internal(name)));
    if !file.is_file() {
        eprintln!("code-db: no query {name}; available:");
        let mut names: Vec<String> = std::fs::read_dir(&queries)
            .map(|d| {
                d.filter_map(|e| {
                    e.ok()?
                        .file_name()
                        .to_str()?
                        .strip_suffix(".sql")
                        .map(|n| session.terms.display(n))
                })
                .collect()
            })
            .unwrap_or_default();
        names.sort();
        for n in names {
            eprintln!("  {n}");
        }
        std::process::exit(1);
    }
    session.check_tools();
    session.check_inputs_for(&file, name);
    let text = std::fs::read_to_string(&file).unwrap_or_else(|e| die(e));
    let text = match session.args.limit {
        // run.sh's wrapping: each line's trailing `;` removed.
        Some(limit) => {
            let lines: Vec<&str> = text
                .lines()
                .map(|l| {
                    let t = l.trim_end_matches([' ', '\t']);
                    t.strip_suffix(';').map_or(l, |s| s)
                })
                .collect();
            format!("SELECT * FROM (\n{}\n) LIMIT {limit};\n", lines.join("\n"))
        }
        None => text,
    };
    session.run_query(&text)
}

fn sql_file(mut session: Session, rest: &[String]) -> ! {
    let Some(file) = rest.first() else { usage() };
    let file = Path::new(file)
        .canonicalize()
        .unwrap_or_else(|_| die(format!("no such file: {file}")));
    if session.args.limit.is_some() {
        die("LIMIT applies to query only");
    }
    session.translate();
    session.check_tools();
    session.check_inputs_for(&file, &file.display().to_string());
    session.run(&format!(".read {}\n", file.display()))
}

fn shell(mut session: Session) -> ! {
    session.translate();
    session.check_tools();
    if !session.indexed() {
        eprintln!(
            "code-db: no SCIP index in {}; the units and graph views are empty (run 'code-db index')",
            session.out.join("scip").display()
        );
    }
    let cli = session.cli();
    let (db, prelude) = session.start();
    drop(db);
    let mut init = session.cli_script(&prelude);
    init.push_str(".bail off\n");
    let path = session
        .out
        .join(format!("shell-init-{}.sql", std::process::id()));
    std::fs::write(&path, init).unwrap_or_else(|e| die(e));
    let status = Command::new(&cli)
        .args(["-unsigned", "-bail", "-init"])
        .arg(&path)
        .current_dir(&session.root)
        .status();
    let _ = std::fs::remove_file(&path);
    session.remove_generated();
    session.prune_cache();
    let status = status.unwrap_or_else(|e| die(format!("{}: {e}", cli.display())));
    std::process::exit(status.code().unwrap_or(1))
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn print_limits(repo: &Path, rest: &[String]) -> ! {
    let json = rest.iter().any(|a| a == "--json");
    let args: Vec<String> = rest.iter().filter(|a| *a != "--json").cloned().collect();
    let args = parse_arguments(&args);
    if !args.variables.is_empty() || args.format.is_some() || args.limit.is_some() {
        die("limits takes memory_limit, threads and temp_limit only");
    }
    let db = Db::open().unwrap_or_else(|e| die(e));
    let (limits, temp_directory) = derive_limits(&db, repo, &args.overrides);
    if json {
        println!(
            "{{\"memory_limit\": {}, \"threads\": {}, \"temp_limit\": {}, \"temp_directory\": {}, \"line\": {}}}",
            json_string(&limits.memory_limit),
            limits.threads,
            limits
                .temp_limit
                .as_deref()
                .map_or("null".to_owned(), json_string),
            json_string(&temp_directory.display().to_string()),
            json_string(&limits.line)
        );
    } else {
        println!("{}", limits.line);
    }
    std::process::exit(0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        usage();
    }
    let command = args[0].as_str();
    let repo = Path::new(&args[1]);
    if !repo.is_dir() {
        die(format!("no such directory: {}", args[1]));
    }
    let repo = repo.canonicalize().unwrap_or_else(|e| die(e));
    let rest = &args[2..];
    let root = horizon_root();
    match command {
        "index" => {
            let mut tool_memory_max = None;
            for arg in rest {
                match arg.strip_prefix("tool_memory_max=") {
                    Some(size) => tool_memory_max = Some(size.to_owned()),
                    None => usage(),
                }
            }
            let session = Session::new(root.clone(), repo.clone(), &[]);
            session.check_tools();
            let inputs = session.index_inputs();
            index::index(&root, &repo, &inputs, tool_memory_max.as_deref())
                .unwrap_or_else(|e| die(e));
        }
        "query" => {
            let session = Session::new(root, repo, rest.get(1..).unwrap_or_default());
            query(session, rest)
        }
        "sql" => {
            let session = Session::new(root, repo, rest.get(1..).unwrap_or_default());
            sql_file(session, rest)
        }
        "shell" => shell(Session::new(root, repo, rest)),
        "limits" => print_limits(&repo, rest),
        _ => usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_swap_whole_and_at_once() {
        let go = Terms {
            pairs: vec![
                ("package".to_owned(), "module".to_owned()),
                ("namespace".to_owned(), "package".to_owned()),
                ("closure".to_owned(), "func_literal".to_owned()),
            ],
        };
        assert_eq!(go.display("fan_in_packages"), "fan_in_modules");
        assert_eq!(go.display("namespace_graph"), "package_graph");
        assert_eq!(go.internal("package_graph"), "namespace_graph");
        assert_eq!(go.internal("min_packages"), "min_namespaces");
        assert_eq!(go.internal("'module'"), "'package'");
        assert_eq!(go.internal("func_literal_count"), "closure_count");
        assert_eq!(go.display("packaged"), "packaged");
        assert_eq!(Terms::default().display("namespace"), "namespace");
    }

    #[test]
    fn literals_follow_run_sh() {
        assert_eq!(literal("50"), "50");
        assert_eq!(literal("-1.5"), "-1.5");
        assert_eq!(literal("true"), "true");
        assert_eq!(literal("NULL"), "NULL");
        assert_eq!(literal("e3c619a0"), "'e3c619a0'");
        assert_eq!(literal("1."), "'1.'");
        assert_eq!(literal("it's"), "'it''s'");
    }
}
