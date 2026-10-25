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
        new session).

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
    "lang/rust.sql",
    "views/units.sql",
    "views/history.sql",
    "views/clones.sql",
]
CACHE_FILE = "cache.sql"
REFRESH_FILE = "refresh.sql"
# The views refresh.sql points at the persistent cache, plus files (its file snapshot).
REFRESHED_VIEWS = ["files", "nodes", "units", "tags", "commits", "file_changes", "hunks"]
QUERIES = ["file_cochange", "file_cochange_thresholds", "clone_site_tags", "clone_site_tags_summary"]


def sql_literal(value):
    if isinstance(value, list):
        return "[" + ", ".join(sql_literal(v) for v in value) + "]"
    return "'" + str(value).replace("'", "''") + "'"


def read_views(con):
    for name in VIEW_FILES:
        con.execute((SQL_DIR / name).read_text())


def read_cache(con):
    con.execute((SQL_DIR / CACHE_FILE).read_text())


def connect(repo, reports=None, variables=None):
    os.chdir(HORIZON_ROOT)
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"SET VARIABLE repo = {sql_literal(str(Path(repo).resolve()))}")
    if reports:
        con.execute(f"SET VARIABLE clone_reports = {sql_literal([str(Path(r).resolve()) for r in reports])}")
    for name, value in (variables or {}).items():
        con.execute(f"SET VARIABLE {name} = {value}")
    read_views(con)
    return con


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


def snapshot_live(con):
    """Copy every view refresh.sql replaces, read live, into temp tables live_<view>."""
    for view in REFRESHED_VIEWS:
        con.execute(f"CREATE OR REPLACE TEMP TABLE live_{view} AS SELECT * FROM {view}")


def diff_from_live(con):
    """{view: (live rows missing from the view, view rows missing from live)}, nonzero only."""
    diffs = {}
    for view in REFRESHED_VIEWS:
        counts = rows(con, f"SELECT (SELECT count(*) FROM (FROM live_{view} EXCEPT ALL FROM {view})), "
                           f"(SELECT count(*) FROM (FROM {view} EXCEPT ALL FROM live_{view}))")[0]
        if counts != (0, 0):
            diffs[view] = counts
    return diffs


def refresh_against_live(con):
    """Re-read the view files (live, with a new file snapshot), copy the live rows and
    the four queries' results, read refresh.sql and compare. Returns (wall, meta)."""
    read_views(con)
    snapshot_live(con)
    live_queries = [rows(con, query_sql(q)) for q in QUERIES]
    wall, meta = read_refresh(con)
    diffs = diff_from_live(con)
    assert not diffs, f"refreshed views differ from live: {diffs}"
    assert [rows(con, query_sql(q)) for q in QUERIES] == live_queries, "queries differ after refresh"
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


def git_env(repo, date=None):
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(repo),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_AUTHOR_NAME": "Fixture",
        "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
        "GIT_COMMITTER_NAME": "Fixture",
        "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
    }
    if date:
        env["GIT_AUTHOR_DATE"] = env["GIT_COMMITTER_DATE"] = date
    return env


def git(repo, *args, date=None):
    subprocess.run(["git", *args], cwd=repo, env=git_env(repo, date), check=True, capture_output=True)


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
        plan = "\n".join(r[1] for r in rows(con, "EXPLAIN " + query_sql("clone_site_tags")))
        assert "READ_AST" not in plan and "READ_TEXT" not in plan, plan
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
    check_refresh_fixture()


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
                  "tags_inserted", "commits_added")

        def stats(meta):
            return {k: meta[k] for k in counts}

        def expect(meta, syntax="incremental", history="appended", **nonzero):
            expected = {k: nonzero.get(k, 0) for k in counts}
            got = stats(meta)
            for k, v in nonzero.items():
                if v is None:  # nonzero, value not asserted
                    assert got[k] > 0, (k, got)
                    expected[k] = got[k]
            assert (meta["syntax"], meta["history"], got) == (syntax, history, expected), meta
            assert meta["state"] == "ok" and meta["derived"] == syntax, meta

        # Initial refresh from an empty cache: everything built, equal to live.
        _, meta = refresh_against_live(con)
        expect(meta, syntax="rebuilt", history="rebuilt", files_added=4, nodes_parsed=None,
               units_inserted=None, tags_inserted=None, commits_added=4)
        assert meta["head"] == git_out(repo, "rev-parse", "HEAD"), meta
        assert rows(con, "SELECT count(*) FROM cache.nodes_files") == [(4,)]
        plan = "\n".join(r[1] for r in rows(con, "EXPLAIN " + query_sql("clone_site_tags")))
        assert "READ_AST" not in plan and "READ_TEXT" not in plan, plan

        # Nothing changed: no file parsed, nothing appended; reading refresh.sql again in
        # the same session takes under 100 ms (the best of three reads, as the desktop
        # this runs on is shared).
        _, meta = refresh_against_live(con)
        expect(meta)
        walls = []
        for _ in range(3):
            wall, meta = read_refresh(con)
            expect(meta)
            walls.append(wall)
        assert min(walls) < 0.1, f"refresh with nothing changed took {walls} s: {meta['seconds']}"
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
        expect(meta, commits_added=1)
        assert meta["head"] == git_out(repo, "rev-parse", "HEAD"), meta
        assert rows(con, "SELECT count(*) FROM commits") == [(5,)]

        # An amended commit: the cached head is gone from HEAD's history, so it is rebuilt.
        git(repo, "commit", "-q", "--amend", "-m", "Amended", date="2026-01-06T00:00:00Z")
        _, meta = refresh_against_live(con)
        expect(meta, history="rebuilt", commits_added=5)
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
        expect(meta, syntax="rebuilt", history="rebuilt", files_added=4, nodes_parsed=None,
               units_inserted=None, tags_inserted=None, commits_added=5)

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
        # query; and the views are live again after the view files are re-read.
        con = connect(repo, [report], variables)
        _, meta = refresh_against_live(con)
        expect(meta)  # nothing rebuilt or parsed
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


def check_repo(repo, reports):
    con = connect(repo, reports)
    for view in ["files", "nodes", "units", "tags", "commits", "file_changes", "hunks", "clone_pairs"]:
        print(f"{view}: {rows(con, f'SELECT count(*) FROM {view}')[0][0]}")
    print("units by kind:", rows(con, "SELECT kind, count(*) FROM units GROUP BY 1 ORDER BY 2 DESC"))
    print("tags by tag:", rows(con, "SELECT tag, count(*) FROM tags GROUP BY 1 ORDER BY 2 DESC"))
    for name in QUERIES:
        result = rows(con, query_sql(name))
        print(f"{name}: {len(result)} rows; first: {result[:3]}")


# Each step runs in its own process. A statement is timed live and, in the `cached`
# column, after reading cache.sql (its build time is not included); `build cache` times
# cache.sql itself, layer by layer.
STATEMENTS = {
    **{f"count {v}": f"SELECT count(*) FROM {v}" for v in ["nodes", "units", "tags", "hunks", "clone_pairs"]},
    "nodes referenced twice": "SELECT (SELECT count(*) FROM nodes), (SELECT count(*) FROM nodes)",
    **{f"query {q}": q for q in QUERIES},
}
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


def run_step(repo, reports, step, cached, refreshed=False):
    con = connect(repo, reports, {"cache_path": sql_literal(str(refresh_cache_path(repo)))})
    rss = lambda: resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024
    if step == REFRESH_STEP:
        wall, meta = read_refresh(con)
        print(json.dumps({"wall": wall, "rss_mib": rss(), **meta["seconds"],
                          **{k: v for k, v in meta.items() if k.startswith("files_") or k in
                             ("syntax", "derived", "history", "nodes_parsed", "units_inserted",
                              "tags_inserted", "commits_added")}}))
        return
    if step == FILE_SET_STEP:
        start = time.perf_counter()
        con.execute(file_set_statement())
        print(json.dumps({"wall": time.perf_counter() - start, "rss_mib": rss(),
                          "files": len(rows(con, "SELECT * FROM files"))}))
        return
    if refreshed:
        read_refresh(con)
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
    sql = query_sql(statement) if statement in QUERIES else statement
    start = time.perf_counter()
    con.execute(sql).fetchall()
    wall = time.perf_counter() - start
    print(json.dumps({"wall": wall, "rss_mib": rss()}))


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

    for step in [*STATEMENTS, BUILD_CACHE]:
        if only and only not in step:
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
    con = connect(repo, reports, {"cache_path": sql_literal(str(refresh_cache_path(repo)))})
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

    def span(results, key, fmt="{:.2f}"):
        values = [r[key] for r in results]
        return f"{fmt.format(min(values))}-{fmt.format(max(values))}"

    def report(name, results):
        layers = ", ".join(f"{k} {span(results, k)}" for k in
                           ("files", "nodes", "units", "tags", "commits", "file_changes", "hunks", "checkpoint"))
        last = results[-1]
        stats = ", ".join(f"{k}={last[k]}" for k in last if k.startswith("files_") or k in
                          ("syntax", "derived", "history", "nodes_parsed", "units_inserted", "tags_inserted",
                           "commits_added"))
        print(f"{name}: wall {span(results, 'wall')} s (meta total {span(results, 'total')} s; {layers}), "
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
    reset_and_sync()
    size = sum(p.stat().st_size for p in (cache, cache.with_name(cache.name + ".wal")) if p.exists())
    print(f"cache file: {size / 2**20:.1f} MiB")
    for name in QUERIES:
        results = [step(f"query {name}", "--refreshed") for _ in range(runs)]
        print(f"query {name} from the cache: {span(results, 'wall')} s, "
              f"peak RSS {span(results, 'rss_mib', '{:.0f}')} MiB (refresh included)")
    results = [step(FILE_SET_STEP) for _ in range(runs)]
    print(f"file set (git_tracked_files snapshot): {span(results, 'wall', '{:.3f}')} s, "
          f"{results[-1]['files']} files")


def explain_counts(repo, reports):
    con = connect(repo, reports)
    for view in ["nodes", "units", "tags"]:
        plan = "\n".join(r[1] for r in rows(con, f"EXPLAIN SELECT count(*) FROM {view}"))
        print(f"{view}: READ_AST in plan {plan.count('READ_AST')}, READ_TEXT {plan.count('READ_TEXT')}")
    twice = STATEMENTS["nodes referenced twice"]
    plan = "\n".join(r[1] for r in rows(con, "EXPLAIN " + twice))
    print(f"nodes referenced twice: READ_AST in plan {plan.count('READ_AST')}")
    for name in QUERIES:
        plan = "\n".join(r[1] for r in rows(con, "EXPLAIN " + query_sql(name)))
        print(f"{name}: READ_AST in plan {plan.count('READ_AST')}, READ_TEXT {plan.count('READ_TEXT')}")


def check_cli(repo, reports):
    cli = os.environ.get("DUCKDB_CLI", str(Path.home() / ".local/bin/duckdb2"))
    if not Path(cli).exists():
        print(f"cli: {cli} not found; skipped")
        return
    reads = "\n".join(f".read {SQL_DIR.relative_to(HORIZON_ROOT) / f}" for f in VIEW_FILES)
    probe = ("SELECT (SELECT count(*) FROM files), (SELECT count(*) FROM units), (SELECT count(*) FROM tags), "
             "(SELECT count(*) FROM hunks), (SELECT count(*) FROM clone_pairs);")
    query_counts = "\n".join(f"SELECT count(*) FROM ({query_sql(q).rstrip().rstrip(';')});" for q in QUERIES)
    # The same counts again after refresh.sql (the persistent cache of REPO).
    script = (f"SET VARIABLE repo = {sql_literal(str(Path(repo).resolve()))};\n"
              f"SET VARIABLE clone_reports = {sql_literal([str(Path(r).resolve()) for r in reports])};\n"
              f"SET VARIABLE cache_path = {sql_literal(str(refresh_cache_path(repo)))};\n"
              f"{reads}\n.mode csv\n.headers off\n{probe}\n{query_counts}\n"
              f".read {SQL_DIR.relative_to(HORIZON_ROOT) / REFRESH_FILE}\n{probe}\n{query_counts}\n")
    out = subprocess.run([cli, "-unsigned", "-no-agent", "-batch", "-bail"], input=script, cwd=HORIZON_ROOT,
                         check=True, capture_output=True, text=True).stdout
    cli_values = [line for line in out.splitlines() if line and line[0].isdigit()]
    con = connect(repo, reports)
    py_values = [",".join(str(v) for v in rows(con, probe.rstrip(";"))[0])]
    py_values += [str(len(rows(con, query_sql(q)))) for q in QUERIES]
    py_values += py_values  # live, then refreshed
    print("cli:", cli_values)
    print("python:", py_values)
    assert cli_values == py_values, "CLI and Python results differ"
    print("cli: ok")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["fixture", "repo", "refresh", "measure", "measure-refresh",
                                            "explain", "cli", "step"])
    parser.add_argument("repo", nargs="?")
    parser.add_argument("step", nargs="?")
    parser.add_argument("--reports", nargs="+")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--only", help="measure: run only the steps whose name contains this text")
    parser.add_argument("--cached", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--refreshed", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.command == "fixture":
        check_fixture()
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
        run_step(args.repo, reports, args.step, args.cached, args.refreshed)


if __name__ == "__main__":
    main()
