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
                                      Cargo workspace in REPO/target/code-db/scip/
                                      (rust-analyzer scip), each tool under a memory cap
                                      (README.md, Running and Limits)
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

The views are read in README.md's order, then refresh.sql, which keeps the cache in
REPO/target/code-db/cache/. A query needs the SCIP indexes, and the jscpd reports when its
header says so. The limits line goes to stderr. Markdown is printed by this command; any
other FORMAT, and shell, hand the same loading to the DuckDB CLI.
Environment: DUCKDB_CLI (default ~/.local/bin/duckdb2), RUST_ANALYZER (default
rust-analyzer), JSCPD (default the refactor audit's pinned jscpd).
";

/// The view files, in the order every client reads them (README.md, Running).
const VIEW_FILES: &[&str] = &[
    "00_load.sql",
    "views/files.sql",
    "views/scip.sql",
    "views/units.sql",
    "views/graph.sql",
    "views/clones.sql",
    "views/quality.sql",
];
/// The view files `index` reads to list the source files.
const FILE_VIEW_FILES: &[&str] = &["00_load.sql", "views/files.sql"];
/// What a query's header says when it needs an input `index` writes.
const NEEDS_INDEX: &str = "Needs the SCIP indexes";
const NEEDS_CLONES: &str = "Needs the jscpd reports";
const SQL_DIR: &str = "duckdb-ext/sql";
const EXTENSION_DIR: &str = "duckdb-ext/target/extension/v2.0.0/linux_amd64";
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
            ["name", "module_pattern"],
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

struct Session {
    root: PathBuf,
    repo: PathBuf,
    out: PathBuf,
    settings: Settings,
    args: Arguments,
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
                .unwrap_or_else(|e| die(e));
        }
        if self.layered() {
            self.check_layers(&db);
        }
        self.prune_cache();
        db
    }

    /// The source files and the workspaces to index (index::Inputs), from the
    /// views: 00_load.sql and views/files.sql read in this process.
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
        index::Inputs { files, workspaces }
    }

    /// A module may belong to one layer only.
    fn check_layers(&self, db: &Db) {
        let rows = db
            .query("SELECT module, layer FROM module_layer_matches")
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
        let several = settings::modules_in_several_layers(&matches);
        if !several.is_empty() {
            let list: Vec<String> = several
                .iter()
                .map(|(m, layers)| format!("  {m}: {}", layers.join(", ")))
                .collect();
            die(format!(
                "{}: a module matches the patterns of more than one layer:\n{}",
                self.repo.join(settings::FILE_NAME).display(),
                list.join("\n")
            ));
        }
    }

    /// The same loading as a DuckDB CLI script, its output dropped.
    fn cli_script(&self, prelude: &str) -> String {
        let mut script = String::from(prelude);
        script.push_str(".output /dev/null\n");
        for file in load_files() {
            script.push_str(&format!(".read {file}\n"));
        }
        if self.layered() {
            // module_layers fails when a module is in two layers.
            script.push_str("SELECT module, layer FROM module_layers;\n");
        }
        script.push_str(&format!(
            ".output\n.headers on\n.mode {}\n.maxrows -1\n.maxwidth 0\n",
            self.args.format.as_deref().unwrap_or("markdown")
        ));
        script
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
                Err(e) => die(e),
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
        self.prune_cache();
        std::process::exit(status.code().unwrap_or(1))
    }
}

fn query(session: Session, rest: &[String]) -> ! {
    let Some(name) = rest.first() else { usage() };
    let name = name.strip_suffix(".sql").unwrap_or(name);
    let queries = session.root.join(SQL_DIR).join("queries");
    let file = queries.join(format!("{name}.sql"));
    if !file.is_file() {
        eprintln!("code-db: no query {name}; available:");
        let mut names: Vec<String> = std::fs::read_dir(&queries)
            .map(|d| {
                d.filter_map(|e| {
                    e.ok()?
                        .file_name()
                        .to_str()?
                        .strip_suffix(".sql")
                        .map(str::to_owned)
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
    session.run(&text)
}

fn sql_file(session: Session, rest: &[String]) -> ! {
    let Some(file) = rest.first() else { usage() };
    let file = Path::new(file)
        .canonicalize()
        .unwrap_or_else(|_| die(format!("no such file: {file}")));
    if session.args.limit.is_some() {
        die("LIMIT applies to query only");
    }
    session.check_tools();
    session.check_inputs_for(&file, &file.display().to_string());
    session.run(&format!(".read {}\n", file.display()))
}

fn shell(session: Session) -> ! {
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
