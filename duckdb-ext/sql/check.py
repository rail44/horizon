# /// script
# requires-python = ">=3.12"
# dependencies = ["duckdb==2.0.0"]
# ///
"""Execute the code database SQL files in order and run every query.

    uv run --script duckdb-ext/sql/check.py fixture
        Build a small git repository in a temporary directory (Rust files with a
        trait impl, a test module, an untracked file, excluded build and hidden
        directories, several commits) plus a jscpd-shaped report, and assert on the
        rows of every view and query.

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
        ~/.local/bin/duckdb2) and compare its counts with this client's.

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


def git(repo, *args, date=None):
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
    subprocess.run(["git", *args], cwd=repo, env=env, check=True, capture_output=True)


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
    # Not committed: an untracked source file (included), a build output and a file in
    # a hidden directory (both excluded by the default exclude_patterns).
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
        con = connect(repo, [report], {"min_shared": "2"})
        shapes = (repo / "src/shapes.rs").read_text()

        files = rows(con, "SELECT path, language FROM files ORDER BY path")
        assert files == [("src/lib.rs", "rust"), ("src/rect.rs", "rust"), ("src/scratch.rs", "rust"),
                         ("src/shapes.rs", "rust"), ("tests/it.rs", "rust")], files

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
        assert changes == [("README.md", 2), ("src/lib.rs", 3), ("src/rect.rs", 1),
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
    """cache.sql's statements grouped by layer: each layer starts at a DROP TABLE."""
    layers = []
    for statement in (SQL_DIR / CACHE_FILE).read_text().split(";\n"):
        lines = [l for l in statement.splitlines() if l.strip() and not l.lstrip().startswith("--")]
        if not lines:
            continue
        sql = "\n".join(lines)
        if sql.startswith("DROP TABLE IF EXISTS "):
            layers.append((sql.split()[-1].removesuffix("_cache"), []))
        layers[-1][1].append(sql)
    return layers


def run_step(repo, reports, step, cached):
    con = connect(repo, reports)
    rss = lambda: resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024
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
    script = (f"SET VARIABLE repo = {sql_literal(str(Path(repo).resolve()))};\n"
              f"SET VARIABLE clone_reports = {sql_literal([str(Path(r).resolve()) for r in reports])};\n"
              f"{reads}\n.mode csv\n.headers off\n{probe}\n{query_counts}\n")
    out = subprocess.run([cli, "-unsigned", "-no-agent", "-batch"], input=script, cwd=HORIZON_ROOT,
                         check=True, capture_output=True, text=True).stdout
    cli_values = [line for line in out.splitlines() if line and line[0].isdigit()]
    con = connect(repo, reports)
    py_values = [",".join(str(v) for v in rows(con, probe.rstrip(";"))[0])]
    py_values += [str(len(rows(con, query_sql(q)))) for q in QUERIES]
    print("cli:", cli_values)
    print("python:", py_values)
    assert cli_values == py_values, "CLI and Python results differ"
    print("cli: ok")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["fixture", "repo", "measure", "explain", "cli", "step"])
    parser.add_argument("repo", nargs="?")
    parser.add_argument("step", nargs="?")
    parser.add_argument("--reports", nargs="+")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--only", help="measure: run only the steps whose name contains this text")
    parser.add_argument("--cached", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.command == "fixture":
        check_fixture()
        return
    if not args.repo:
        parser.error("REPO is required")
    reports = args.reports or default_reports(args.repo)
    if args.command == "repo":
        check_repo(args.repo, reports)
    elif args.command == "measure":
        measure(args.repo, reports, args.runs, args.only)
    elif args.command == "explain":
        explain_counts(args.repo, reports)
    elif args.command == "cli":
        check_cli(args.repo, reports)
    else:
        run_step(args.repo, reports, args.step, args.cached)


if __name__ == "__main__":
    main()
