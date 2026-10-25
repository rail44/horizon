# /// script
# requires-python = ">=3.12"
# dependencies = ["duckdb==2.0.0"]
# ///
"""Execute the code database SQL files in order and run every query.

    uv run --script duckdb-ext/sql/check.py fixture
        Build a small git repository in a temporary directory (Rust files with a
        trait impl, a test module, an untracked file, excluded build and hidden
        directories, several commits) plus a jscpd-shaped report, and assert on the
        rows of every view and query, live, through cache.sql and through refresh.sql
        (edits, additions, deletions, renames, commits, an amend, untracked files, a
        new session); a history fixture asserts blob_files, blob_units and every row
        of unit_changes, unit_churn and clone_change_consistency; a SCIP fixture
        (a rust-analyzer index of the fixture crate; skipped without rust-analyzer)
        asserts views/scip.sql and unit_fan_in_churn, live and cached.

    uv run --script duckdb-ext/sql/check.py scip-fixture
        Only the SCIP fixture.

    The commands below read views/scip.sql too when REPO/target/code-db/index.scip
    exists, and then include the SCIP views and unit_fan_in_churn.

    uv run --script duckdb-ext/sql/check.py refresh REPO [--reports ...]
        Read refresh.sql twice (cache REPO/target/code-db/cache.duckdb) and compare
        every refreshed view and query with its live counterpart.

    uv run --script duckdb-ext/sql/check.py measure-refresh CLONE [--runs 3]
        Time refresh.sql in a fresh process per run: empty cache, nothing changed,
        1 and 10 files edited, one commit of 3 files, an amend; then the queries from
        the cache, the cache file size and the file snapshot. Edits, commits to and
        resets CLONE: pass a throwaway clone.

    uv run --script duckdb-ext/sql/check.py repo REPO [--reports R1 R2 ...]
        Load REPO (reports default to REPO/target/code-db/jscpd/{exact,normalized}),
        print the counts by unit kind and tag, and run every query.

    uv run --script duckdb-ext/sql/check.py measure REPO [--runs 3] [--reports ...]
        Time each statement in a fresh process per run, live and with cache.sql read
        first, and the cache build; print min-max wall time and peak RSS (see
        docs/research/code-db-views-single-scan-2026-10-24.md).

    uv run --script duckdb-ext/sql/check.py explain REPO [--reports ...]
        Count the read_ast/read_text scans in each statement's plan.

    uv run --script duckdb-ext/sql/check.py cli REPO [--reports ...]
        Run the same files with `.read` in the DuckDB 2.0.0 CLI (DUCKDB_CLI, default
        ~/.local/bin/duckdb2), then refresh.sql, and compare its counts with this
        client's.

The process works from the root of the Horizon checkout holding this file, because
00_load.sql loads git_history by a path relative to it.
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
    "views/nodes.sql",
    "views/blobs.sql",
    "lang/rust.sql",
    "views/units.sql",
    "views/history.sql",
    "views/unit_changes.sql",
    "views/clones.sql",
]
CACHE_FILE = "cache.sql"
REFRESH_FILE = "refresh.sql"
# The views refresh.sql points at the persistent cache, plus files (its file snapshot).
REFRESHED_VIEWS = ["files", "nodes", "units", "tags", "commits", "file_changes", "hunks",
                   "blob_files", "blob_nodes", "blob_units", "unit_changes"]
QUERIES = ["file_cochange", "file_cochange_thresholds", "clone_site_tags", "clone_site_tags_summary",
           "unit_churn", "clone_change_consistency"]
# views/scip.sql is optional: read after VIEW_FILES when a SCIP index is given. Its views
# and queries join the lists above once it has been read.
SCIP_FILE = "views/scip.sql"
SCIP_VIEWS = ["scip_occurrences", "scip_symbols"]
SCIP_QUERIES = ["unit_fan_in_churn"]
RUST_ANALYZER = os.environ.get("RUST_ANALYZER", "rust-analyzer")


def sql_literal(value):
    if isinstance(value, list):
        return "[" + ", ".join(sql_literal(v) for v in value) + "]"
    return "'" + str(value).replace("'", "''") + "'"


def scip_loaded(con):
    """Whether views/scip.sql has been read on this connection."""
    return rows(con, "SELECT count(*) FROM duckdb_views() "
                     "WHERE view_name = 'scip_index_occurrences' AND NOT internal")[0][0] > 0


def read_views(con, scip=None):
    """Read the view files, and views/scip.sql when `scip` (None: when it was read before)."""
    if scip is None:
        scip = scip_loaded(con)
    for name in VIEW_FILES:
        con.execute((SQL_DIR / name).read_text())
    if scip:
        con.execute((SQL_DIR / SCIP_FILE).read_text())


def queries(con):
    return QUERIES + (SCIP_QUERIES if scip_loaded(con) else [])


def refreshed_views(con):
    return REFRESHED_VIEWS + (SCIP_VIEWS if scip_loaded(con) else [])


def repo_index(repo):
    """REPO's SCIP index at the default scip_index path, or None when there is none."""
    index = Path(repo).resolve() / "target/code-db/index.scip"
    return index if index.exists() else None


def read_cache(con):
    con.execute((SQL_DIR / CACHE_FILE).read_text())


def connect(repo, reports=None, variables=None, scip=False):
    os.chdir(HORIZON_ROOT)
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"SET VARIABLE repo = {sql_literal(str(Path(repo).resolve()))}")
    if reports:
        con.execute(f"SET VARIABLE clone_reports = {sql_literal([str(Path(r).resolve()) for r in reports])}")
    for name, value in (variables or {}).items():
        con.execute(f"SET VARIABLE {name} = {value}")
    read_views(con, scip)
    return con


def assert_no_source_scan(con):
    """The cached views answer the queries that read syntax without reading a source."""
    for name in ["clone_site_tags", "unit_churn", "clone_change_consistency"]:
        plan = "\n".join(r[1] for r in rows(con, "EXPLAIN " + query_sql(name)))
        assert "READ_AST" not in plan and "READ_TEXT" not in plan, (name, plan)


def query_sql(name):
    return (SQL_DIR / "queries" / f"{name}.sql").read_text()


def rows(con, sql):
    return con.execute(sql).fetchall()


def read_refresh(con):
    """Read refresh.sql; return its wall time and the cache's meta row as a dict."""
    start = time.perf_counter()
    con.execute((SQL_DIR / REFRESH_FILE).read_text())
    wall = time.perf_counter() - start
    cursor = con.execute("SELECT * FROM cache.meta")
    names = [d[0] for d in cursor.description]
    return wall, dict(zip(names, cursor.fetchone()))


# Views compared through a digest rather than row by row: blob_nodes has about 31 million
# rows on this repository, and a copy of them plus the comparison peaked above 30 GB.
DIGESTED_VIEWS = {"blob_nodes": "SELECT blob_id, count(*) AS n, sum(hash(v)) AS h FROM blob_nodes v GROUP BY blob_id"}


def view_rows(view):
    return DIGESTED_VIEWS.get(view, f"SELECT * FROM {view}")


def snapshot_live(con):
    """Copy every view refresh.sql replaces, read live, into temp tables live_<view>
    (blob_nodes as one digest row per blob)."""
    for view in refreshed_views(con):
        con.execute(f"CREATE OR REPLACE TEMP TABLE live_{view} AS {view_rows(view)}")


def diff_from_live(con):
    """{view: (live rows missing from the view, view rows missing from live)}, nonzero only."""
    diffs = {}
    for view in refreshed_views(con):
        counts = rows(con, f"SELECT (SELECT count(*) FROM (FROM live_{view} EXCEPT ALL ({view_rows(view)}))), "
                           f"(SELECT count(*) FROM (({view_rows(view)}) EXCEPT ALL FROM live_{view}))")[0]
        if counts != (0, 0):
            diffs[view] = counts
    return diffs


def refresh_against_live(con):
    """Re-read the view files (live, with a new file snapshot), copy the live rows and
    the four queries' results, read refresh.sql and compare. Returns (wall, meta)."""
    read_views(con)
    snapshot_live(con)
    live_queries = [rows(con, query_sql(q)) for q in queries(con)]
    wall, meta = read_refresh(con)
    diffs = diff_from_live(con)
    assert not diffs, f"refreshed views differ from live: {diffs}"
    assert [rows(con, query_sql(q)) for q in queries(con)] == live_queries, "queries differ after refresh"
    return wall, meta


# --- fixture ---------------------------------------------------------------------

SHAPES_V1 = """\
pub trait Shape {
    fn area(&self) -> u32;
}

pub struct Square {
    side: u32,
}

impl Shape for Square {
    fn area(&self) -> u32 {
        let side = self.side;
        let doubled = side * 2;
        let half = doubled / 2;
        half * side
    }
}

impl Square {
    pub fn new(side: u32) -> Self {
        Self { side }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_area() {
        assert_eq!(Square::new(2).area(), 4);
    }
}
"""

RECT = """\
use crate::shapes::Shape;

pub struct Rect {
    side: u32,
}

impl Shape for Rect {
    fn area(&self) -> u32 {
        let side = self.side;
        let doubled = side * 2;
        let half = doubled / 2;
        half * side
    }
}
"""

LIB_V1 = """\
pub mod rect;
pub mod shapes;

pub fn area(w: u32, h: u32) -> u32 {
    w * h
}
"""

IT = """\
#[test]
fn integration() {
    assert_eq!(fixture::area(2, 3), 6);
}
"""


def git_env(repo, date=None, author="Fixture"):
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(repo),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_AUTHOR_NAME": author,
        "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
        "GIT_COMMITTER_NAME": "Fixture",
        "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
    }
    if date:
        env["GIT_AUTHOR_DATE"] = env["GIT_COMMITTER_DATE"] = date
    return env


def git(repo, *args, date=None, author="Fixture"):
    subprocess.run(["git", *args], cwd=repo, env=git_env(repo, date, author), check=True, capture_output=True)


def git_out(repo, *args):
    return subprocess.run(["git", *args], cwd=repo, env=git_env(repo), check=True,
                          capture_output=True, text=True).stdout.strip()


def write(repo, path, text):
    target = repo / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text)


def line_of(text, needle, start=1):
    """1-based number of the first line at or after `start` containing `needle`."""
    for number, line in enumerate(text.splitlines(), 1):
        if number >= start and needle in line:
            return number
    raise AssertionError(f"{needle!r} not found")


def end_of_block(text, start):
    """1-based number of the line closing the brace block opened on line `start`."""
    lines = text.splitlines()
    indent = len(lines[start - 1]) - len(lines[start - 1].lstrip())
    for number in range(start + 1, len(lines) + 1):
        line = lines[number - 1]
        if line.strip() == "}" and len(line) - len(line.lstrip()) == indent:
            return number
    raise AssertionError(f"no block end after line {start}")


def build_fixture(root):
    repo = root / "repo"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    write(repo, "src/lib.rs", LIB_V1)
    write(repo, "src/shapes.rs", SHAPES_V1)
    write(repo, "src/rect.rs", RECT)
    write(repo, "README.md", "fixture\n")
    write(repo, ".gitignore", "target/\n.hidden/\n")
    git(repo, "add", ".")
    git(repo, "commit", "-q", "-m", "Add shapes", date="2026-01-01T00:00:00Z")
    # Two more commits touching lib.rs and shapes.rs together, one touching README only.
    lib = LIB_V1.replace("w * h", "h * w")
    write(repo, "src/lib.rs", lib)
    write(repo, "src/shapes.rs", SHAPES_V1.replace("Self { side }", "Square { side }"))
    git(repo, "commit", "-q", "-am", "Swap operands", date="2026-01-02T00:00:00Z")
    write(repo, "src/lib.rs", lib + "\npub fn perimeter(w: u32, h: u32) -> u32 {\n    2 * (w + h)\n}\n")
    write(repo, "src/shapes.rs", SHAPES_V1)
    write(repo, "tests/it.rs", IT)
    git(repo, "add", ".")
    git(repo, "commit", "-q", "-m", "Add perimeter and an integration test", date="2026-01-03T00:00:00Z")
    write(repo, "README.md", "fixture\nmore\n")
    git(repo, "commit", "-q", "-am", "Extend README", date="2026-01-04T00:00:00Z")
    # Not committed: an untracked source file (included with include_untracked), a
    # build output and a file in a hidden directory (both ignored by .gitignore).
    write(repo, "src/scratch.rs", "fn scratch() {}\n")
    write(repo, "target/debug/build/out.rs", "fn generated() {}\n")
    write(repo, ".hidden/x.rs", "fn hidden() {}\n")
    return repo


def fixture_report(root, repo):
    """A report in jscpd 5.3.1's JSON shape (--absolute) with one pair per construct."""
    shapes = (repo / "src/shapes.rs").read_text()
    rect = (repo / "src/rect.rs").read_text()
    sa = line_of(shapes, "fn area(&self) -> u32 {")
    ra = line_of(rect, "fn area(&self) -> u32 {")
    st = line_of(shapes, "fn square_area()")

    def side(path, start, end):
        return {"name": str(repo / path), "start": start, "end": end,
                "startLoc": {"line": start, "column": 1, "position": 0},
                "endLoc": {"line": end, "column": 1, "position": 0}}

    duplicates = [
        # inside both trait impls
        {"format": "rust", "lines": 5, "tokens": 40, "fragment": "", "isNew": False, "kind": "exact",
         "firstFile": side("src/shapes.rs", sa, sa + 4), "secondFile": side("src/rect.rs", ra, ra + 4)},
        # a test function against a range starting before rect.rs's impl (contained by
        # no tagged node, overlapping the impl)
        {"format": "rust", "lines": 6, "tokens": 30, "fragment": "", "isNew": False, "kind": "exact",
         "firstFile": side("src/shapes.rs", st, st + 2), "secondFile": side("src/rect.rs", 4, 9)},
    ]
    path = root / "jscpd" / "exact" / "jscpd-report.json"
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps({"duplicates": duplicates, "statistics": {}}))
    return path, {"sa": sa, "ra": ra, "st": st}


def check_fixture():
    with tempfile.TemporaryDirectory(prefix="code-db-fixture-") as tmp:
        root = Path(tmp)
        repo = build_fixture(root)
        report, lines = fixture_report(root, repo)
        con = connect(repo, [report], {"min_shared": "2", "include_untracked": "true"})
        shapes = (repo / "src/shapes.rs").read_text()

        files = rows(con, "SELECT path, language, mode, length(blob_id) FROM files ORDER BY path")
        assert files == [(p, "rust", "100644", 40) for p in
                         ["src/lib.rs", "src/rect.rs", "src/scratch.rs", "src/shapes.rs", "tests/it.rs"]], files
        blob = rows(con, "SELECT blob_id FROM files WHERE path = 'src/shapes.rs'")[0][0]
        assert blob == git_out(repo, "rev-parse", "HEAD:src/shapes.rs"), blob

        node_files = rows(con, "SELECT count(DISTINCT file_id), min(start_line), bool_and(file_id IS NOT NULL) FROM nodes")
        assert node_files == [(5, 1, True)], node_files

        units = rows(con, """
            SELECT u.kind, u.qualified_name, u.start_line, u.end_line, u.loc, p.qualified_name
            FROM units u JOIN files f USING (file_id)
            LEFT JOIN units p ON p.unit_id = u.parent_unit_id
            WHERE f.path = 'src/shapes.rs' ORDER BY u.unit_id""")
        impl_shape = line_of(shapes, "impl Shape for Square")
        impl_square = line_of(shapes, "impl Square")
        mod_tests = line_of(shapes, "mod tests")
        new = line_of(shapes, "pub fn new")
        sq = lines["st"]
        expected = [
            ("type", "Shape", 1, 3, 3, None),
            ("method", "Shape.area", 2, 2, 1, "Shape"),
            ("type", "Square", 5, 7, 3, None),
            ("impl", "Shape for Square", impl_shape, end_of_block(shapes, impl_shape), 8, None),
            ("method", "Shape for Square.area", lines["sa"], end_of_block(shapes, lines["sa"]), 6, "Shape for Square"),
            ("impl", "Square", impl_square, end_of_block(shapes, impl_square), 5, None),
            ("method", "Square.new", new, new + 2, 3, "Square"),
            ("module", "tests", mod_tests, end_of_block(shapes, mod_tests), 7, None),
            ("function", "tests.square_area", sq, sq + 2, 3, "tests"),
        ]
        assert units == expected, units

        tags = rows(con, """
            SELECT f.path, n.type, n.start_line, t.tag
            FROM tags t JOIN nodes n USING (file_id, node_id) JOIN files f USING (file_id)
            ORDER BY f.path, n.start_line, t.tag, n.type""")
        assert tags == [
            ("src/rect.rs", "impl_item", 7, "trait_impl"),
            ("src/shapes.rs", "impl_item", impl_shape, "trait_impl"),
            ("src/shapes.rs", "mod_item", mod_tests, "test"),
            ("src/shapes.rs", "function_item", sq, "test"),
            ("src/shapes.rs", "macro_invocation", sq + 1, "generated_or_macro"),
            ("tests/it.rs", "source_file", 1, "test"),
            ("tests/it.rs", "function_item", 2, "test"),
            ("tests/it.rs", "macro_invocation", 3, "generated_or_macro"),
        ], tags

        assert rows(con, "SELECT count(*), count(*) FILTER (WHERE is_merge) FROM commits") == [(4, 0)]
        changes = rows(con, "SELECT path, count(*) FROM file_changes GROUP BY 1 ORDER BY 1")
        assert changes == [(".gitignore", 1), ("README.md", 2), ("src/lib.rs", 3), ("src/rect.rs", 1),
                           ("src/shapes.rs", 3), ("tests/it.rs", 1)], changes
        hunk_paths = rows(con, "SELECT DISTINCT path FROM hunks ORDER BY 1")
        assert hunk_paths == [("src/lib.rs",), ("src/rect.rs",), ("src/shapes.rs",), ("tests/it.rs",)], hunk_paths

        pairs = rows(con, "SELECT * FROM clone_pairs ORDER BY pair_id")
        assert pairs == [
            (0, "exact", "src/shapes.rs", lines["sa"], lines["sa"] + 4, "src/rect.rs", lines["ra"], lines["ra"] + 4, 40, 5),
            (1, "exact", "src/shapes.rs", sq, sq + 2, "src/rect.rs", 4, 9, 30, 6),
        ], pairs

        cochange = rows(con, query_sql("file_cochange"))
        assert cochange == [("src/lib.rs", "src/shapes.rs", 3, 3, 3, 100.0, 100.0)], cochange
        thresholds = rows(con, query_sql("file_cochange_thresholds"))
        assert thresholds == [(t, 1, 1) for t in (10, 20, 30, 40, 50)], thresholds

        sites = rows(con, query_sql("clone_site_tags"))
        assert [(r[0], r[4], r[7]) for r in sites] == [(0, "trait_impl", "trait_impl"), (1, "test", "none")], sites
        summary = rows(con, query_sql("clone_site_tags_summary"))
        assert summary == [("exact", "none / test", 1, 50.0, 6),
                           ("exact", "trait_impl / trait_impl", 1, 50.0, 5)], summary
        con.execute("SET VARIABLE match_mode = 'overlap'")
        sites = rows(con, query_sql("clone_site_tags"))
        assert [(r[0], r[4], r[7]) for r in sites] == [(0, "trait_impl", "trait_impl"),
                                                        (1, "generated_or_macro+test", "trait_impl")], sites
        con.execute("SET VARIABLE clone_source = 'normalized'")
        assert rows(con, query_sql("clone_site_tags")) == []

        # Variables are read at scan time: a narrower history changes the views.
        first = rows(con, "SELECT sha FROM commits ORDER BY author_time LIMIT 1")[0][0]
        con.execute(f"SET VARIABLE since = '{first}'")
        assert rows(con, "SELECT count(*) FROM commits") == [(3,)]
        # Contents are live: an edit shows up without reloading anything.
        write(repo, "src/scratch.rs", "fn scratch() {}\nfn more() {}\n")
        scratch_units = ("SELECT count(*) FROM units u JOIN files f USING (file_id) "
                         "WHERE f.path = 'src/scratch.rs'")
        assert rows(con, scratch_units) == [(2,)]

        # cache.sql: the same rows from the copies, frozen until the view files are read.
        con.execute("SET VARIABLE match_mode = 'contain'")
        con.execute("SET VARIABLE clone_source = NULL")
        probes = ["SELECT * FROM nodes ORDER BY file_id, node_id", "SELECT * FROM units ORDER BY unit_id",
                  "SELECT * FROM tags ORDER BY ALL", "SELECT * FROM commits ORDER BY ALL",
                  "SELECT * FROM file_changes ORDER BY ALL", "SELECT * FROM hunks ORDER BY ALL",
                  *(query_sql(q) for q in QUERIES)]
        live = [rows(con, q) for q in probes]
        read_cache(con)
        assert [rows(con, q) for q in probes] == live
        assert_no_source_scan(con)
        write(repo, "src/scratch.rs", "fn scratch() {}\nfn more() {}\nfn third() {}\n")
        con.execute("SET VARIABLE since = NULL")
        assert rows(con, scratch_units) == [(2,)]
        assert rows(con, "SELECT count(*) FROM commits") == [(3,)]
        # Reading cache.sql again without the view files in between fails instead of
        # copying the copies.
        try:
            read_cache(con)
        except duckdb.CatalogException:
            pass
        else:
            raise AssertionError("cache.sql read twice in a row did not fail")
        read_views(con)
        assert rows(con, scratch_units) == [(3,)]
        assert rows(con, "SELECT count(*) FROM commits") == [(4,)]
        # Refresh: the view files, then cache.sql.
        read_cache(con)
        assert rows(con, scratch_units) == [(3,)]
        write(repo, "src/scratch.rs", "fn scratch() {}\n")
        assert rows(con, scratch_units) == [(3,)]
    print("fixture: ok")
    check_history_fixture()
    check_scip_fixture()
    check_refresh_fixture()


def build_history_fixture(root):
    """build_fixture plus commits for unit_changes: one clone side edited, then both (by
    a second author), a unit added, a rename with a unit replaced, a unit removed, and a
    pure rename. Returns the repository and the commit shas by step name."""
    repo = build_fixture(root)
    sha = {}

    def commit(name, message, date, author="Fixture"):
        git(repo, "add", "-A", "src/lib.rs", "src/shapes.rs", "src/rect.rs", "tests")
        git(repo, "commit", "-q", "-m", message, date=date, author=author)
        sha[name] = git_out(repo, "rev-parse", "HEAD")

    sha["readme"] = git_out(repo, "rev-parse", "HEAD")
    shapes = SHAPES_V1.replace("let half = doubled / 2;", "let half = doubled >> 1;")
    write(repo, "src/shapes.rs", shapes)
    commit("one_side", "Edit one clone side", "2026-02-01T00:00:00Z")
    write(repo, "src/shapes.rs", shapes.replace("half * side", "side * half"))
    write(repo, "src/rect.rs", RECT.replace("half * side", "side * half"))
    commit("both_sides", "Edit both clone sides", "2026-02-02T00:00:00Z", author="Second")
    lib = (repo / "src/lib.rs").read_text()
    write(repo, "src/lib.rs", lib + "\npub fn volume(w: u32, h: u32, d: u32) -> u32 { w * h * d }\n")
    commit("added", "Add a unit", "2026-02-03T00:00:00Z")
    (repo / "tests/it.rs").unlink()
    write(repo, "tests/integration.rs", IT.replace("fn integration()", "fn integration_area()"))
    commit("renamed_edit", "Rename the integration test and its function", "2026-02-04T00:00:00Z")
    write(repo, "src/lib.rs", lib)
    commit("removed", "Remove a unit", "2026-02-05T00:00:00Z")
    git(repo, "mv", "tests/integration.rs", "tests/area.rs")
    commit("pure_rename", "Rename only", "2026-02-06T00:00:00Z")
    sha["initial"] = git_out(repo, "rev-list", "--max-parents=0", "HEAD")
    sha["swap"], sha["perimeter"] = git_out(repo, "rev-list", "--reverse", "HEAD").splitlines()[1:3]
    return repo, sha


# unit_changes of build_history_fixture, by step: (path, kind, qualified_name, change).
HISTORY_CHANGES = {
    "initial": [("src/lib.rs", "function", "area", "added"), ("src/lib.rs", "module", "rect", "added"),
                ("src/lib.rs", "module", "shapes", "added"),
                ("src/rect.rs", "impl", "Shape for Rect", "added"),
                ("src/rect.rs", "method", "Shape for Rect.area", "added"), ("src/rect.rs", "type", "Rect", "added"),
                ("src/shapes.rs", "function", "tests.square_area", "added"),
                ("src/shapes.rs", "impl", "Shape for Square", "added"), ("src/shapes.rs", "impl", "Square", "added"),
                ("src/shapes.rs", "method", "Shape for Square.area", "added"),
                ("src/shapes.rs", "method", "Shape.area", "added"), ("src/shapes.rs", "method", "Square.new", "added"),
                ("src/shapes.rs", "module", "tests", "added"), ("src/shapes.rs", "type", "Shape", "added"),
                ("src/shapes.rs", "type", "Square", "added")],
    "swap": [("src/lib.rs", "function", "area", "modified"), ("src/shapes.rs", "impl", "Square", "modified"),
             ("src/shapes.rs", "method", "Square.new", "modified")],
    # The appended function's -U0 insertion could also be placed one line earlier
    # (it ends with `}` like the line before it); gix places it after area's `}`.
    "perimeter": [("src/lib.rs", "function", "perimeter", "added"), ("src/shapes.rs", "impl", "Square", "modified"),
                  ("src/shapes.rs", "method", "Square.new", "modified"),
                  ("tests/it.rs", "function", "integration", "added")],
    "readme": [],
    "one_side": [("src/shapes.rs", "impl", "Shape for Square", "modified"),
                 ("src/shapes.rs", "method", "Shape for Square.area", "modified")],
    "both_sides": [("src/rect.rs", "impl", "Shape for Rect", "modified"),
                   ("src/rect.rs", "method", "Shape for Rect.area", "modified"),
                   ("src/shapes.rs", "impl", "Shape for Square", "modified"),
                   ("src/shapes.rs", "method", "Shape for Square.area", "modified")],
    "added": [("src/lib.rs", "function", "volume", "added")],
    # A rename with an edit: the new unit under the new path, the old one under the old path.
    "renamed_edit": [("tests/integration.rs", "function", "integration_area", "added"),
                     ("tests/it.rs", "function", "integration", "removed")],
    "removed": [("src/lib.rs", "function", "volume", "removed")],
    # A pure rename has no hunks: no rows.
    "pure_rename": [],
}

# The `current` units of each file, with ids replaced by positions as in blob_units.
CURRENT_UNITS_BY_BLOB = """
    SELECT f.blob_id, u.unit_id - min(u.unit_id) OVER (PARTITION BY u.file_id) AS unit_ord,
           u.parent_unit_id - min(u.unit_id) OVER (PARTITION BY u.file_id) AS parent_unit_ord,
           u.node_id, u.kind, u.name, u.qualified_name, u.start_line, u.end_line, u.loc
    FROM units u JOIN files f USING (file_id)"""


def blob_units_equal_units(con):
    """(current files whose blob has units, files with units, rows only in units, rows only
    in blob_units) over the current files' blobs."""
    current = CURRENT_UNITS_BY_BLOB
    return rows(con, f"""
        SELECT (SELECT count(DISTINCT blob_id) FROM ({current}) WHERE blob_id IN (SELECT blob_id FROM blob_units)),
               (SELECT count(DISTINCT blob_id) FROM ({current})),
               (SELECT count(*) FROM ({current} EXCEPT ALL
                                      SELECT * FROM blob_units WHERE blob_id IN (SELECT blob_id FROM files))),
               (SELECT count(*) FROM (SELECT * FROM blob_units WHERE blob_id IN (SELECT blob_id FROM files)
                                      EXCEPT ALL {current}))""")[0]


def check_history_fixture():
    with tempfile.TemporaryDirectory(prefix="code-db-history-") as tmp:
        root = Path(tmp)
        repo, sha = build_history_fixture(root)
        report, _ = fixture_report(root, repo)
        con = connect(repo, [report])
        assert sorted(sha) == sorted(HISTORY_CHANGES), sha

        # blob_files: one row per side with a blob; no old side for an addition, no row
        # for the pure rename (the same content on both sides).
        sides = rows(con, "SELECT side, path, old_path, count(*) FROM blob_files GROUP BY ALL ORDER BY ALL")
        assert sides == [
            ("new", "src/lib.rs", None, 5), ("new", "src/rect.rs", None, 2), ("new", "src/shapes.rs", None, 5),
            ("new", "tests/integration.rs", "tests/it.rs", 1), ("new", "tests/it.rs", None, 1),
            ("old", "src/lib.rs", None, 4), ("old", "src/rect.rs", None, 1), ("old", "src/shapes.rs", None, 4),
            ("old", "tests/integration.rs", "tests/it.rs", 1),
        ], sides
        assert rows(con, "SELECT bool_and(file = getvariable('blob_dir') || '/' || blob_id || '.rs') FROM blob_files") == [(True,)]
        # One parse per distinct blob.
        blobs = rows(con, "SELECT count(DISTINCT blob_id) FROM blob_files")[0][0]
        assert rows(con, "SELECT count(DISTINCT file_path), count(DISTINCT blob_id) FROM blob_nodes") == [(blobs, blobs)]

        # The units of every current file's blob equal its units, modulo ids.
        files = rows(con, "SELECT count(*) FROM files")[0][0]
        assert blob_units_equal_units(con) == (files, files, 0, 0), blob_units_equal_units(con)

        names = {v: k for k, v in sha.items()}
        got = {}
        for r in rows(con, "SELECT sha, path, unit_kind, unit_qualified_name, change_kind FROM unit_changes"):
            got.setdefault(names[r[0]], []).append(r[1:])
        expected = {k: sorted(v) for k, v in HISTORY_CHANGES.items() if v}
        assert {k: sorted(v) for k, v in got.items()} == expected, got
        assert rows(con, "SELECT count(*) = count(DISTINCT (sha, path, unit_kind, unit_qualified_name, change_kind)) "
                         "FROM unit_changes") == [(True,)]

        # unit_churn: every current unit; history under a previous path is not followed.
        churn = {(r[0], r[2], r[3]): r[6:8] for r in rows(con, query_sql("unit_churn"))}
        assert len(churn) == rows(con, "SELECT count(*) FROM units")[0][0], churn
        assert churn[("src/shapes.rs", "method", "Shape for Square.area")] == (3, 2), churn
        assert churn[("src/shapes.rs", "impl", "Shape for Square")] == (3, 2), churn
        assert churn[("src/rect.rs", "method", "Shape for Rect.area")] == (2, 2), churn
        assert churn[("src/shapes.rs", "method", "Square.new")] == (3, 1), churn
        assert churn[("src/lib.rs", "function", "area")] == (2, 1), churn
        assert churn[("src/lib.rs", "function", "perimeter")] == (1, 1), churn
        assert churn[("tests/area.rs", "function", "integration_area")] == (0, 0), churn
        con.execute("SET VARIABLE since_time = TIMESTAMP '2026-02-01 00:00:00'")
        con.execute("SET VARIABLE kind = 'method'")
        churn = [(r[0], r[3], r[6]) for r in rows(con, query_sql("unit_churn")) if r[6]]
        assert churn == [("src/shapes.rs", "Shape for Square.area", 2), ("src/rect.rs", "Shape for Rect.area", 1)], churn
        con.execute("SET VARIABLE since_time = NULL")
        con.execute("SET VARIABLE kind = NULL")

        # clone_change_consistency: pair 0 changed together twice (initial, both_sides) and
        # on side A alone once (one_side); pair 1's side B lies in no single unit.
        consistency = [(r[0], r[5], r[9], r[12], r[13:]) for r in rows(con, query_sql("clone_change_consistency"))]
        assert consistency == [(0, "Shape for Square.area", "Shape for Rect.area", False, (1, 0, 2, 1)),
                               (1, "tests.square_area", None, None, (1, 0, 0, 1))], consistency
        con.execute("SET VARIABLE clone_source = 'normalized'")
        assert rows(con, query_sql("clone_change_consistency")) == []
        con.execute("SET VARIABLE clone_source = NULL")

        # `since` narrows blob_files and hunks together (views/blobs.sql re-read for its
        # snapshot).
        con.execute(f"SET VARIABLE since = '{sha['readme']}'")
        read_views(con)
        steps = sorted({names[r[0]] for r in rows(con, "SELECT sha FROM unit_changes")})
        assert steps == ["added", "both_sides", "one_side", "removed", "renamed_edit"], steps
        assert rows(con, "SELECT count(DISTINCT sha) FROM blob_files") == [(5,)]
        con.close()
    print("history fixture: ok")


FIXTURE_CARGO_TOML = """\
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"

[workspace]
"""


def make_scip_index(repo, root):
    """Index the fixture's crate with rust-analyzer into REPO/target/code-db/index.scip
    (the default scip_index; target/ is ignored in the fixture): a copy of src/ and tests/
    plus a Cargo.toml for the package `fixture`, outside the repository. Returns the
    index, or None when rust-analyzer is not installed."""
    if shutil.which(RUST_ANALYZER) is None:
        return None
    crate = root / "scip-crate"
    shutil.rmtree(crate, ignore_errors=True)
    for d in ("src", "tests"):
        shutil.copytree(repo / d, crate / d)
    (crate / "Cargo.toml").write_text(FIXTURE_CARGO_TOML)
    index = repo / "target/code-db/index.scip"
    index.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run([RUST_ANALYZER, "scip", str(crate), "--output", str(index)], check=True,
                   capture_output=True, cwd=root)
    return index


def assert_no_scip_scan(con):
    """The cached views answer the SCIP query without reading the index."""
    plan = "\n".join(r[1] for r in rows(con, "EXPLAIN " + query_sql("unit_fan_in_churn")))
    assert "READ_JSON" not in plan and "READ_SCIP" not in plan, plan


def check_scip_fixture():
    """views/scip.sql and unit_fan_in_churn over a real rust-analyzer index of the first
    fixture, live and through cache.sql; skipped when rust-analyzer is not installed."""
    with tempfile.TemporaryDirectory(prefix="code-db-scip-") as tmp:
        root = Path(tmp)
        repo = build_fixture(root)
        report, _ = fixture_report(root, repo)
        if make_scip_index(repo, root) is None:
            print(f"scip fixture: SKIPPED ({RUST_ANALYZER} not found)")
            return
        con = connect(repo, [report], scip=True)
        shapes = (repo / "src/shapes.rs").read_text()
        rect = (repo / "src/rect.rs").read_text()
        lib = (repo / "src/lib.rs").read_text()

        meta = rows(con, "SELECT tool_name, document_count, occurrence_count, external_symbol_count "
                         "FROM scip_index_metadata")
        assert meta == [("rust-analyzer", 4, rows(con, "SELECT count(*) FROM scip_index_occurrences")[0][0], 0)], meta
        # Every document is a source file (src/scratch.rs is in no crate's module tree).
        docs = rows(con, "SELECT DISTINCT o.document, o.file_id = f.file_id FROM scip_occurrences o "
                         "LEFT JOIN files f ON f.path = o.document ORDER BY 1")
        assert docs == [(p, True) for p in ["src/lib.rs", "src/rect.rs", "src/shapes.rs", "tests/it.rs"]], docs
        assert rows(con, "SELECT bool_and(is_reference = NOT is_definition) FROM scip_occurrences") == [(True,)]

        # Lines 1-based; columns 1-based with the end after the last character. The trait's
        # definition encloses exactly its unit's lines.
        shape = "'rust-analyzer cargo fixture 0.1.0 shapes/Shape#'"
        trait_line = line_of(shapes, "pub trait Shape")
        col = shapes.splitlines()[trait_line - 1].index("Shape") + 1
        defs = rows(con, f"SELECT document, start_line, start_column, end_line, end_column, enclosing_start_line, "
                         f"enclosing_end_line FROM scip_occurrences WHERE symbol = {shape} AND is_definition")
        assert defs == [("src/shapes.rs", trait_line, col, trait_line, col + 5, 1, 3)], defs
        refs = rows(con, f"SELECT document, start_line FROM scip_occurrences WHERE symbol = {shape} AND is_reference "
                         "ORDER BY ALL")
        assert refs == [("src/rect.rs", line_of(rect, "use crate::shapes::Shape")),
                        ("src/rect.rs", line_of(rect, "impl Shape for Rect")),
                        ("src/shapes.rs", line_of(shapes, "impl Shape for Square"))], refs

        # Local symbols are qualified by their document, in occurrences and symbols.
        assert rows(con, "SELECT count(*) FROM scip_occurrences WHERE starts_with(symbol, 'local ') "
                         "AND NOT ends_with(symbol, ' ' || document)") == [(0,)]
        assert rows(con, "SELECT list(DISTINCT symbol ORDER BY symbol) FROM scip_occurrences "
                         "WHERE symbol LIKE 'local 0 %'") == [(["local 0 src/lib.rs", "local 0 src/rect.rs",
                                                              "local 0 src/shapes.rs"],)]
        local = rows(con, "SELECT display_name, enclosing_symbol FROM scip_symbols WHERE symbol = 'local 0 src/rect.rs'")
        assert local == [("self", "rust-analyzer cargo fixture 0.1.0 rect/impl#[Rect][Shape]area().")], local
        # The package's crate/ symbol is listed under each target's root.
        crate = rows(con, "SELECT list(document ORDER BY document) FROM scip_symbols "
                          "WHERE symbol = 'rust-analyzer cargo fixture 0.1.0 crate/'")
        assert crate == [(["src/lib.rs", "tests/it.rs"],)], crate

        # unit_fan_in_churn: lib.rs's area is called from tests/it.rs, the trait is named
        # twice in rect.rs; the whole-file definitions of the modules lie in no unit.
        fan_in = [("src/lib.rs", "rust", "function", "area", line_of(lib, "pub fn area"), 3, 1, 1, 2),
                  ("src/shapes.rs", "rust", "type", "Shape", trait_line, 3, 1, 2, 1)]
        assert rows(con, query_sql("unit_fan_in_churn")) == fan_in, rows(con, query_sql("unit_fan_in_churn"))
        con.execute("SET VARIABLE kind = 'type'")
        assert rows(con, query_sql("unit_fan_in_churn")) == fan_in[1:]
        con.execute("SET VARIABLE kind = NULL")
        con.execute("SET VARIABLE min_fan_in = 2")
        assert rows(con, query_sql("unit_fan_in_churn")) == []
        con.execute("SET VARIABLE min_fan_in = NULL")

        # cache.sql: the same rows from the copies, and the query reads no index.
        probes = ["SELECT * FROM scip_occurrences ORDER BY ALL", "SELECT * FROM scip_symbols ORDER BY ALL",
                  query_sql("unit_fan_in_churn")]
        live = [rows(con, q) for q in probes]
        read_cache(con)
        assert [rows(con, q) for q in probes] == live
        assert_no_scip_scan(con)

        # A document that is not a source file has no file_id, and its references count
        # for nothing (tests/it.rs excluded from the snapshot).
        con.execute("SET VARIABLE exclude_patterns = ['tests/*']")
        read_views(con)
        assert rows(con, "SELECT count(*), count(file_id) FROM scip_occurrences WHERE document = 'tests/it.rs'")[0][1] == 0
        assert rows(con, query_sql("unit_fan_in_churn")) == fan_in[1:]
        con.close()

        # Without views/scip.sql: cache.sql still covers the SCIP views, empty.
        con = connect(repo, [report])
        assert not scip_loaded(con)
        read_cache(con)
        assert rows(con, "SELECT (SELECT count(*) FROM scip_occurrences), (SELECT count(*) FROM scip_symbols), "
                         "(SELECT count(*) FROM scip_occurrences_cache)") == [(0, 0, 0)]
        con.close()
    print("scip fixture: ok")


def cached_rows_by_path(con, path_filter):
    """Rows of the cache's per-file tables for the paths matching path_filter (SQL)."""
    return {t: sorted(rows(con, f"SELECT * FROM cache.{t} WHERE {path_filter}"), key=repr)
            for t in ["nodes", "units", "tags"]}


def check_refresh_fixture():
    with tempfile.TemporaryDirectory(prefix="code-db-refresh-") as tmp:
        root = Path(tmp)
        repo = build_fixture(root)
        report, _ = fixture_report(root, repo)
        cache = root / "cache" / "cache.duckdb"
        cache.parent.mkdir()
        variables = {"min_shared": "2", "cache_path": sql_literal(str(cache))}
        con = connect(repo, [report], variables)
        counts = ("files_added", "files_changed", "files_removed", "nodes_parsed", "units_inserted",
                  "tags_inserted", "commits_added", "blob_rows_added", "blobs_parsed", "blobs_derived")

        def stats(meta):
            return {k: meta[k] for k in counts}

        def expect(meta, syntax="incremental", history="appended", scip="kept", **nonzero):
            assert meta["scip_cache"] == scip, meta
            expected = {k: nonzero.get(k, 0) for k in counts}
            got = stats(meta)
            for k, v in nonzero.items():
                if v is None:  # nonzero, value not asserted
                    assert got[k] > 0, (k, got)
                    expected[k] = got[k]
            assert (meta["syntax"], meta["history"], got) == (syntax, history, expected), meta
            assert meta["state"] == "ok" and meta["derived"] == syntax, meta
            blob_files = "rebuilt" if "rebuilt" in (syntax, history) else "appended"
            assert meta["blob_history"] == blob_files, meta
            assert meta["blob_nodes_parsed"] > 0 if meta["blobs_parsed"] else meta["blob_nodes_parsed"] == 0, meta
            assert (meta["blob_bytes_parsed"] > 0) == (meta["blobs_parsed"] > 0), meta

        # Initial refresh from an empty cache: everything built, equal to live.
        _, meta = refresh_against_live(con)
        expect(meta, syntax="rebuilt", history="rebuilt", scip="rebuilt", files_added=4, nodes_parsed=None,
               units_inserted=None, tags_inserted=None, commits_added=4, blob_rows_added=12, blobs_parsed=7,
               blobs_derived=7)
        # Without views/scip.sql the SCIP tables are empty, and so are the two views.
        assert meta["scip_key"] is None and meta["scip_occurrences_inserted"] == 0, meta
        assert rows(con, "SELECT (SELECT count(*) FROM scip_occurrences), (SELECT count(*) FROM scip_symbols)") == [(0, 0)]
        assert meta["head"] == git_out(repo, "rev-parse", "HEAD"), meta
        assert meta["blobs_cached"] == 7, meta
        assert rows(con, "SELECT count(*) FROM cache.nodes_files") == [(4,)]
        assert_no_source_scan(con)

        # Nothing changed: no file parsed, nothing appended; reading refresh.sql again in
        # the same session takes under 500 ms (the best of three reads, as the desktop
        # this runs on is shared).
        _, meta = refresh_against_live(con)
        expect(meta)
        walls = []
        for _ in range(3):
            wall, meta = read_refresh(con)
            expect(meta)
            walls.append(wall)
        assert min(walls) < 0.5, f"refresh with nothing changed took {walls} s: {meta['seconds']}"
        print("refresh with nothing changed:", ", ".join(f"{w * 1000:.0f} ms" for w in walls))
        assert meta["nodes_parsed"] == 0 and rows(con, "SELECT count(*) FROM cache.nodes_files") == [(4,)]

        # Edit one file: exactly its rows are replaced.
        others = "path <> 'src/rect.rs'"
        before = cached_rows_by_path(con, others)
        old_rect = cached_rows_by_path(con, "path = 'src/rect.rs'")
        write(repo, "src/rect.rs", RECT + "\n#[test]\nfn rect_area() {\n    assert!(true);\n}\n")
        _, meta = refresh_against_live(con)
        rect_nodes = rows(con, "SELECT count(*) FROM cache.nodes WHERE path = 'src/rect.rs'")[0][0]
        rect_units = rows(con, "SELECT count(*) FROM cache.units WHERE path = 'src/rect.rs'")[0][0]
        rect_tags = rows(con, "SELECT count(*) FROM cache.tags WHERE path = 'src/rect.rs'")[0][0]
        expect(meta, files_changed=1, nodes_parsed=rect_nodes, units_inserted=rect_units, tags_inserted=rect_tags)
        assert cached_rows_by_path(con, others) == before
        assert cached_rows_by_path(con, "path = 'src/rect.rs'") != old_rect

        # Add a file (tracked, not committed).
        write(repo, "src/extra.rs", "pub fn extra() -> u32 {\n    1\n}\n")
        git(repo, "add", "src/extra.rs")
        before = cached_rows_by_path(con, "path <> 'src/extra.rs'")
        _, meta = refresh_against_live(con)
        expect(meta, files_added=1, nodes_parsed=None, units_inserted=1)
        assert cached_rows_by_path(con, "path <> 'src/extra.rs'") == before

        # Delete it again.
        git(repo, "rm", "-q", "-f", "src/extra.rs")
        _, meta = refresh_against_live(con)
        expect(meta, files_removed=1)
        assert rows(con, "SELECT count(*) FROM cache.nodes WHERE path = 'src/extra.rs'") == [(0,)]

        # Rename a file: the old path is dropped, the new one parsed (paths are keys).
        git(repo, "mv", "tests/it.rs", "tests/integration.rs")
        _, meta = refresh_against_live(con)
        expect(meta, files_added=1, files_removed=1, nodes_parsed=None, units_inserted=1, tags_inserted=3)

        # A new commit (of the edit and the rename, already in the cache): history appended.
        git(repo, "commit", "-q", "-am", "Edit rect, rename the integration test", date="2026-01-05T00:00:00Z")
        _, meta = refresh_against_live(con)
        expect(meta, commits_added=1, blob_rows_added=2, blobs_parsed=1, blobs_derived=1)
        assert meta["head"] == git_out(repo, "rev-parse", "HEAD"), meta
        assert meta["blobs_cached"] == 8, meta
        assert rows(con, "SELECT count(*) FROM commits") == [(5,)]

        # An amended commit: the cached head is gone from HEAD's history, so it is rebuilt.
        git(repo, "commit", "-q", "--amend", "-m", "Amended", date="2026-01-06T00:00:00Z")
        _, meta = refresh_against_live(con)
        expect(meta, history="rebuilt", commits_added=5, blob_rows_added=14)
        assert meta["blobs_cached"] == 8, meta
        assert rows(con, "SELECT list(subject ORDER BY subject) FILTER (WHERE subject LIKE 'Amended' "
                         "OR subject LIKE 'Edit rect%') FROM commits") == [(["Amended"],)]

        # Untracked files count only with include_untracked (scratch.rs is untracked).
        _, meta = refresh_against_live(con)
        expect(meta)
        con.execute("SET VARIABLE include_untracked = true")
        _, meta = refresh_against_live(con)
        expect(meta, files_added=1, nodes_parsed=None, units_inserted=1)
        assert rows(con, "SELECT count(*) FROM files WHERE path = 'src/scratch.rs'") == [(1,)]
        con.execute("SET VARIABLE include_untracked = false")
        _, meta = refresh_against_live(con)
        expect(meta, files_removed=1)

        # An interrupted refresh (state left at 'refreshing') makes the next one rebuild.
        con.execute("UPDATE cache.meta SET state = 'refreshing'")
        _, meta = refresh_against_live(con)
        expect(meta, syntax="rebuilt", history="rebuilt", scip="rebuilt", files_added=4, nodes_parsed=None,
               units_inserted=None, tags_inserted=None, commits_added=5, blob_rows_added=14, blobs_parsed=8,
               blobs_derived=8)

        # SCIP: views/scip.sql read, then refreshed against live (rebuilt), unchanged
        # (kept, and the index is not read), the index changed (rebuilt). Nothing else
        # is rebuilt or re-derived by reading views/scip.sql.
        index = make_scip_index(repo, root)
        if index is None:
            print(f"refresh fixture: SCIP steps SKIPPED ({RUST_ANALYZER} not found)")
        else:
            con.execute((SQL_DIR / SCIP_FILE).read_text())
            _, meta = refresh_against_live(con)
            expect(meta, scip="rebuilt")
            live_counts = rows(con, "SELECT (SELECT count(*) FROM live_scip_occurrences), "
                                    "(SELECT count(*) FROM live_scip_symbols)")[0]
            assert live_counts[0] > 0 and (meta["scip_occurrences_inserted"], meta["scip_symbols_inserted"]) == live_counts, meta
            assert rows(con, "SELECT count(*) FROM cache.scip_occurrences") == [(live_counts[0],)]
            _, meta = refresh_against_live(con)
            expect(meta)
            assert meta["scip_occurrences_inserted"] == 0, meta
            plan = "\n".join(r[1] for r in rows(con, "EXPLAIN INSERT INTO cache.scip_occurrences "
                                                     "SELECT * FROM scip_document_occurrences WHERE getvariable('refresh_scip_reset')"))
            assert "READ_JSON" not in plan and "READ_SCIP" not in plan, plan
            assert_no_scip_scan(con)
            # Another index with the same rows: an empty document appended (field 2,
            # length 0) changes the content hash.
            with open(index, "ab") as f:
                f.write(b"\x12\x00")
            _, meta = refresh_against_live(con)
            expect(meta, scip="rebuilt")
            # A missing index fails the refresh before anything changes.
            index.rename(index.with_name("moved.scip"))
            try:
                read_refresh(con)
            except duckdb.Error as e:
                assert "does not exist" in str(e), e
            else:
                raise AssertionError("refresh.sql with views/scip.sql and no index did not fail")
            assert rows(con, "SELECT state FROM cache.meta") == [("ok",)]
            index.with_name("moved.scip").rename(index)

        # cache.sql and refresh.sql refuse each other's views; the view files reset both.
        try:
            read_cache(con)
        except duckdb.Error as e:
            assert "persistent cache" in str(e), e
        else:
            raise AssertionError("cache.sql over the persistent cache did not fail")
        read_views(con)
        read_cache(con)
        try:
            read_refresh(con)
        except duckdb.Error as e:
            assert "session copies" in str(e), e
        else:
            raise AssertionError("refresh.sql over the session cache did not fail")
        con.close()

        # The cache file in a new session: read the view files, refresh (nothing to do),
        # query; and the views are live again after the view files are re-read. Without
        # views/scip.sql in this session, the SCIP tables are emptied.
        con = connect(repo, [report], variables)
        _, meta = refresh_against_live(con)
        expect(meta, scip="rebuilt" if index else "kept")  # nothing else rebuilt or parsed
        assert rows(con, "SELECT count(*) FROM cache.scip_occurrences") == [(0,)]
        assert rows(con, "SELECT count(*) FROM units") == rows(con, "SELECT count(*) FROM live_units")
        read_views(con)
        view_sql = rows(con, "SELECT sql FROM duckdb_views() WHERE view_name = 'nodes'")[0][0]
        assert "cache.nodes" not in view_sql and "read_ast" in view_sql, view_sql
        con.close()
    print("refresh fixture: ok")


# --- this repository -------------------------------------------------------------


def default_reports(repo):
    base = Path(repo).resolve() / "target/code-db/jscpd"
    return [base / "exact/jscpd-report.json", base / "normalized/jscpd-report.json"]


def connect_repo(repo, reports, variables=None):
    """connect(), reading views/scip.sql too when REPO has an index (repo_index)."""
    index = repo_index(repo)
    if index is None:
        print(f"no SCIP index at {Path(repo).resolve() / 'target/code-db/index.scip'}: views/scip.sql not read",
              file=sys.stderr)
    return connect(repo, reports, variables, scip=index is not None)


def check_repo(repo, reports):
    con = connect_repo(repo, reports)
    views = ["files", "nodes", "units", "tags", "commits", "file_changes", "hunks", "clone_pairs"]
    for view in views + (SCIP_VIEWS if scip_loaded(con) else []):
        print(f"{view}: {rows(con, f'SELECT count(*) FROM {view}')[0][0]}")
    print("units by kind:", rows(con, "SELECT kind, count(*) FROM units GROUP BY 1 ORDER BY 2 DESC"))
    print("tags by tag:", rows(con, "SELECT tag, count(*) FROM tags GROUP BY 1 ORDER BY 2 DESC"))
    for name in queries(con):
        result = rows(con, query_sql(name))
        print(f"{name}: {len(result)} rows; first: {result[:3]}")


# Each step runs in its own process. A statement is timed live and, in the `cached`
# column, after reading cache.sql (its build time is not included); `build cache` times
# cache.sql itself, layer by layer.
STATEMENTS = {
    **{f"count {v}": f"SELECT count(*) FROM {v}" for v in ["nodes", "units", "tags", "hunks", "clone_pairs",
                                                           "blob_files", "blob_units", "unit_changes"]},
    "nodes referenced twice": "SELECT (SELECT count(*) FROM nodes), (SELECT count(*) FROM nodes)",
    **{f"query {q}": q for q in QUERIES},
}
# Run only when REPO has a SCIP index. `baseline` is the process after the view files.
SCIP_STATEMENTS = {
    "baseline": "SELECT 1",
    **{f"count {v}": f"SELECT count(*) FROM {v}" for v in ["scip_index_occurrences", "scip_index_symbols",
                                                           "scip_occurrences", "scip_symbols"]},
    "scip_index_metadata": "SELECT * FROM scip_index_metadata",
    **{f"query {q}": q for q in SCIP_QUERIES},
}
STATEMENTS.update(SCIP_STATEMENTS)
BUILD_CACHE = "build cache"


def cache_layers():
    """cache.sql's statements grouped by layer: each layer starts at a DROP TABLE; the
    statements before the first one (the guard) form a layer of their own."""
    layers = [("guard", [])]
    for statement in (SQL_DIR / CACHE_FILE).read_text().split(";\n"):
        lines = [l for l in statement.splitlines() if l.strip() and not l.lstrip().startswith("--")]
        if not lines:
            continue
        sql = "\n".join(lines)
        if sql.startswith("DROP TABLE IF EXISTS "):
            layers.append((sql.split()[-1].removesuffix("_cache"), []))
        layers[-1][1].append(sql)
    return layers


REFRESH_STEP = "refresh"
FILE_SET_STEP = "file set"


def file_set_statement():
    """00_load.sql's statement that takes the file snapshot (the git_tracked_files step)."""
    text = (SQL_DIR / "00_load.sql").read_text()
    start = text.index("SET VARIABLE source_files = (")
    return text[start:text.index(";", start)]


REFRESH_STATS = ("syntax", "derived", "history", "nodes_parsed", "units_inserted", "tags_inserted",
                 "commits_added", "blob_history", "blob_rows_added", "blobs_parsed", "blob_bytes_parsed",
                 "blob_nodes_parsed", "blobs_derived", "blob_units_inserted", "blobs_cached", "scip_cache",
                 "scip_occurrences_inserted", "scip_symbols_inserted")


def run_step(repo, reports, step, cached, refreshed=False, uc_table=False):
    start = time.perf_counter()
    con = connect_repo(repo, reports, {"cache_path": sql_literal(str(refresh_cache_path(repo)))})
    load = time.perf_counter() - start
    rss = lambda: resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024
    if step == REFRESH_STEP:
        wall, meta = read_refresh(con)
        print(json.dumps({"wall": wall, "load": load, "rss_mib": rss(), **meta["seconds"],
                          **{k: v for k, v in meta.items() if k.startswith("files_") or k in REFRESH_STATS}}))
        return
    if step == FILE_SET_STEP:
        start = time.perf_counter()
        con.execute(file_set_statement())
        print(json.dumps({"wall": time.perf_counter() - start, "rss_mib": rss(),
                          "files": len(rows(con, "SELECT * FROM files"))}))
        return
    if refreshed:
        read_refresh(con)
    materialize = 0.0
    if uc_table:
        # unit_changes materialized (over whichever layers are in place) instead of a view.
        start = time.perf_counter()
        con.execute("CREATE TEMP TABLE unit_changes_table AS SELECT * FROM unit_changes")
        con.execute("CREATE OR REPLACE VIEW unit_changes AS SELECT * FROM unit_changes_table")
        materialize = time.perf_counter() - start
    if step == BUILD_CACHE:
        result = {}
        start = time.perf_counter()
        for layer, statements in cache_layers():
            t = time.perf_counter()
            for sql in statements:
                con.execute(sql)
            result[layer] = time.perf_counter() - t
        result["wall"] = time.perf_counter() - start
        result["rss_mib"] = rss()
        print(json.dumps(result))
        return
    if cached:
        read_cache(con)
    statement = STATEMENTS[step]
    sql = query_sql(statement) if statement in QUERIES + SCIP_QUERIES else statement
    start = time.perf_counter()
    con.execute(sql).fetchall()
    wall = time.perf_counter() - start
    print(json.dumps({"wall": wall, "load": load, "materialize": materialize, "rss_mib": rss()}))


def measure(repo, reports, runs, only=None):
    def series(step, cached):
        results = []
        for _ in range(runs):
            out = subprocess.run([sys.executable, __file__, "step", str(repo), step,
                                  "--reports", *map(str, reports), *(["--cached"] if cached else [])],
                                 check=True, capture_output=True, text=True).stdout
            results.append(json.loads(out.strip().splitlines()[-1]))
        return results

    def span(results, key, unit="s", fmt="{:.2f}"):
        values = [r[key] for r in results]
        return f"{fmt.format(min(values))}-{fmt.format(max(values))} {unit}"

    has_index = repo_index(repo) is not None
    for step in [*STATEMENTS, BUILD_CACHE]:
        if only and only not in step or step in SCIP_STATEMENTS and not has_index:
            continue
        if step == BUILD_CACHE:
            results = series(step, False)
            layers = ", ".join(f"{k} {span(results, k)}" for k in results[0] if k not in ("wall", "rss_mib"))
            print(f"{step}: {span(results, 'wall')} ({layers}), peak RSS {span(results, 'rss_mib', 'MiB', '{:.0f}')}")
            continue
        live, cached = series(step, False), series(step, True)
        print(f"{step}: live {span(live, 'wall')}, peak RSS {span(live, 'rss_mib', 'MiB', '{:.0f}')}; "
              f"cached {span(cached, 'wall')}, peak RSS {span(cached, 'rss_mib', 'MiB', '{:.0f}')}")


def refresh_cache_path(repo):
    """refresh.sql's default cache file for REPO, with its directory created."""
    path = Path(repo).resolve() / "target/code-db/cache.duckdb"
    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def check_refresh_repo(repo, reports):
    """Refresh REPO's cache twice, comparing every refreshed view and query with live."""
    con = connect_repo(repo, reports, {"cache_path": sql_literal(str(refresh_cache_path(repo)))})
    for attempt in ("first", "second"):
        wall, meta = refresh_against_live(con)
        print(f"{attempt} refresh: {wall:.2f} s, equal to live; meta: "
              + ", ".join(f"{k}={v}" for k, v in meta.items() if k not in ("seconds", "derive_key")))
        print("  seconds:", {k: round(v, 3) for k, v in meta["seconds"].items()})
    print("refresh: ok")


def measure_refresh(repo, reports, runs):
    """Time refresh.sql in a fresh process per run on REPO, a throwaway clone this
    command edits, commits to and resets (never a checkout you work in)."""
    repo = Path(repo).resolve()
    if repo == HORIZON_ROOT.resolve() or not (repo / ".git").is_dir():
        raise SystemExit("measure-refresh edits and commits: pass a throwaway clone")
    cache = refresh_cache_path(repo)
    base = git_out(repo, "rev-parse", "HEAD")
    rust = git_out(repo, "ls-files", "*.rs").splitlines()
    spread = rust[:: max(1, len(rust) // 10)][:10]
    index = repo_index(repo)
    index_bytes = index.read_bytes() if index else None

    def step(name, *extra):
        out = subprocess.run([sys.executable, __file__, "step", str(repo), name,
                              "--reports", *map(str, reports), *extra],
                             check=True, capture_output=True, text=True).stdout
        return json.loads(out.strip().splitlines()[-1])

    def reset_and_sync():
        git(repo, "reset", "-q", "--hard", base)
        step(REFRESH_STEP)

    def edit(paths):
        for p in paths:
            with open(repo / p, "a") as f:
                f.write("\nfn code_db_measure_edit() {}\n")

    def drop_cache():
        for p in (cache, cache.with_name(cache.name + ".wal")):
            p.unlink(missing_ok=True)
        shutil.rmtree(repo / "target/code-db/blobs", ignore_errors=True)

    def span(results, key, fmt="{:.2f}"):
        values = [r[key] for r in results]
        return f"{fmt.format(min(values))}-{fmt.format(max(values))}"

    def report(name, results):
        layers = ", ".join(f"{k} {span(results, k)}" for k in
                           ("files", "nodes", "units", "tags", "commits", "file_changes", "hunks", "blob_files",
                            "blob_nodes", "blob_units", "scip", "checkpoint"))
        last = results[-1]
        stats = ", ".join(f"{k}={last[k]}" for k in last if k.startswith("files_") or k in REFRESH_STATS)
        print(f"{name}: wall {span(results, 'wall')} s (view files {span(results, 'load')} s; "
              f"meta total {span(results, 'total')} s; {layers}), "
              f"peak RSS {span(results, 'rss_mib', '{:.0f}')} MiB; {stats}")

    def scenario(name, prepare):
        results = []
        for _ in range(runs):
            prepare()
            results.append(step(REFRESH_STEP))
        report(name, results)

    git(repo, "reset", "-q", "--hard", base)
    scenario("cold (empty cache)", drop_cache)
    scenario("nothing changed", lambda: None)
    scenario("1 file edited", lambda: (reset_and_sync(), edit(spread[:1])))
    scenario("10 files edited", lambda: (reset_and_sync(), edit(spread)))
    scenario("1 commit of 3 edited files",
             lambda: (reset_and_sync(), edit(spread[:3]), git(repo, "commit", "-q", "-am", "Measure")))
    scenario("amended HEAD (history rebuilt)",
             lambda: (reset_and_sync(), git(repo, "commit", "-q", "--amend", "-m", "Measure amend")))

    def change_index():
        # Another index with the same rows: an empty document appended (field 2, length 0).
        reset_and_sync()
        with open(index, "ab") as f:
            f.write(b"\x12\x00")

    if index:
        scenario("SCIP index changed (SCIP rebuilt)", change_index)
        index.write_bytes(index_bytes)
    reset_and_sync()
    size = sum(p.stat().st_size for p in (cache, cache.with_name(cache.name + ".wal")) if p.exists())
    print(f"cache file: {size / 2**20:.1f} MiB")
    for name in [*(f"query {q}" for q in QUERIES + (SCIP_QUERIES if index else [])), "count unit_changes"]:
        results = [step(name, "--refreshed") for _ in range(runs)]
        print(f"{name} from the cache: {span(results, 'wall')} s, "
              f"peak RSS {span(results, 'rss_mib', '{:.0f}')} MiB (refresh included)")
    for name in ["query unit_churn", "query clone_change_consistency", "count unit_changes"]:
        results = [step(name, "--refreshed", "--uc-table") for _ in range(runs)]
        print(f"{name} from the cache, unit_changes materialized: {span(results, 'wall')} s "
              f"(materializing {span(results, 'materialize')} s), "
              f"peak RSS {span(results, 'rss_mib', '{:.0f}')} MiB (refresh included)")
    results = [step(FILE_SET_STEP) for _ in range(runs)]
    print(f"file set (git_tracked_files snapshot): {span(results, 'wall', '{:.3f}')} s, "
          f"{results[-1]['files']} files")


def explain_counts(repo, reports):
    con = connect_repo(repo, reports)
    for view in ["nodes", "units", "tags"]:
        plan = "\n".join(r[1] for r in rows(con, f"EXPLAIN SELECT count(*) FROM {view}"))
        print(f"{view}: READ_AST in plan {plan.count('READ_AST')}, READ_TEXT {plan.count('READ_TEXT')}")
    twice = STATEMENTS["nodes referenced twice"]
    plan = "\n".join(r[1] for r in rows(con, "EXPLAIN " + twice))
    print(f"nodes referenced twice: READ_AST in plan {plan.count('READ_AST')}")
    for name in queries(con):
        plan = "\n".join(r[1] for r in rows(con, "EXPLAIN " + query_sql(name)))
        print(f"{name}: READ_AST in plan {plan.count('READ_AST')}, READ_TEXT {plan.count('READ_TEXT')}, "
              f"READ_JSON {plan.count('READ_JSON')}, READ_SCIP {plan.count('READ_SCIP')}")


def check_cli(repo, reports):
    cli = os.environ.get("DUCKDB_CLI", str(Path.home() / ".local/bin/duckdb2"))
    if not Path(cli).exists():
        print(f"cli: {cli} not found; skipped")
        return
    con = connect_repo(repo, reports)
    scip = scip_loaded(con)
    reads = "\n".join(f".read {SQL_DIR.relative_to(HORIZON_ROOT) / f}" for f in VIEW_FILES + ([SCIP_FILE] if scip else []))
    probe = ("SELECT (SELECT count(*) FROM files), (SELECT count(*) FROM units), (SELECT count(*) FROM tags), "
             "(SELECT count(*) FROM hunks), (SELECT count(*) FROM clone_pairs)"
             + (", (SELECT count(*) FROM scip_occurrences), (SELECT count(*) FROM scip_symbols);" if scip else ";"))
    query_counts = "\n".join(f"SELECT count(*) FROM ({query_sql(q).rstrip().rstrip(';')});" for q in queries(con))
    # The same counts again after refresh.sql (the persistent cache of REPO).
    script = (f"SET VARIABLE repo = {sql_literal(str(Path(repo).resolve()))};\n"
              f"SET VARIABLE clone_reports = {sql_literal([str(Path(r).resolve()) for r in reports])};\n"
              f"SET VARIABLE cache_path = {sql_literal(str(refresh_cache_path(repo)))};\n"
              f"{reads}\n.mode csv\n.headers off\n{probe}\n{query_counts}\n"
              f".read {SQL_DIR.relative_to(HORIZON_ROOT) / REFRESH_FILE}\n{probe}\n{query_counts}\n")
    proc = subprocess.run([cli, "-unsigned", "-no-agent", "-batch", "-bail"], input=script, cwd=HORIZON_ROOT,
                          capture_output=True, text=True)
    if proc.returncode:
        raise SystemExit(f"cli failed:\n{proc.stderr[-2000:]}")
    out = proc.stdout
    cli_values = [line for line in out.splitlines() if line and line[0].isdigit()]
    py_values = [",".join(str(v) for v in rows(con, probe.rstrip(";"))[0])]
    py_values += [str(len(rows(con, query_sql(q)))) for q in queries(con)]
    py_values += py_values  # live, then refreshed
    print("cli:", cli_values)
    print("python:", py_values)
    assert cli_values == py_values, "CLI and Python results differ"
    print("cli: ok")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["fixture", "scip-fixture", "repo", "refresh", "measure", "measure-refresh",
                                            "explain", "cli", "step"])
    parser.add_argument("repo", nargs="?")
    parser.add_argument("step", nargs="?")
    parser.add_argument("--reports", nargs="+")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--only", help="measure: run only the steps whose name contains this text")
    parser.add_argument("--cached", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--refreshed", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--uc-table", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.command == "fixture":
        check_fixture()
        return
    if args.command == "scip-fixture":
        check_scip_fixture()
        return
    if not args.repo:
        parser.error("REPO is required")
    reports = args.reports or default_reports(args.repo)
    if args.command == "repo":
        check_repo(args.repo, reports)
    elif args.command == "refresh":
        check_refresh_repo(args.repo, reports)
    elif args.command == "measure":
        measure(args.repo, reports, args.runs, args.only)
    elif args.command == "measure-refresh":
        measure_refresh(args.repo, reports, args.runs)
    elif args.command == "explain":
        explain_counts(args.repo, reports)
    elif args.command == "cli":
        check_cli(args.repo, reports)
    else:
        run_step(args.repo, reports, args.step, args.cached, args.refreshed, args.uc_table)


if __name__ == "__main__":
    main()
