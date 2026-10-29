# /// script
# requires-python = ">=3.12"
# dependencies = ["duckdb==2.0.0"]
# ///
"""Execute the code database SQL files in order and check their rows.

    uv run --locked --script duckdb-ext/sql/check.py fixture
        Build a git repository in a temporary directory (a Cargo workspace of two crates,
        a second workspace nested in it, a file outside every workspace, an excluded file,
        tests in the usual places), index each workspace with rust-analyzer (RUST_ANALYZER,
        default rust-analyzer on PATH), write a report in jscpd 5.3.1's shape and fill the
        settings' tables, then assert the rows of files, units, the graph, every axis
        query and unit_quality, live and through refresh.sql (built, then kept, then
        rebuilt after an edit).

    uv run --locked --script duckdb-ext/sql/check.py fixture --print
        The same fixture, printing each view's and query's rows instead of asserting.

    uv run --locked --script duckdb-ext/sql/check.py repo REPO
        Load REPO (its SCIP indexes and jscpd reports under REPO/target/code-db, as
        `code-db index` writes them), print the counts by unit kind and every query's
        first rows with its time.

    uv run --locked --script duckdb-ext/sql/check.py refresh REPO
        Read refresh.sql and compare every cached layer and every query with live.

    uv run --locked --script duckdb-ext/sql/check.py measure REPO [--runs 3]
        Time each query in a fresh process per run, live and through refresh.sql (its
        build and a kept cache), and print min-max wall time and peak RSS.

    `repo`, `refresh` and `measure` take --set NAME=EXPR (repeatable) to set a DuckDB
    variable on every connection. Every connection takes its limits from `code-db limits
    REPO --json` (CODE_DB, default duckdb-ext/code-db/target/release/code-db, built by
    duckdb-ext/build.sh), passing it memory_limit, threads and temp_limit from --set, and
    prints its limits line to stderr.

The process works from the root of the Horizon checkout holding this file, because
00_load.sql loads the extensions by paths relative to it.
"""

import argparse
import json
import os
import resource
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import duckdb

SQL_DIR = Path(__file__).resolve().parent
HORIZON_ROOT = SQL_DIR.parent.parent
VIEW_FILES = [
    "00_load.sql",
    "views/files.sql",
    "views/scip.sql",
    "views/units.sql",
    "views/graph.sql",
    "views/clones.sql",
    "views/quality.sql",
]
REFRESH_FILE = "refresh.sql"
# The layers refresh.sql copies, in its order.
CACHED_VIEWS = ["scip_occurrences", "scip_symbols", "units", "unit_ast", "unit_occurrences", "symbol_units",
                "unit_references", "unit_edges", "unit_ancestors", "impls", "impl_targets", "unit_impl_type",
                "modules", "unit_module", "module_edges", "unit_scope", "unit_fan_in", "unit_fan_out",
                "unit_reference_status", "unit_debt_markers", "unit_complexity"]
QUERIES = ["size", "duplication", "coupling", "interface", "public_surface", "intent", "unreferenced",
           "module_graph", "module_cycles", "layer_violations", "unit_dependencies", "complexity"]
RUST_ANALYZER = os.environ.get("RUST_ANALYZER", "rust-analyzer")


def sql_literal(value):
    if isinstance(value, list):
        return "[" + ", ".join(sql_literal(v) for v in value) + "]::VARCHAR[]"
    return "'" + str(value).replace("'", "''") + "'"


TEMP_DIRECTORY_STATEMENT = "SET temp_directory = coalesce(getvariable('temp_directory'), current_setting('temp_directory'));\n"


def without_temp_directory(con, text):
    """00_load.sql's text, without its SET temp_directory when the connection already
    has that directory: once DuckDB has spilled it refuses to set the directory again,
    even to the same path, so a re-read on this connection would fail."""
    assert TEMP_DIRECTORY_STATEMENT in text
    same = rows(con, "SELECT current_setting('temp_directory') = "
                     "coalesce(getvariable('temp_directory'), current_setting('temp_directory'))")[0][0]
    return text.replace(TEMP_DIRECTORY_STATEMENT, "") if same else text


def read_views(con):
    """Read the view files in order (live again after refresh.sql)."""
    for name in VIEW_FILES:
        text = (SQL_DIR / name).read_text()
        if name == "00_load.sql":
            text = without_temp_directory(con, text)
        con.execute(text)


# Variables from --set NAME=EXPR, set on every connection after connect()'s own;
# memory_limit, threads and temp_limit go to `code-db limits` instead (README.md, Limits).
SET_VARIABLES = {}
LIMIT_NAMES = ("memory_limit", "threads", "temp_limit")
CODE_DB = Path(os.environ.get("CODE_DB", HORIZON_ROOT / "duckdb-ext/code-db/target/release/code-db"))


def limit_variables(repo):
    """{name: SQL literal} of the limits `code-db limits REPO --json` derives (with the
    --set overrides of memory_limit, threads and temp_limit), the variables 00_load.sql
    applies; prints its limits line to stderr."""
    if not CODE_DB.exists():
        raise SystemExit(f"{CODE_DB} not found: run duckdb-ext/build.sh (or set CODE_DB)")
    overrides = [f"{name}={value.strip().strip(chr(39))}" for name, value in SET_VARIABLES.items()
                 if name in LIMIT_NAMES]
    proc = subprocess.run([str(CODE_DB), "limits", str(Path(repo).resolve()), "--json", *overrides],
                          capture_output=True, text=True)
    if proc.returncode:
        raise SystemExit(f"code-db limits failed: {proc.stderr.strip()}")
    limits = json.loads(proc.stdout)
    print(limits["line"], file=sys.stderr)
    variables = {"memory_limit": sql_literal(limits["memory_limit"]), "threads": str(limits["threads"]),
                 "temp_directory": sql_literal(limits["temp_directory"]), "limits": sql_literal(limits["line"])}
    if limits["temp_limit"] is not None:
        variables["temp_limit"] = sql_literal(limits["temp_limit"])
    return variables


def connect(repo, variables=None, layers=None):
    """A connection with the view files read for REPO. `variables` are set before the
    files (SQL text); `layers` is (patterns, deps), the rows of config_layers and
    config_layer_deps as code-db fills them from a settings file."""
    os.chdir(HORIZON_ROOT)
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"SET VARIABLE repo = {sql_literal(str(Path(repo).resolve()))}")
    for name, value in limit_variables(repo).items():
        con.execute(f"SET VARIABLE {name} = {value}")
    for name, value in {**(variables or {}), **SET_VARIABLES}.items():
        if name not in LIMIT_NAMES:
            con.execute(f"SET VARIABLE {name} = {value}")
    if layers:
        set_layers(con, *layers)
    read_views(con)
    return con


def set_layers(con, patterns, deps):
    con.execute("CREATE OR REPLACE TABLE config_layers (name VARCHAR, module_pattern VARCHAR)")
    con.execute("CREATE OR REPLACE TABLE config_layer_deps (name VARCHAR, may_depend_on VARCHAR)")
    for name, pattern in patterns:
        con.execute("INSERT INTO config_layers VALUES (?, ?)", [name, pattern])
    for name, dep in deps:
        con.execute("INSERT INTO config_layer_deps VALUES (?, ?)", [name, dep])


def query_sql(name):
    return (SQL_DIR / "queries" / f"{name}.sql").read_text()


def rows(con, sql):
    return con.execute(sql).fetchall()


def query(con, name, **variables):
    """The rows of queries/NAME.sql with the variables set for it (and reset after)."""
    for var, value in variables.items():
        con.execute(f"SET VARIABLE {var} = {value if isinstance(value, (int, bool)) else sql_literal(value)}")
    try:
        return rows(con, query_sql(name))
    finally:
        for var in variables:
            con.execute(f"RESET VARIABLE {var}")


def read_refresh(con):
    """Read refresh.sql; return (wall seconds, 'built' or 'kept', the cache file)."""
    start = time.perf_counter()
    result = con.execute((SQL_DIR / REFRESH_FILE).read_text()).fetchall()
    wall = time.perf_counter() - start
    cache_file, state, _ = result[-1]
    return wall, state, cache_file


def snapshot_live(con):
    """Copy every layer refresh.sql caches, read live, into temp tables live_<view>."""
    for view in CACHED_VIEWS:
        con.execute(f"CREATE OR REPLACE TEMP TABLE live_{view} AS SELECT * FROM {view}")


def diff_from_live(con):
    """{view: (live rows missing from the view, view rows missing from live)}, nonzero only."""
    diffs = {}
    for view in CACHED_VIEWS:
        counts = rows(con, f"SELECT (SELECT count(*) FROM (FROM live_{view} EXCEPT ALL FROM {view})), "
                           f"(SELECT count(*) FROM (FROM {view} EXCEPT ALL FROM live_{view}))")[0]
        if counts != (0, 0):
            diffs[view] = counts
    return diffs


def set_dependency_unit(con):
    """unit_dependencies needs a unit: TerminalSession.spawn when the repository has it
    (this repository), else the largest function. Set only when `path` is unset."""
    if rows(con, "SELECT getvariable('path') IS NOT NULL")[0][0]:
        return
    unit = rows(con, """
        SELECT path, kind, qualified_name FROM units WHERE kind IN ('function', 'method')
        ORDER BY (path = 'src/terminal/session.rs' AND qualified_name = 'TerminalSession.spawn') DESC,
                 loc DESC, unit_id
        LIMIT 1""")
    if unit:
        for var, value in zip(("path", "unit_kind", "qualified_name"), unit[0]):
            con.execute(f"SET VARIABLE {var} = {sql_literal(value)}")


def refresh_against_live(con):
    """Copy the live layers and the queries' rows, read refresh.sql, compare."""
    set_dependency_unit(con)
    snapshot_live(con)
    live_queries = {q: sorted(rows(con, query_sql(q)), key=repr) for q in QUERIES}
    live_quality = sorted(rows(con, "SELECT * FROM unit_quality"), key=repr)
    wall, state, cache_file = read_refresh(con)
    diffs = diff_from_live(con)
    assert not diffs, f"cached layers differ from live: {diffs}"
    for q in QUERIES:
        assert sorted(rows(con, query_sql(q)), key=repr) == live_queries[q], f"{q} differs after refresh"
    assert sorted(rows(con, "SELECT * FROM unit_quality"), key=repr) == live_quality, "unit_quality differs"
    return wall, state, cache_file


# --- fixture ---------------------------------------------------------------------

WORKSPACE_TOML = """\
[workspace]
members = ["shapes", "app"]
resolver = "2"
"""

SHAPES_TOML = """\
[package]
name = "shapes"
version = "0.1.0"
edition = "2021"
"""

APP_TOML = """\
[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
shapes = { path = "../shapes" }
"""

TOOLS_TOML = """\
[package]
name = "tools"
version = "0.1.0"
edition = "2021"

[workspace]
"""

SHAPES_LIB = """\
//! Shapes.
pub mod convert;
pub mod generated;
pub mod rect;

/// A shape with an area.
pub trait Shape {
    type Unit;
    fn area(&self) -> u32;
}

pub const UNIT: u32 = 1;

#[derive(Debug)]
pub struct Square {
    pub side: u32,
}

impl Shape for Square {
    type Unit = u32;
    fn area(&self) -> u32 {
        let side = self.side;
        let doubled = side * 2;
        let half = doubled / 2;
        let squared = half * side;
        squared + UNIT - UNIT
    }
}

impl Square {
    /// Make a square.
    pub fn new(side: u32) -> Self {
        Self { side }
    }

    // TODO: check for overflow
    pub(crate) fn scaled(&self, a: u32, b: u32, c: u32) -> u32 {
        self.side * a * b * c
    }
}

pub fn unused_helper() -> u32 {
    0
}

#[inline]
pub fn marked() -> u32 {
    1
}

#[cfg(test)]
impl Square {
    fn probe(&self) -> u32 {
        self.side
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_area() {
        assert_eq!(Square::new(2).area(), 4);
        assert_eq!(Square::new(1).probe(), 1);
    }
}
"""

SHAPES_RECT = """\
use crate::Shape;

pub struct Rect {
    pub side: u32,
}

impl Shape for Rect {
    type Unit = u64;
    fn area(&self) -> u32 {
        let side = self.side;
        let doubled = side * 2;
        let half = doubled / 2;
        let squared = half * side;
        squared + crate::UNIT - crate::UNIT
    }
}

pub fn ring() -> u32 {
    crate::convert::back() + 1
}

pub(crate) fn base() -> u32 {
    2
}
"""

SHAPES_CONVERT = """\
use crate::rect::Rect;
use crate::Square;

impl From<Square> for Rect {
    fn from(square: Square) -> Self {
        Rect { side: square.side }
    }
}

pub fn back() -> u32 {
    crate::rect::base()
}
"""

SHAPES_GENERATED = """\
pub fn generated() -> u32 {
    3
}
"""

APP_MAIN = """\
use shapes::{Shape, Square};

fn main() {
    let square = Square::new(2);
    println!("{}", square.area() + shapes::UNIT + shapes::rect::ring());
}
"""

APP_FIXTURES = """\
pub fn sample() -> shapes::Square {
    shapes::Square::new(3)
}
"""

APP_IT = """\
#[test]
fn integration() {
    assert_eq!(shapes::Square::new(1).side, 1);
}
"""

TOOLS_MAIN = """\
fn main() {
    run(1, 2);
}

fn run(a: u32, b: u32) -> u32 {
    a + b
}
"""

LOOSE = """\
fn loose() {}
"""


def git_env(repo):
    return {
        "PATH": os.environ["PATH"],
        "HOME": str(repo),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_AUTHOR_NAME": "Fixture",
        "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
        "GIT_COMMITTER_NAME": "Fixture",
        "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
    }


def git(repo, *args):
    subprocess.run(["git", *args], cwd=repo, env=git_env(repo), check=True, capture_output=True)


def write(repo, path, text):
    target = repo / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text)


FIXTURE_FILES = {
    "Cargo.toml": WORKSPACE_TOML,
    "shapes/Cargo.toml": SHAPES_TOML,
    "shapes/src/lib.rs": SHAPES_LIB,
    "shapes/src/rect.rs": SHAPES_RECT,
    "shapes/src/convert.rs": SHAPES_CONVERT,
    "shapes/src/generated.rs": SHAPES_GENERATED,
    "app/Cargo.toml": APP_TOML,
    "app/src/main.rs": APP_MAIN + "\nmod fixtures;\n",
    "app/src/fixtures.rs": APP_FIXTURES,
    "app/tests/it.rs": APP_IT,
    "tools/Cargo.toml": TOOLS_TOML,
    "tools/src/main.rs": TOOLS_MAIN,
    "scripts/loose.rs": LOOSE,
    ".gitignore": "target/\n",
}

# The settings code-db would read from the fixture's .code-db.toml.
FIXTURE_VARIABLES = {
    "exclude_patterns": "['shapes/src/generated.rs']",
    "extra_test_paths": "['app/src/fixtures.rs']",
    "extra_test_attributes": "['inline']",
}
FIXTURE_LAYERS = ([("rect", "shapes/src/rect.rs*"), ("convert", "shapes/src/convert.rs*")],
                  [("convert", "rect")])


def index_workspaces(repo):
    """One rust-analyzer index per workspace, named as code-db names them (index_name in
    duckdb-ext/code-db/src/index.rs)."""
    out = repo / "target/code-db/scip"
    out.mkdir(parents=True, exist_ok=True)
    for workspace, name in (("", "root.scip"), ("tools", "ws-tools.scip")):
        subprocess.run([RUST_ANALYZER, "scip", ".", "--output", str(out / name)], check=True,
                       capture_output=True, cwd=repo / workspace)


def line_of(text, needle):
    for number, line in enumerate(text.splitlines(), 1):
        if needle in line:
            return number
    raise AssertionError(f"{needle!r} not found")


def fixture_report(repo):
    """Two reports in jscpd 5.3.1's JSON shape: in `exact`, the two `area` bodies (Square's
    and Rect's, two types a From impl joins) and a pair between `run` of tools and
    Square.scaled; `normalized` is empty."""
    def side(path, start, end):
        return {"name": str(repo / path), "start": start, "end": end,
                "startLoc": {"line": start, "column": 1}, "endLoc": {"line": end, "column": 1}}

    lib_area = line_of(SHAPES_LIB, "let side = self.side;")
    rect_area = line_of(SHAPES_RECT, "let side = self.side;")
    scaled = line_of(SHAPES_LIB, "self.side * a * b * c")
    run = line_of(TOOLS_MAIN, "a + b")
    duplicates = [
        {"format": "rust", "lines": 5, "tokens": 80, "fragment": "",
         "firstFile": side("shapes/src/lib.rs", lib_area, lib_area + 4),
         "secondFile": side("shapes/src/rect.rs", rect_area, rect_area + 4)},
        {"format": "rust", "lines": 1, "tokens": 10, "fragment": "",
         "firstFile": side("shapes/src/lib.rs", scaled, scaled),
         "secondFile": side("tools/src/main.rs", run, run)},
    ]
    for mode, dups in (("exact", duplicates), ("normalized", [])):
        out = repo / "target/code-db/jscpd" / mode
        out.mkdir(parents=True, exist_ok=True)
        (out / "jscpd-report.json").write_text(json.dumps({"duplicates": dups, "statistics": {}}))


def build_fixture(root):
    repo = root / "repo"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    for path, text in FIXTURE_FILES.items():
        write(repo, path, text)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "fixture")
    index_workspaces(repo)
    fixture_report(repo)
    (repo / "target/code-db/cache").mkdir(parents=True, exist_ok=True)
    return repo


# The views and queries the fixture asserts, as (name, SQL); their expected rows are
# EXPECTED below. `check.py fixture --print` prints the same rows.
CHECKED = [
    ("files", "SELECT f.path, count(u.unit_id) FROM files f LEFT JOIN units u USING (file_id) GROUP BY ALL ORDER BY 1"),
    ("units", "SELECT u.path, u.kind, u.symbol_kind, u.qualified_name, p.qualified_name AS parent, u.start_line, "
              "u.end_line, u.name_line, u.loc, u.is_test, u.visibility, u.parameters, u.has_doc, u.is_file_module, "
              "u.member_of, u.impl_type, u.impl_trait, u.impl_header_line "
              "FROM units u LEFT JOIN units p ON p.unit_id = u.parent_unit_id ORDER BY u.unit_id"),
    ("impl_targets", "SELECT f.path, i.header_line, i.impl_type, i.impl_trait, t.part, u.qualified_name, t.resolved_by "
                     "FROM impl_targets t JOIN impls i USING (impl_id) JOIN files f USING (file_id) "
                     "LEFT JOIN units u ON u.unit_id = t.target_unit_id ORDER BY ALL"),
    ("unit_edges", "SELECT s.path, s.qualified_name, d.path, d.qualified_name, e.symbols, e.occurrences, e.is_self, "
                   "e.ambiguous, e.src_is_test, e.dst_is_test FROM unit_edges e JOIN units s ON s.unit_id = e.src_unit_id "
                   "JOIN units d ON d.unit_id = e.dst_unit_id ORDER BY ALL"),
    ("modules", "SELECT module, crate, module_path FROM modules ORDER BY ALL"),
    ("crate_edges", "SELECT * FROM crate_edges ORDER BY ALL"),
    ("module_cycles", "SELECT cycle, crate, modules, members, edges FROM module_cycles ORDER BY ALL"),
    ("unit_quality", "SELECT * EXCLUDE (unit_id) FROM unit_quality ORDER BY unit_id"),
    *[(f"query {q}", q) for q in QUERIES],
    ("query size level=file", ("size", {"level": "file"})),
    ("query size level=crate", ("size", {"level": "crate"})),
    ("query coupling kind=type min_crates=1", ("coupling", {"kind": "type", "min_crates": 1})),
    ("query unreferenced include_structural", ("unreferenced", {"include_structural": True})),
    ("query duplication include_tests source=exact", ("duplication", {"include_tests": True, "source": "exact"})),
]


def checked_rows(con, sql):
    if isinstance(sql, tuple):
        return query(con, sql[0], **sql[1])
    if sql in QUERIES:
        return rows(con, query_sql(sql))
    return rows(con, sql)


def print_rows(con, title, result_rows):
    print(f"    {title!r}: [")
    for r in result_rows:
        print(f"        {r!r},")
    print("    ],")


def check_fixture(print_only=False):
    """The fixture's rows; see the module doc. Skipped when rust-analyzer is missing."""
    if shutil.which(RUST_ANALYZER) is None:
        print(f"fixture: skipped, {RUST_ANALYZER} not on PATH")
        return
    with tempfile.TemporaryDirectory(prefix="code-db-fixture-") as tmp:
        repo = build_fixture(Path(tmp))
        con = connect(repo, FIXTURE_VARIABLES, FIXTURE_LAYERS)
        con.execute("SET VARIABLE path = 'shapes/src/rect.rs'")
        con.execute("SET VARIABLE unit_kind = 'function'")
        con.execute("SET VARIABLE qualified_name = 'ring'")
        if print_only:
            for title, sql in CHECKED:
                print_rows(con, title, checked_rows(con, sql))
            check_flow_fixture(Path(tmp), print_only)
            return
        assert_fixture(con, repo)
        # Live and cached agree; a second read keeps the file; an edit builds another.
        wall, state, first = refresh_against_live(con)
        assert state == "built", state
        assert_fixture(con, repo)
        read_views(con)
        _, state, again = read_refresh(con)
        assert (state, again) == ("kept", first), (state, again, first)
        assert_error(con, (SQL_DIR / REFRESH_FILE).read_text(), "the views read a cache already")
        write(repo, "shapes/src/rect.rs", SHAPES_RECT + "\npub fn added() {}\n")
        read_views(con)
        _, state, edited = refresh_against_live(con)
        assert state == "built" and edited != first, (state, edited, first)
        assert rows(con, "SELECT count(*) FROM units WHERE qualified_name = 'added'") == [(0,)], \
            "the index describes the tree it was made from"
        # Without indexes and reports the views exist and are empty; the files are listed.
        bare = connect(repo, {**FIXTURE_VARIABLES, "scip_indexes": "[]::VARCHAR[]",
                              "clone_reports": "[]::VARCHAR[]"})
        assert rows(bare, "SELECT count(*) FROM files") == [(8,)]
        for view in ("units", "unit_edges", "module_cycles", "clone_pairs", "unit_quality"):
            assert rows(bare, f"SELECT count(*) FROM {view}") == [(0,)], view
        check_flow_fixture(Path(tmp))
        print(f"fixture: ok (refresh built in {wall:.2f} s)")


# --- syntax-tree fixture -------------------------------------------------------------

FLOW_TOML = """\
[package]
name = "flow"
version = "0.1.0"
edition = "2024"
"""

# Functions of known shapes; FLOW_EXPECTED holds their counts, read off this text.
FLOW_LIB = """\
pub fn nested_loop(items: &[i32]) -> i32 {
    let mut total = 0;
    for x in items {
        if *x > 0 {
            if *x > 10 {
                total += 2;
            } else if *x > 5 {
                total += 1;
            }
        } else {
            continue;
        }
    }
    total
}

pub fn classify(v: Option<i32>, w: i32) -> &'static str {
    match v {
        Some(n) if n > 0 => match w {
            0 => "zero",
            _ => "pos",
        },
        Some(n) if n < 0 && w > 0 || w < -5 => "neg",
        Some(_) => "nil",
        None => "none",
    }
}

pub fn chain(a: &str, b: &str) -> Result<i32, std::num::ParseIntError> {
    let x: i32 = a.parse()?;
    let y: i32 = b.trim().parse()?;
    Ok(x + y)
}

pub fn closures(v: &[i32]) -> Vec<i32> {
    let keep = |x: &i32| if *x > 1 { true } else { false };
    v.iter()
        .copied()
        .filter(|x| keep(x))
        .map(|x| match x {
            2 => 0,
            _ => x,
        })
        .collect()
}

pub fn outer(flag: bool) -> i32 {
    fn inner(n: i32) -> i32 {
        if n > 0 { loop { break n; } } else { 0 }
    }
    if flag {
        return inner(1);
    }
    let Some(v) = Some(2) else { return 0; };
    v
}

pub fn with_macro(x: i32) -> bool {
    assert!(if x > 0 { true } else { x == 0 });
    matches!(x, 1 | 2 if x > 0)
}

pub fn while_let(mut it: std::vec::IntoIter<i32>) -> i32 {
    let mut s = 0;
    while let Some(x) = it.next() {
        if let Some(y) = x.checked_mul(2) && y > 4 {
            s += y;
        }
    }
    s
}

pub mod picks {
    pub fn pick(a: bool) -> i32 {
        if a { 1 } else { 2 }
    }
}
"""

FLOW_FILES = {
    "Cargo.toml": FLOW_TOML,
    "src/lib.rs": FLOW_LIB,
    ".gitignore": "target/\n",
}

FLOW_CHECKED = [
    ("unit_complexity", "SELECT u.qualified_name, u.kind, c.* EXCLUDE (unit_id) FROM unit_complexity c "
                        "JOIN units u USING (unit_id) ORDER BY u.unit_id"),
    # Each tree is whole: its root is a source_file spanning the unit's lines, nothing
    # failed to parse, and an enclosing unit's tree holds the nodes of the units nested in
    # it, which its own nodes (unit_own_ast) leave out.
    ("trees", "SELECT u.qualified_name, count(*) AS nodes, "
              "count(*) FILTER (WHERE a.node_id = 0 AND a.type = 'source_file' AND a.depth = 0 "
              "AND a.file_start_line = u.start_line AND a.file_end_line = u.end_line) AS root, "
              "count(*) FILTER (WHERE a.type = 'ERROR' OR a.type LIKE 'MISSING%') AS errors, "
              "count(*) FILTER (WHERE a.type = 'if_expression') AS if_nodes, "
              "(SELECT count(*) FROM unit_own_ast o WHERE o.unit_id = u.unit_id) AS own_nodes "
              "FROM units u JOIN unit_ast a USING (unit_id) GROUP BY u.unit_id, u.qualified_name ORDER BY u.unit_id"),
    # A macro's arguments are tokens: the `if` of with_macro is a keyword token in a
    # token_tree, pick's is an if_expression; file lines are the file's.
    ("macro_tokens", "SELECT u.qualified_name, a.type, p.type, a.file_start_line FROM unit_ast a "
                     "JOIN units u USING (unit_id) "
                     "JOIN unit_ast p ON p.unit_id = a.unit_id AND p.node_id = a.parent_id "
                     "WHERE u.qualified_name IN ('with_macro', 'picks.pick') AND a.type IN ('if', 'if_expression') "
                     "ORDER BY u.unit_id, a.node_id"),
    ("query complexity", "complexity"),
]


def check_flow_fixture(root, print_only=False):
    """A crate of known control-flow shapes (FLOW_LIB): its trees and the counts over
    them, live and through refresh.sql."""
    repo = root / "flow"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    for path, text in FLOW_FILES.items():
        write(repo, path, text)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "flow")
    (repo / "target/code-db/scip").mkdir(parents=True)
    subprocess.run([RUST_ANALYZER, "scip", ".", "--output", str(repo / "target/code-db/scip/root.scip")],
                   check=True, capture_output=True, cwd=repo)
    (repo / "target/code-db/cache").mkdir(parents=True)
    con = connect(repo, {"clone_reports": "[]::VARCHAR[]"})
    if print_only:
        for title, sql in FLOW_CHECKED:
            print_rows(con, title, checked_rows(con, sql))
        return
    for cached in (False, True):
        if cached:
            _, state, _ = refresh_against_live(con)
            assert state == "built", state
        for title, sql in FLOW_CHECKED:
            actual = checked_rows(con, sql)
            expected = FLOW_EXPECTED[title]
            assert actual == expected, f"flow {title}:\n  actual   {actual}\n  expected {expected}"


FLOW_EXPECTED = {
    'unit_complexity': [
        ('flow', 'module', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
        ('nested_loop', 'function', 3, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 4, 3),
        ('classify', 'function', 0, 0, 2, 6, 2, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 8, 2),
        ('chain', 'function', 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 2, 0),
        ('closures', 'function', 1, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 2, 1),
        ('outer', 'function', 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 0, 0, 0, 2, 1),
        ('inner', 'function', 1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 1, 2),
        ('with_macro', 'function', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
        ('while_let', 'function', 1, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 3, 2),
        ('picks', 'module', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
        ('picks.pick', 'function', 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1),
    ],
    'trees': [
        ('flow', 740, 1, 0, 8, 1),
        ('nested_loop', 98, 1, 0, 3, 98),
        ('classify', 132, 1, 0, 0, 132),
        ('chain', 90, 1, 0, 0, 90),
        ('closures', 133, 1, 0, 1, 133),
        ('outer', 102, 1, 0, 2, 60),
        ('inner', 43, 1, 0, 1, 43),
        ('with_macro', 57, 1, 0, 0, 57),
        ('while_let', 95, 1, 0, 1, 95),
        ('picks', 40, 1, 0, 1, 9),
        ('picks.pick', 32, 1, 0, 1, 32),
    ],
    'macro_tokens': [
        ('with_macro', 'if', 'token_tree', 59),
        ('with_macro', 'if', 'token_tree', 60),
        ('picks.pick', 'if_expression', 'expression_statement', 75),
        ('picks.pick', 'if', 'if_expression', 75),
    ],
    'query complexity': [
        ('src/lib.rs', 'flow', 'function', 'classify', 17, 11, 9, 8, 2, 0, 0, 2, 6, 2, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, False),
        ('src/lib.rs', 'flow', 'function', 'nested_loop', 1, 15, 5, 4, 3, 3, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, False),
        ('src/lib.rs', 'flow', 'function', 'while_let', 63, 9, 4, 3, 2, 1, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, False),
        ('src/lib.rs', 'flow', 'function', 'closures', 35, 11, 3, 2, 1, 1, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, False),
        ('src/lib.rs', 'flow', 'function', 'outer', 47, 10, 3, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 0, 0, 0, False),
        ('src/lib.rs', 'flow', 'function', 'chain', 29, 5, 3, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, False),
        ('src/lib.rs', 'flow', 'function', 'inner', 48, 3, 2, 1, 2, 1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, False),
        ('src/lib.rs', 'flow', 'function', 'picks.pick', 74, 3, 2, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('src/lib.rs', 'flow', 'function', 'with_macro', 58, 4, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
    ],
}


def assert_error(con, sql, text):
    try:
        con.execute(sql).fetchall()
    except duckdb.Error as e:
        assert text in str(e), (text, str(e))
        return
    raise AssertionError(f"no error containing {text!r}")


# The fixture's expected rows (CHECKED), each read and checked by hand against the
# fixture's text when it was written.
EXPECTED = {
    'files': [
        ('app/src/fixtures.rs', 2),
        ('app/src/main.rs', 2),
        ('app/tests/it.rs', 2),
        ('scripts/loose.rs', 0),
        ('shapes/src/convert.rs', 3),
        ('shapes/src/lib.rs', 15),
        ('shapes/src/rect.rs', 6),
        ('tools/src/main.rs', 3),
    ],
    'units': [
        ('app/src/fixtures.rs', 'module', 'Module', 'fixtures', None, 1, 4, 1, 3, True, 'private', None, False, True, None, None, None, None),
        ('app/src/fixtures.rs', 'function', 'Function', 'sample', 'fixtures', 1, 3, 1, 3, True, 'pub', 0, False, False, None, None, None, None),
        ('app/src/main.rs', 'module', 'Module', 'app', None, 1, 9, 1, 6, False, 'private', None, False, True, None, None, None, None),
        ('app/src/main.rs', 'function', 'Function', 'main', 'app', 3, 6, 3, 4, False, 'private', 0, False, False, None, None, None, None),
        ('app/tests/it.rs', 'module', 'Module', 'app', None, 1, 5, 1, 4, True, 'private', None, False, True, None, None, None, None),
        ('app/tests/it.rs', 'function', 'Function', 'integration', 'app', 1, 4, 2, 4, True, 'private', 0, False, False, None, None, None, None),
        ('shapes/src/convert.rs', 'module', 'Module', 'convert', None, 1, 13, 1, 10, False, 'pub', None, False, True, None, None, None, None),
        ('shapes/src/convert.rs', 'method', 'StaticMethod', 'From<Square> for Rect.from', 'convert', 5, 7, 5, 3, False, None, 1, None, False, 'trait_impl', 'Rect', 'From<Square>', 4),
        ('shapes/src/convert.rs', 'function', 'Function', 'back', 'convert', 10, 12, 10, 3, False, 'pub', 0, False, False, None, None, None, None),
        ('shapes/src/lib.rs', 'module', 'Module', 'shapes', None, 1, 68, 1, 56, False, 'private', None, True, True, None, None, None, None),
        ('shapes/src/lib.rs', 'type', 'Trait', 'Shape', 'shapes', 6, 10, 7, 5, False, 'pub', None, True, False, None, None, None, None),
        ('shapes/src/lib.rs', 'type', 'AssociatedType', 'Shape.Unit', 'Shape', 8, 8, 8, 1, False, None, None, False, False, 'trait', None, None, None),
        ('shapes/src/lib.rs', 'method', 'TraitMethod', 'Shape.area', 'Shape', 9, 9, 9, 1, False, None, 1, False, False, 'trait', None, None, None),
        ('shapes/src/lib.rs', 'constant', 'Constant', 'UNIT', 'shapes', 12, 12, 12, 1, False, 'pub', None, False, False, None, None, None, None),
        ('shapes/src/lib.rs', 'type', 'Struct', 'Square', 'shapes', 14, 17, 15, 4, False, 'pub', None, False, False, None, None, None, None),
        ('shapes/src/lib.rs', 'type', 'AssociatedType', 'Shape for Square.Unit', 'shapes', 20, 20, 20, 1, False, None, None, None, False, 'trait_impl', 'Square', 'Shape', 19),
        ('shapes/src/lib.rs', 'method', 'Method', 'Shape for Square.area', 'shapes', 21, 27, 21, 7, False, None, 1, None, False, 'trait_impl', 'Square', 'Shape', 19),
        ('shapes/src/lib.rs', 'method', 'StaticMethod', 'Square.new', 'shapes', 31, 34, 32, 4, False, 'pub', 1, True, False, 'impl', 'Square', None, 30),
        ('shapes/src/lib.rs', 'method', 'Method', 'Square.scaled', 'shapes', 37, 39, 37, 3, False, 'pub(crate)', 4, False, False, 'impl', 'Square', None, 30),
        ('shapes/src/lib.rs', 'function', 'Function', 'unused_helper', 'shapes', 42, 44, 42, 3, False, 'pub', 0, False, False, None, None, None, None),
        ('shapes/src/lib.rs', 'function', 'Function', 'marked', 'shapes', 46, 49, 47, 4, True, 'pub', 0, False, False, None, None, None, None),
        ('shapes/src/lib.rs', 'method', 'Method', 'Square.probe', 'shapes', 53, 55, 53, 3, True, 'private', 1, False, False, 'impl', 'Square', None, 52),
        ('shapes/src/lib.rs', 'module', 'Module', 'tests', 'shapes', 58, 67, 59, 9, True, 'private', None, False, False, None, None, None, None),
        ('shapes/src/lib.rs', 'function', 'Function', 'tests.square_area', 'tests', 62, 66, 63, 5, True, 'private', 0, False, False, None, None, None, None),
        ('shapes/src/rect.rs', 'module', 'Module', 'rect', None, 1, 25, 1, 20, False, 'pub', None, False, True, None, None, None, None),
        ('shapes/src/rect.rs', 'type', 'Struct', 'Rect', 'rect', 3, 5, 3, 3, False, 'pub', None, False, False, None, None, None, None),
        ('shapes/src/rect.rs', 'type', 'AssociatedType', 'Shape for Rect.Unit', 'rect', 8, 8, 8, 1, False, None, None, None, False, 'trait_impl', 'Rect', 'Shape', 7),
        ('shapes/src/rect.rs', 'method', 'Method', 'Shape for Rect.area', 'rect', 9, 15, 9, 7, False, None, 1, None, False, 'trait_impl', 'Rect', 'Shape', 7),
        ('shapes/src/rect.rs', 'function', 'Function', 'ring', 'rect', 18, 20, 18, 3, False, 'pub', 0, False, False, None, None, None, None),
        ('shapes/src/rect.rs', 'function', 'Function', 'base', 'rect', 22, 24, 22, 3, False, 'pub(crate)', 0, False, False, None, None, None, None),
        ('tools/src/main.rs', 'module', 'Module', 'tools', None, 1, 8, 1, 6, False, 'private', None, False, True, None, None, None, None),
        ('tools/src/main.rs', 'function', 'Function', 'main', 'tools', 1, 3, 1, 3, False, 'private', 0, False, False, None, None, None, None),
        ('tools/src/main.rs', 'function', 'Function', 'run', 'tools', 5, 7, 5, 3, False, 'private', 2, False, False, None, None, None, None),
    ],
    'impl_targets': [
        ('shapes/src/convert.rs', 4, 'Rect', 'From<Square>', 'trait', None, None),
        ('shapes/src/convert.rs', 4, 'Rect', 'From<Square>', 'type', 'Rect', 'header'),
        ('shapes/src/lib.rs', 19, 'Square', 'Shape', 'trait', 'Shape', 'header'),
        ('shapes/src/lib.rs', 19, 'Square', 'Shape', 'type', 'Square', 'header'),
        ('shapes/src/lib.rs', 30, 'Square', None, 'type', 'Square', 'header'),
        ('shapes/src/lib.rs', 52, 'Square', None, 'type', 'Square', 'header'),
        ('shapes/src/rect.rs', 7, 'Rect', 'Shape', 'trait', 'Shape', 'header'),
        ('shapes/src/rect.rs', 7, 'Rect', 'Shape', 'type', 'Rect', 'header'),
    ],
    'unit_edges': [
        ('app/src/fixtures.rs', 'sample', 'shapes/src/lib.rs', 'Square', 1, 2, False, False, True, False),
        ('app/src/fixtures.rs', 'sample', 'shapes/src/lib.rs', 'Square.new', 1, 1, False, False, True, False),
        ('app/src/main.rs', 'app', 'shapes/src/lib.rs', 'Shape', 1, 1, False, False, False, False),
        ('app/src/main.rs', 'app', 'shapes/src/lib.rs', 'Square', 1, 1, False, False, False, False),
        ('app/src/main.rs', 'main', 'shapes/src/lib.rs', 'Shape for Square.area', 1, 1, False, False, False, False),
        ('app/src/main.rs', 'main', 'shapes/src/lib.rs', 'Square', 1, 1, False, False, False, False),
        ('app/src/main.rs', 'main', 'shapes/src/lib.rs', 'Square.new', 1, 1, False, False, False, False),
        ('app/src/main.rs', 'main', 'shapes/src/lib.rs', 'UNIT', 1, 1, False, False, False, False),
        ('app/src/main.rs', 'main', 'shapes/src/rect.rs', 'ring', 1, 1, False, False, False, False),
        ('app/tests/it.rs', 'integration', 'shapes/src/lib.rs', 'Square', 2, 2, False, False, True, False),
        ('app/tests/it.rs', 'integration', 'shapes/src/lib.rs', 'Square.new', 1, 1, False, False, True, False),
        ('shapes/src/convert.rs', 'From<Square> for Rect.from', 'shapes/src/lib.rs', 'Square', 2, 2, False, False, False, False),
        ('shapes/src/convert.rs', 'From<Square> for Rect.from', 'shapes/src/rect.rs', 'Rect', 2, 2, False, False, False, False),
        ('shapes/src/convert.rs', 'back', 'shapes/src/rect.rs', 'base', 1, 1, False, False, False, False),
        ('shapes/src/convert.rs', 'convert', 'shapes/src/lib.rs', 'Square', 1, 2, False, False, False, False),
        ('shapes/src/convert.rs', 'convert', 'shapes/src/rect.rs', 'Rect', 1, 2, False, False, False, False),
        ('shapes/src/lib.rs', 'Shape for Square.area', 'shapes/src/lib.rs', 'Square', 1, 1, False, False, False, False),
        ('shapes/src/lib.rs', 'Shape for Square.area', 'shapes/src/lib.rs', 'UNIT', 1, 2, False, False, False, False),
        ('shapes/src/lib.rs', 'Square.new', 'shapes/src/lib.rs', 'Square', 1, 1, False, False, False, False),
        ('shapes/src/lib.rs', 'Square.probe', 'shapes/src/lib.rs', 'Square', 1, 1, False, False, True, False),
        ('shapes/src/lib.rs', 'Square.scaled', 'shapes/src/lib.rs', 'Square', 1, 1, False, False, False, False),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'Shape', 1, 1, False, False, False, False),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'Square', 1, 3, False, False, False, False),
        ('shapes/src/lib.rs', 'tests.square_area', 'shapes/src/lib.rs', 'Shape for Square.area', 1, 1, False, False, True, False),
        ('shapes/src/lib.rs', 'tests.square_area', 'shapes/src/lib.rs', 'Square', 1, 2, False, False, True, False),
        ('shapes/src/lib.rs', 'tests.square_area', 'shapes/src/lib.rs', 'Square.new', 1, 2, False, False, True, False),
        ('shapes/src/lib.rs', 'tests.square_area', 'shapes/src/lib.rs', 'Square.probe', 1, 1, False, False, True, True),
        ('shapes/src/rect.rs', 'Shape for Rect.area', 'shapes/src/lib.rs', 'UNIT', 1, 2, False, False, False, False),
        ('shapes/src/rect.rs', 'Shape for Rect.area', 'shapes/src/rect.rs', 'Rect', 1, 1, False, False, False, False),
        ('shapes/src/rect.rs', 'rect', 'shapes/src/lib.rs', 'Shape', 1, 2, False, False, False, False),
        ('shapes/src/rect.rs', 'rect', 'shapes/src/rect.rs', 'Rect', 1, 1, False, False, False, False),
        ('shapes/src/rect.rs', 'ring', 'shapes/src/convert.rs', 'back', 1, 1, False, False, False, False),
        ('tools/src/main.rs', 'main', 'tools/src/main.rs', 'run', 1, 1, False, False, False, False),
    ],
    'modules': [
        ('app/src/fixtures.rs', 'app', 'app/src/fixtures'),
        ('app/src/main.rs', 'app', 'app/src'),
        ('app/tests/it.rs', 'app', 'app/tests/it'),
        ('shapes/src/convert.rs', 'shapes', 'shapes/src/convert'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src'),
        ('shapes/src/lib.rs::tests', 'shapes', 'shapes/src/tests'),
        ('shapes/src/rect.rs', 'shapes', 'shapes/src/rect'),
        ('tools/src/main.rs', 'tools', 'tools/src'),
    ],
    'crate_edges': [
        ('app', 'shapes', 11, 13, 4, 0),
    ],
    'module_cycles': [
        ('shapes/src/convert.rs', 'shapes', 2, ['shapes/src/convert.rs', 'shapes/src/rect.rs'], 2),
    ],
    'unit_quality': [
        ('app/src/fixtures.rs', 'app', 'app/src/fixtures.rs', 'module', 'fixtures', 1, 4, True, 3, 0, 0, 0, 0, 0, 0, 0, 1, 1, None, None, 'private', None, False, 0, 1, 'referenced'),
        ('app/src/fixtures.rs', 'app', 'app/src/fixtures.rs', 'function', 'sample', 1, 3, True, 3, 0, 0, 0, 0, 0, 0, 0, 1, 1, None, None, 'pub', 0, False, 0, 0, 'test'),
        ('app/src/main.rs', 'app', 'app/src/main.rs', 'module', 'app', 1, 9, False, 6, 0, 0, 0, 0, 0, 0, 0, 2, 1, None, None, 'private', None, False, 0, 0, 'crate_root'),
        ('app/src/main.rs', 'app', 'app/src/main.rs', 'function', 'main', 3, 6, False, 4, 0, 0, 0, 0, 0, 0, 0, 2, 1, None, None, 'private', 0, False, 0, 0, 'entry_point'),
        ('app/tests/it.rs', 'app', 'app/tests/it.rs', 'module', 'app', 1, 5, True, 4, 0, 0, 0, 0, 0, 0, 0, 1, 1, None, None, 'private', None, False, 0, 0, 'test'),
        ('app/tests/it.rs', 'app', 'app/tests/it.rs', 'function', 'integration', 1, 4, True, 4, 0, 0, 0, 0, 0, 0, 0, 1, 1, None, None, 'private', 0, False, 0, 0, 'test'),
        ('shapes/src/convert.rs', 'shapes', 'shapes/src/convert.rs', 'module', 'convert', 1, 13, False, 10, 0, 0, 0, 0, 0, 1, 0, 2, 0, 'shapes/src/convert.rs', 'convert', 'pub', None, False, 0, 2, 'referenced'),
        ('shapes/src/convert.rs', 'shapes', 'shapes/src/convert.rs', 'method', 'From<Square> for Rect.from', 5, 7, False, 3, 0, 0, 0, 0, 0, 0, 0, 2, 0, 'shapes/src/convert.rs', 'convert', None, 1, None, 0, 0, 'trait_impl'),
        ('shapes/src/convert.rs', 'shapes', 'shapes/src/convert.rs', 'function', 'back', 10, 12, False, 3, 0, 0, 0, 0, 0, 1, 0, 1, 0, 'shapes/src/convert.rs', 'convert', 'pub', 0, False, 0, 1, 'referenced'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'module', 'shapes', 1, 68, False, 56, 0, 0, 0, 0, 0, 5, 1, 0, 0, None, None, 'private', None, True, 1, 16, 'referenced'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'type', 'Shape', 6, 10, False, 5, 0, 0, 0, 0, 0, 2, 1, 0, 0, None, None, 'pub', None, True, 0, 4, 'referenced'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'type', 'Shape.Unit', 8, 8, False, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, None, None, None, None, False, 0, 0, 'trait_item'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'method', 'Shape.area', 9, 9, False, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, None, None, None, 1, False, 0, 0, 'trait_item'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'constant', 'UNIT', 12, 12, False, 1, 0, 0, 0, 0, 0, 2, 1, 0, 0, None, None, 'pub', None, False, 0, 5, 'referenced'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'type', 'Square', 14, 17, False, 4, 0, 0, 0, 0, 0, 4, 1, 0, 0, None, None, 'pub', None, False, 0, 13, 'referenced'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'type', 'Shape for Square.Unit', 20, 20, False, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, None, None, None, None, None, 0, 0, 'trait_impl'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'method', 'Shape for Square.area', 21, 27, False, 7, 0, 0, 1, 1, 0, 1, 1, 0, 0, None, None, None, 1, None, 0, 2, 'referenced'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'method', 'Square.new', 31, 34, False, 4, 0, 0, 0, 0, 0, 3, 1, 0, 0, None, None, 'pub', 1, True, 0, 5, 'referenced'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'method', 'Square.scaled', 37, 39, False, 3, 0, 0, 1, 1, 0, 0, 0, 0, 0, None, None, 'pub(crate)', 4, False, 0, 0, 'unreferenced'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'function', 'unused_helper', 42, 44, False, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, None, None, 'pub', 0, False, 0, 0, 'unreferenced'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'function', 'marked', 46, 49, True, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, None, None, 'pub', 0, False, 0, 0, 'test'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs', 'method', 'Square.probe', 53, 55, True, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, None, None, 'private', 1, False, 0, 1, 'referenced'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs::tests', 'module', 'tests', 58, 67, True, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, None, None, 'private', None, False, 0, 0, 'test'),
        ('shapes/src/lib.rs', 'shapes', 'shapes/src/lib.rs::tests', 'function', 'tests.square_area', 62, 66, True, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, None, None, 'private', 0, False, 0, 0, 'test'),
        ('shapes/src/rect.rs', 'shapes', 'shapes/src/rect.rs', 'module', 'rect', 1, 25, False, 20, 0, 0, 0, 0, 0, 2, 1, 2, 0, 'shapes/src/convert.rs', 'rect', 'pub', None, False, 0, 4, 'referenced'),
        ('shapes/src/rect.rs', 'shapes', 'shapes/src/rect.rs', 'type', 'Rect', 3, 5, False, 3, 0, 0, 0, 0, 0, 1, 0, 1, 0, 'shapes/src/convert.rs', 'rect', 'pub', None, False, 0, 4, 'referenced'),
        ('shapes/src/rect.rs', 'shapes', 'shapes/src/rect.rs', 'type', 'Shape for Rect.Unit', 8, 8, False, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 'shapes/src/convert.rs', 'rect', None, None, None, 0, 0, 'trait_impl'),
        ('shapes/src/rect.rs', 'shapes', 'shapes/src/rect.rs', 'method', 'Shape for Rect.area', 9, 15, False, 7, 0, 0, 1, 1, 0, 0, 0, 1, 0, 'shapes/src/convert.rs', 'rect', None, 1, None, 0, 0, 'trait_impl'),
        ('shapes/src/rect.rs', 'shapes', 'shapes/src/rect.rs', 'function', 'ring', 18, 20, False, 3, 0, 0, 0, 0, 0, 1, 1, 1, 0, 'shapes/src/convert.rs', 'rect', 'pub', 0, False, 0, 1, 'referenced'),
        ('shapes/src/rect.rs', 'shapes', 'shapes/src/rect.rs', 'function', 'base', 22, 24, False, 3, 0, 0, 0, 0, 0, 1, 0, 0, 0, 'shapes/src/convert.rs', 'rect', 'pub(crate)', 0, False, 0, 1, 'referenced'),
        ('tools/src/main.rs', 'tools', 'tools/src/main.rs', 'module', 'tools', 1, 8, False, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, None, None, 'private', None, False, 0, 0, 'crate_root'),
        ('tools/src/main.rs', 'tools', 'tools/src/main.rs', 'function', 'main', 1, 3, False, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, None, None, 'private', 0, False, 0, 0, 'entry_point'),
        ('tools/src/main.rs', 'tools', 'tools/src/main.rs', 'function', 'run', 5, 7, False, 3, 0, 0, 1, 1, 0, 0, 0, 0, 0, None, None, 'private', 2, False, 0, 1, 'referenced'),
    ],
    'query size': [
        ('unit', 'shapes', 'shapes/src/lib.rs', 'method', 'Shape for Square.area', 21, 7, None, False, None),
        ('unit', 'shapes', 'shapes/src/rect.rs', 'method', 'Shape for Rect.area', 9, 7, None, False, None),
        ('unit', 'shapes', 'shapes/src/lib.rs', 'type', 'Shape', 6, 5, None, False, None),
        ('unit', 'app', 'app/src/main.rs', 'function', 'main', 3, 4, None, False, None),
        ('unit', 'shapes', 'shapes/src/lib.rs', 'type', 'Square', 14, 4, None, False, None),
        ('unit', 'shapes', 'shapes/src/lib.rs', 'method', 'Square.new', 31, 4, None, False, None),
        ('unit', 'shapes', 'shapes/src/convert.rs', 'method', 'From<Square> for Rect.from', 5, 3, None, False, None),
        ('unit', 'shapes', 'shapes/src/convert.rs', 'function', 'back', 10, 3, None, False, None),
        ('unit', 'shapes', 'shapes/src/lib.rs', 'method', 'Square.scaled', 37, 3, None, False, None),
        ('unit', 'shapes', 'shapes/src/lib.rs', 'function', 'unused_helper', 42, 3, None, False, None),
        ('unit', 'shapes', 'shapes/src/rect.rs', 'type', 'Rect', 3, 3, None, False, None),
        ('unit', 'shapes', 'shapes/src/rect.rs', 'function', 'ring', 18, 3, None, False, None),
        ('unit', 'shapes', 'shapes/src/rect.rs', 'function', 'base', 22, 3, None, False, None),
        ('unit', 'tools', 'tools/src/main.rs', 'function', 'main', 1, 3, None, False, None),
        ('unit', 'tools', 'tools/src/main.rs', 'function', 'run', 5, 3, None, False, None),
        ('unit', 'shapes', 'shapes/src/lib.rs', 'type', 'Shape.Unit', 8, 1, None, False, None),
        ('unit', 'shapes', 'shapes/src/lib.rs', 'method', 'Shape.area', 9, 1, None, False, None),
        ('unit', 'shapes', 'shapes/src/lib.rs', 'constant', 'UNIT', 12, 1, None, False, None),
        ('unit', 'shapes', 'shapes/src/lib.rs', 'type', 'Shape for Square.Unit', 20, 1, None, False, None),
        ('unit', 'shapes', 'shapes/src/rect.rs', 'type', 'Shape for Rect.Unit', 8, 1, None, False, None),
    ],
    'query duplication': [
        (0, 'exact', 5, 80, 'shapes/src/lib.rs', 22, 26, 'method', 'Shape for Square.area', False, 'shapes/src/rect.rs', 10, 14, 'method', 'Shape for Rect.area', False, False, True),
        (1, 'exact', 1, 10, 'shapes/src/lib.rs', 38, 38, 'method', 'Square.scaled', False, 'tools/src/main.rs', 6, 6, 'function', 'run', False, False, False),
    ],
    'query coupling': [
        ('shapes/src/lib.rs', 'shapes', 'module', 'shapes', 1, 56, 5, 1, 8, 0, 0, 0, False),
        ('shapes/src/lib.rs', 'shapes', 'type', 'Square', 14, 4, 4, 1, 8, 0, 0, 1, False),
        ('shapes/src/lib.rs', 'shapes', 'method', 'Square.new', 31, 4, 3, 1, 4, 0, 0, 1, False),
        ('shapes/src/rect.rs', 'shapes', 'module', 'rect', 1, 20, 2, 1, 4, 2, 0, 3, False),
        ('shapes/src/lib.rs', 'shapes', 'type', 'Shape', 6, 5, 2, 1, 3, 0, 0, 0, False),
        ('shapes/src/lib.rs', 'shapes', 'constant', 'UNIT', 12, 1, 2, 1, 3, 0, 0, 0, False),
        ('shapes/src/rect.rs', 'shapes', 'function', 'ring', 18, 3, 1, 1, 1, 1, 0, 1, False),
        ('shapes/src/lib.rs', 'shapes', 'method', 'Shape for Square.area', 21, 7, 1, 1, 2, 0, 0, 2, False),
        ('shapes/src/convert.rs', 'shapes', 'module', 'convert', 1, 10, 1, 0, 1, 2, 0, 3, False),
        ('shapes/src/convert.rs', 'shapes', 'function', 'back', 10, 3, 1, 0, 1, 1, 0, 1, False),
        ('shapes/src/rect.rs', 'shapes', 'type', 'Rect', 3, 3, 1, 0, 2, 1, 0, 2, False),
        ('shapes/src/rect.rs', 'shapes', 'function', 'base', 22, 3, 1, 0, 1, 0, 0, 0, False),
        ('app/src/main.rs', 'app', 'module', 'app', 1, 6, 0, 0, 0, 2, 1, 6, False),
        ('app/src/main.rs', 'app', 'function', 'main', 3, 4, 0, 0, 0, 2, 1, 5, False),
        ('shapes/src/convert.rs', 'shapes', 'method', 'From<Square> for Rect.from', 5, 3, 0, 0, 0, 2, 0, 2, False),
        ('shapes/src/rect.rs', 'shapes', 'method', 'Shape for Rect.area', 9, 7, 0, 0, 0, 1, 0, 2, False),
        ('shapes/src/lib.rs', 'shapes', 'method', 'Square.scaled', 37, 3, 0, 0, 0, 0, 0, 1, False),
        ('tools/src/main.rs', 'tools', 'function', 'main', 1, 3, 0, 0, 0, 0, 0, 1, False),
        ('tools/src/main.rs', 'tools', 'function', 'run', 5, 3, 0, 0, 1, 0, 0, 0, False),
    ],
    'query interface': [
        ('shapes/src/lib.rs', 'shapes', 'method', 'Square.scaled', 37, 4, 'pub(crate)', 0, 3, 'pub(crate) fn scaled(&self, a: u32, b: u32, c: u32) -> u32'),
        ('tools/src/main.rs', 'tools', 'function', 'run', 5, 2, 'private', 0, 3, 'fn run(a: u32, b: u32) -> u32'),
        ('shapes/src/lib.rs', 'shapes', 'method', 'Shape for Square.area', 21, 1, None, 1, 7, 'fn area(&self) -> u32'),
        ('shapes/src/lib.rs', 'shapes', 'method', 'Square.new', 31, 1, 'pub', 1, 4, 'pub fn new(side: u32) -> Self'),
        ('shapes/src/convert.rs', 'shapes', 'method', 'From<Square> for Rect.from', 5, 1, None, 0, 3, 'fn from(square: Square) -> Self'),
        ('shapes/src/lib.rs', 'shapes', 'method', 'Shape.area', 9, 1, None, 0, 1, 'pub fn area(&self) -> u32'),
        ('shapes/src/rect.rs', 'shapes', 'method', 'Shape for Rect.area', 9, 1, None, 0, 7, 'fn area(&self) -> u32'),
        ('shapes/src/rect.rs', 'shapes', 'function', 'ring', 18, 0, 'pub', 1, 3, 'pub fn ring() -> u32'),
        ('app/src/main.rs', 'app', 'function', 'main', 3, 0, 'private', 0, 4, 'fn main()'),
        ('shapes/src/convert.rs', 'shapes', 'function', 'back', 10, 0, 'pub', 0, 3, 'pub fn back() -> u32'),
        ('shapes/src/lib.rs', 'shapes', 'function', 'unused_helper', 42, 0, 'pub', 0, 3, 'pub fn unused_helper() -> u32'),
        ('shapes/src/rect.rs', 'shapes', 'function', 'base', 22, 0, 'pub(crate)', 0, 3, 'pub(crate) fn base() -> u32'),
        ('tools/src/main.rs', 'tools', 'function', 'main', 1, 0, 'private', 0, 3, 'fn main()'),
    ],
    'query public_surface': [
        ('shapes', 8, 6, 3, 1),
        ('app', 0, 0, 0, 0),
        ('tools', 0, 0, 0, 0),
    ],
    'query intent': [
        ('shapes/src/lib.rs', 'shapes', 'module', 'shapes', 1, 'private', True, 1, 1, 5),
        ('shapes/src/lib.rs', 'shapes', 'type', 'Square', 14, 'pub', False, 0, 1, 4),
        ('shapes/src/lib.rs', 'shapes', 'constant', 'UNIT', 12, 'pub', False, 0, 1, 2),
        ('shapes/src/rect.rs', 'shapes', 'module', 'rect', 1, 'pub', False, 0, 1, 2),
        ('shapes/src/rect.rs', 'shapes', 'function', 'ring', 18, 'pub', False, 0, 1, 1),
    ],
    'query unreferenced': [
        ('shapes/src/lib.rs', 'shapes', 'method', 'Square.scaled', 37, 3, 'pub(crate)', 'unreferenced'),
        ('shapes/src/lib.rs', 'shapes', 'function', 'unused_helper', 42, 3, 'pub', 'unreferenced'),
    ],
    'query module_graph': [
        ('app/src/main.rs', 'shapes/src/lib.rs', 'app', 'shapes', 6, 6, 0, 0, False, 0),
        ('shapes/src/lib.rs::tests', 'shapes/src/lib.rs', 'shapes', 'shapes', 4, 6, 4, 0, True, 0),
        ('shapes/src/convert.rs', 'shapes/src/rect.rs', 'shapes', 'shapes', 3, 5, 0, 0, False, 1),
        ('shapes/src/convert.rs', 'shapes/src/lib.rs', 'shapes', 'shapes', 2, 4, 0, 0, True, 0),
        ('shapes/src/rect.rs', 'shapes/src/lib.rs', 'shapes', 'shapes', 2, 4, 0, 0, True, 0),
        ('app/src/fixtures.rs', 'shapes/src/lib.rs', 'app', 'shapes', 2, 3, 2, 0, False, 0),
        ('app/tests/it.rs', 'shapes/src/lib.rs', 'app', 'shapes', 2, 3, 2, 0, False, 0),
        ('app/src/main.rs', 'shapes/src/rect.rs', 'app', 'shapes', 1, 1, 0, 0, False, 0),
        ('shapes/src/rect.rs', 'shapes/src/convert.rs', 'shapes', 'shapes', 1, 1, 0, 0, False, 3),
    ],
    'query module_cycles': [
        ('shapes/src/convert.rs', 'shapes', 2, 'shapes/src/rect.rs', 'shapes/src/convert.rs', 1, 1, 0, 0, 3, ['back'], 1),
        ('shapes/src/convert.rs', 'shapes', 2, 'shapes/src/convert.rs', 'shapes/src/rect.rs', 3, 5, 0, 0, 1, ['Rect', 'base', 'side'], 1),
    ],
    'query layer_violations': [
        ('rect', 'convert', 'shapes/src/rect.rs', 'shapes/src/convert.rs', 1, 1, 0, 0, False, 'shapes/src/convert.rs'),
    ],
    'query unit_dependencies': [
        ('out', 1, 'shapes/src/convert.rs', 'function', 'back', 10, 3, 1, 1, 1, ['back'], 1),
        ('in', 1, 'app/src/main.rs', 'function', 'main', 3, 4, 1, 1, 1, ['ring'], 0),
    ],
    'query complexity': [
        ('shapes/src/lib.rs', 'shapes', 'method', 'Shape for Square.area', 21, 7, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('shapes/src/rect.rs', 'shapes', 'method', 'Shape for Rect.area', 9, 7, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('app/src/main.rs', 'app', 'function', 'main', 3, 4, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('shapes/src/lib.rs', 'shapes', 'method', 'Square.new', 31, 4, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('shapes/src/convert.rs', 'shapes', 'method', 'From<Square> for Rect.from', 5, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('shapes/src/convert.rs', 'shapes', 'function', 'back', 10, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('shapes/src/lib.rs', 'shapes', 'method', 'Square.scaled', 37, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('shapes/src/lib.rs', 'shapes', 'function', 'unused_helper', 42, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('shapes/src/rect.rs', 'shapes', 'function', 'ring', 18, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('shapes/src/rect.rs', 'shapes', 'function', 'base', 22, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('tools/src/main.rs', 'tools', 'function', 'main', 1, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('tools/src/main.rs', 'tools', 'function', 'run', 5, 3, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
        ('shapes/src/lib.rs', 'shapes', 'method', 'Shape.area', 9, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, False),
    ],
    'query size level=file': [
        ('file', 'shapes', 'shapes/src/lib.rs', None, None, None, 56, 14, False, None),
        ('file', 'shapes', 'shapes/src/rect.rs', None, None, None, 20, 5, False, None),
        ('file', 'shapes', 'shapes/src/convert.rs', None, None, None, 10, 2, False, None),
        ('file', 'app', 'app/src/main.rs', None, None, None, 6, 1, False, None),
        ('file', 'tools', 'tools/src/main.rs', None, None, None, 6, 2, False, None),
    ],
    'query size level=crate': [
        ('crate', 'shapes', None, None, None, None, 86, 21, None, 18.6),
        ('crate', 'app', None, None, None, None, 13, 3, None, 53.8),
        ('crate', 'tools', None, None, None, None, 6, 2, None, 0.0),
    ],
    'query coupling kind=type min_crates=1': [
        ('shapes/src/lib.rs', 'shapes', 'type', 'Square', 14, 4, 4, 1, 8, 0, 0, 1, True),
        ('shapes/src/lib.rs', 'shapes', 'type', 'Shape', 6, 5, 2, 1, 3, 0, 0, 0, True),
        ('shapes/src/rect.rs', 'shapes', 'type', 'Rect', 3, 3, 1, 0, 2, 1, 0, 2, False),
    ],
    'query unreferenced include_structural': [
        ('shapes/src/lib.rs', 'shapes', 'method', 'Square.scaled', 37, 3, 'pub(crate)', 'unreferenced'),
        ('shapes/src/lib.rs', 'shapes', 'function', 'unused_helper', 42, 3, 'pub', 'unreferenced'),
        ('shapes/src/lib.rs', 'shapes', 'module', 'tests', 58, 9, 'private', 'test'),
        ('shapes/src/rect.rs', 'shapes', 'method', 'Shape for Rect.area', 9, 7, None, 'trait_impl'),
        ('app/src/main.rs', 'app', 'module', 'app', 1, 6, 'private', 'crate_root'),
        ('tools/src/main.rs', 'tools', 'module', 'tools', 1, 6, 'private', 'crate_root'),
        ('shapes/src/lib.rs', 'shapes', 'function', 'tests.square_area', 62, 5, 'private', 'test'),
        ('app/src/main.rs', 'app', 'function', 'main', 3, 4, 'private', 'entry_point'),
        ('app/tests/it.rs', 'app', 'module', 'app', 1, 4, 'private', 'test'),
        ('app/tests/it.rs', 'app', 'function', 'integration', 1, 4, 'private', 'test'),
        ('shapes/src/lib.rs', 'shapes', 'function', 'marked', 46, 4, 'pub', 'test'),
        ('app/src/fixtures.rs', 'app', 'function', 'sample', 1, 3, 'pub', 'test'),
        ('shapes/src/convert.rs', 'shapes', 'method', 'From<Square> for Rect.from', 5, 3, None, 'trait_impl'),
        ('tools/src/main.rs', 'tools', 'function', 'main', 1, 3, 'private', 'entry_point'),
        ('shapes/src/lib.rs', 'shapes', 'type', 'Shape.Unit', 8, 1, None, 'trait_item'),
        ('shapes/src/lib.rs', 'shapes', 'method', 'Shape.area', 9, 1, None, 'trait_item'),
        ('shapes/src/lib.rs', 'shapes', 'type', 'Shape for Square.Unit', 20, 1, None, 'trait_impl'),
        ('shapes/src/rect.rs', 'shapes', 'type', 'Shape for Rect.Unit', 8, 1, None, 'trait_impl'),
    ],
    'query duplication include_tests source=exact': [
        (0, 'exact', 5, 80, 'shapes/src/lib.rs', 22, 26, 'method', 'Shape for Square.area', False, 'shapes/src/rect.rs', 10, 14, 'method', 'Shape for Rect.area', False, False, True),
        (1, 'exact', 1, 10, 'shapes/src/lib.rs', 38, 38, 'method', 'Square.scaled', False, 'tools/src/main.rs', 6, 6, 'function', 'run', False, False, False),
    ],
}


def assert_fixture(con, repo):
    for title, sql in CHECKED:
        actual = checked_rows(con, sql)
        expected = EXPECTED[title]
        assert actual == expected, f"{title}:\n  actual   {actual}\n  expected {expected}"
    # Without layers, no violation and no violating edge.
    patterns = rows(con, "SELECT * FROM config_layers")
    deps = rows(con, "SELECT * FROM config_layer_deps")
    set_layers(con, [], [])
    assert rows(con, query_sql("layer_violations")) == []
    assert {r[-1] for r in rows(con, query_sql("module_cycles"))} == {0}
    # A module in two layers is an error.
    set_layers(con, [("a", "shapes/src/rect.rs"), ("b", "shapes/src/*")], [])
    assert_error(con, "SELECT * FROM module_layers", "module_layers: shapes/src/rect.rs matches the layers a, b")
    set_layers(con, patterns, deps)
    # unit_dependencies stops without its unit.
    con.execute("RESET VARIABLE path")
    assert_error(con, query_sql("unit_dependencies"), "set the variables path, unit_kind and qualified_name")
    con.execute("SET VARIABLE path = 'shapes/src/rect.rs'")


# --- repository commands ------------------------------------------------------------


def check_repo(repo):
    con = connect(repo)
    print(rows(con, "SELECT kind, count(*), count(*) FILTER (WHERE is_test) FROM units GROUP BY kind ORDER BY kind"))
    set_dependency_unit(con)
    for q in QUERIES:
        start = time.perf_counter()
        result = rows(con, query_sql(q))
        print(f"{q}: {len(result)} rows, {time.perf_counter() - start:.2f} s")
        for r in result[:3]:
            print("  ", r)


def check_refresh_repo(repo):
    con = connect(repo)
    wall, state, cache_file = refresh_against_live(con)
    print(f"refresh: {state} {cache_file} in {wall:.2f} s; every cached layer and query equals live")


def run_step(repo, step, mode):
    """One timed step in this process: the view files, refresh.sql when mode is not
    'live', then the query `step`. Prints a JSON line."""
    start = time.perf_counter()
    con = connect(repo)
    loaded = time.perf_counter()
    refresh = None
    if mode != "live":
        refresh, _, _ = read_refresh(con)
    set_dependency_unit(con)
    q_start = time.perf_counter()
    n = len(rows(con, "SELECT * FROM unit_quality" if step == "unit_quality" else query_sql(step)))
    end = time.perf_counter()
    print(json.dumps({"load": loaded - start, "refresh": refresh, "query": end - q_start, "rows": n,
                      "rss_mb": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024}))


def measure(repo, runs):
    cache = Path(repo).resolve() / "target/code-db/cache"

    def series(step, mode):
        results = []
        for _ in range(runs):
            if mode == "build":
                shutil.rmtree(cache, ignore_errors=True)
                cache.mkdir(parents=True)
            proc = subprocess.run([sys.executable, __file__, "step", repo, step, "--mode", mode],
                                  capture_output=True, text=True, check=True)
            results.append(json.loads(proc.stdout.strip().splitlines()[-1]))
        return results

    def span(results, key):
        values = [r[key] for r in results if r[key] is not None]
        return f"{min(values):.2f}-{max(values):.2f}" if values else "-"

    for step in [*QUERIES, "unit_quality"]:
        for mode in ("live", "build", "kept"):
            results = series(step, mode)
            print(f"{step:18} {mode:5} load {span(results, 'load')} s, refresh {span(results, 'refresh')} s, "
                  f"query {span(results, 'query')} s, rows {results[0]['rows']}, peak RSS {span(results, 'rss_mb')} MB")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["fixture", "repo", "refresh", "measure", "step"])
    parser.add_argument("repo", nargs="?")
    parser.add_argument("step", nargs="?")
    parser.add_argument("--print", action="store_true", help="fixture: print the rows instead of asserting")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--mode", default="live", help=argparse.SUPPRESS)
    parser.add_argument("--set", action="append", default=[], metavar="NAME=EXPR",
                        help="set a DuckDB variable (EXPR is SQL, e.g. memory_limit=\"'4GB'\")")
    args = parser.parse_args()
    for assignment in args.set:
        name, sep, value = assignment.partition("=")
        if not sep or not name.isidentifier():
            parser.error(f"--set expects NAME=EXPR, got {assignment}")
        SET_VARIABLES[name] = value
    if args.command == "fixture":
        check_fixture(args.print)
        return
    if not args.repo:
        parser.error("REPO is required")
    if args.command == "repo":
        check_repo(args.repo)
    elif args.command == "refresh":
        check_refresh_repo(args.repo)
    elif args.command == "measure":
        measure(args.repo, args.runs)
    else:
        run_step(args.repo, args.step, args.mode)


if __name__ == "__main__":
    main()
