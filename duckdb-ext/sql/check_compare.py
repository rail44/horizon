"""The comparison's fixture (README.md, Comparison), run by `check.py fixture`.

Two snapshots of one small Cargo workspace, each its own git repository indexed with
rust-analyzer, with known changes between them: a function split into three (`big`
keeps its name and shrinks, `part_a` and `part_b` appear), a rename (`old_name` to
`new_name`: one disappeared and one appeared row), a duplicate removed (`dup_b` calls
`dup_a` instead of repeating its body: the clone pair disappears), a cycle broken (`b`
stops calling `a`), a layer violation introduced (`core` calls `ui`, which the layers
forbid, closing a new cycle), a constant whose value changed, an unchanged function, a
test, a crate edge that gains a unit edge, and an integration test whose crate root shares
its descriptor with the binary's (paired by path). Each side is read the way `code-db compare`
reads it: BEFORE's side_* views (compare/sides.sql) copied by compare/export.sql into a
file, attached read-only as `before` in AFTER's connection, then compare/views.sql and the
compare queries.
"""

import json
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True
import check  # noqa: E402

COMPARE_DIR = check.SQL_DIR / "compare"
COMPARE_QUERIES = ["summary", "units", "unit_axes", "clones", "edges", "cycles", "layer_violations"]

WORKSPACE_TOML = """\
[workspace]
members = ["shapes", "app"]
resolver = "2"
"""

APP_TOML = check.APP_TOML

LIB_BEFORE = """\
//! Shapes.
pub mod a;
pub mod b;
pub mod core;
pub mod ui;

pub const LIMIT: u32 = 1;

pub fn big(x: u32) -> u32 {
    let one = x + 1;
    let two = one * 2;
    let three = two + 3;
    let four = three * 4;
    let five = four + 5;
    let six = five * 6;
    six + LIMIT
}

pub fn old_name() -> u32 {
    7
}

pub fn stable() -> u32 {
    8
}

pub fn dup_a(values: &[u32]) -> u32 {
    let mut total = 0;
    for value in values {
        total += value * 2 + 1;
    }
    total
}

pub fn dup_b(values: &[u32]) -> u32 {
    let mut total = 0;
    for value in values {
        total += value * 2 + 1;
    }
    total
}

#[cfg(test)]
mod tests {
    #[test]
    fn checks_big() {
        assert!(super::big(0) > 0);
    }
}
"""

LIB_AFTER = """\
//! Shapes.
pub mod a;
pub mod b;
pub mod core;
pub mod ui;

pub const LIMIT: u32 = 2;

pub fn big(x: u32) -> u32 {
    part_b(part_a(x)) + LIMIT
}

fn part_a(x: u32) -> u32 {
    let one = x + 1;
    let two = one * 2;
    two + 3
}

fn part_b(three: u32) -> u32 {
    let four = three * 4;
    let five = four + 5;
    five * 6
}

pub fn new_name() -> u32 {
    7
}

pub fn stable() -> u32 {
    8
}

pub fn dup_a(values: &[u32]) -> u32 {
    let mut total = 0;
    for value in values {
        total += value * 2 + 1;
    }
    total
}

pub fn dup_b(values: &[u32]) -> u32 {
    dup_a(values)
}

#[cfg(test)]
mod tests {
    #[test]
    fn checks_big() {
        assert!(super::big(0) > 0);
    }
}
"""

A_RS = """\
pub fn ping() -> u32 {
    crate::b::pong() + 1
}

pub fn base() -> u32 {
    1
}
"""

B_BEFORE = """\
pub fn pong() -> u32 {
    crate::a::base() + 1
}
"""

B_AFTER = """\
pub fn pong() -> u32 {
    2
}
"""

CORE_BEFORE = """\
pub fn value() -> u32 {
    3
}
"""

CORE_AFTER = """\
pub fn value() -> u32 {
    crate::ui::width()
}
"""

UI_BEFORE = """\
pub fn show() -> u32 {
    crate::core::value()
}
"""

UI_AFTER = """\
pub fn show() -> u32 {
    crate::core::value()
}

pub fn width() -> u32 {
    3
}
"""

APP_IT = """\
#[test]
fn integration() {
    assert!(shapes::LIMIT > 0);
}
"""

MAIN_BEFORE = """\
fn main() {
    println!("{}", shapes::big(1));
}
"""

MAIN_AFTER = """\
fn main() {
    println!("{}", shapes::big(1) + shapes::stable());
}
"""

SNAPSHOTS = {
    "before": {"shapes/src/lib.rs": LIB_BEFORE, "shapes/src/b.rs": B_BEFORE, "shapes/src/core.rs": CORE_BEFORE,
               "shapes/src/ui.rs": UI_BEFORE, "app/src/main.rs": MAIN_BEFORE},
    "after": {"shapes/src/lib.rs": LIB_AFTER, "shapes/src/b.rs": B_AFTER, "shapes/src/core.rs": CORE_AFTER,
              "shapes/src/ui.rs": UI_AFTER, "app/src/main.rs": MAIN_AFTER},
}
COMMON = {
    "Cargo.toml": WORKSPACE_TOML,
    "shapes/Cargo.toml": check.SHAPES_TOML,
    "shapes/src/a.rs": A_RS,
    "app/Cargo.toml": APP_TOML,
    # A second target of `app`: its root shares the descriptor `crate/` with main.rs's.
    "app/tests/it.rs": APP_IT,
    ".gitignore": "target/\n",
}
# `ui` may use `core`, not the other way round.
LAYERS = ([("core", "shapes/src/core.rs*"), ("ui", "shapes/src/ui.rs*")], [("ui", "core")])


def report(repo, lib):
    """jscpd-shaped reports: in `exact`, dup_a's and dup_b's loops when dup_b repeats it."""
    def side(start, end):
        return {"name": str(repo / "shapes/src/lib.rs"), "start": start, "end": end,
                "startLoc": {"line": start, "column": 1}, "endLoc": {"line": end, "column": 1}}

    starts = [n for n, line in enumerate(lib.splitlines(), 1) if line == "    let mut total = 0;"]
    duplicates = []
    if len(starts) == 2:
        duplicates.append({"format": "rust", "lines": 5, "tokens": 40, "fragment": "",
                           "firstFile": side(starts[0], starts[0] + 4), "secondFile": side(starts[1], starts[1] + 4)})
    for mode, dups in (("exact", duplicates), ("normalized", [])):
        out = repo / "target/code-db/jscpd" / mode
        out.mkdir(parents=True, exist_ok=True)
        (out / "jscpd-report.json").write_text(json.dumps({"duplicates": dups, "statistics": {}}))


def build(root, name):
    repo = root / name
    repo.mkdir()
    check.git(repo, "init", "-q", "-b", "main")
    files = {**COMMON, **SNAPSHOTS[name]}
    for path, text in files.items():
        check.write(repo, path, text)
    check.git(repo, "add", "-A")
    check.git(repo, "commit", "-q", "-m", name)
    out = repo / "target/code-db/scip"
    out.mkdir(parents=True)
    check.subprocess.run([check.RUST_ANALYZER, "scip", ".", "--output", str(out / "root.scip")], check=True,
                         capture_output=True, cwd=repo)
    report(repo, files["shapes/src/lib.rs"])
    (repo / "target/code-db/cache").mkdir(parents=True)
    return repo


def compare_connection(after, before, snapshot):
    """AFTER's connection with BEFORE's side copied into SNAPSHOT and attached as `before`,
    the way `code-db compare` builds it."""
    con = check.connect(before, layers=LAYERS)
    check.read_refresh(con)
    con.execute((COMPARE_DIR / "sides.sql").read_text())
    con.execute(f"ATTACH {check.sql_literal(str(snapshot))} AS snapshot")
    con.execute((COMPARE_DIR / "export.sql").read_text())
    con.execute("DETACH snapshot")
    con.close()
    con = check.connect(after, layers=LAYERS)
    check.read_refresh(con)
    con.execute((COMPARE_DIR / "sides.sql").read_text())
    con.execute(f"ATTACH {check.sql_literal(str(snapshot))} AS before (READ_ONLY)")
    con.execute((COMPARE_DIR / "views.sql").read_text())
    return con


def query_rows(con, name):
    return check.rows(con, (COMPARE_DIR / "queries" / f"{name}.sql").read_text())


CHECKED = [
    ("compare_units", "SELECT status, is_test, kind, qualified_name, path_after, text_changed, ambiguous, paired_by, "
                      "loc_before, loc_after FROM compare_units ORDER BY ALL"),
    ("compare_unit_axes changed", "SELECT qualified_name, column_name, axis, before_value, after_value, delta, change, "
                                  "effect FROM compare_unit_axes WHERE status = 'kept' AND change <> 'unchanged' "
                                  "ORDER BY ALL"),
    ("compare_clones", "SELECT source, unit_a, unit_b, status, is_test, pairs_before, pairs_after FROM compare_clones "
                       "ORDER BY ALL"),
    ("compare_edges", "SELECT level, src, dst, status, is_test, unit_edges_before, unit_edges_after, "
                      "production_unit_edges_delta FROM compare_edges WHERE status <> 'kept' OR unit_edges_delta <> 0 "
                      "ORDER BY ALL"),
    ("compare_cycles", "SELECT status, cycle_before, cycle_after, modules_before, modules_after, members_added, "
                       "members_removed, split, merged FROM compare_cycles ORDER BY ALL"),
    ("compare_layer_violations", "SELECT * FROM compare_layer_violations ORDER BY ALL"),
    *[(f"query {q}", q) for q in COMPARE_QUERIES],
]


def check_compare(print_only=False):
    if check.shutil.which(check.RUST_ANALYZER) is None:
        print(f"compare fixture: skipped, {check.RUST_ANALYZER} not on PATH")
        return
    with tempfile.TemporaryDirectory(prefix="code-db-compare-") as tmp:
        root = Path(tmp)
        before, after = build(root, "before"), build(root, "after")
        con = compare_connection(after, before, root / "before-side.duckdb")
        results = {title: query_rows(con, sql) if sql in COMPARE_QUERIES else check.rows(con, sql)
                   for title, sql in CHECKED}
        if print_only:
            for title, result in results.items():
                check.print_rows(con, title, result)
            return
        for title, expected in EXPECTED.items():
            assert results[title] == expected, f"{title}:\n  got      {results[title]}\n  expected {expected}"
        # The copy is read-only.
        check.assert_error(con, "DELETE FROM before.side_units", "read-only")
        print("compare fixture: ok")


# The fixture's expected rows (CHECKED), each read and checked by hand against the two
# snapshots' text when it was written.
EXPECTED = {
    'compare_units': [
        ('appeared', False, 'function', 'new_name', 'shapes/src/lib.rs', None, False, None, None, 3),
        ('appeared', False, 'function', 'part_a', 'shapes/src/lib.rs', None, False, None, None, 5),
        ('appeared', False, 'function', 'part_b', 'shapes/src/lib.rs', None, False, None, None, 5),
        ('appeared', False, 'function', 'width', 'shapes/src/ui.rs', None, False, None, None, 3),
        ('disappeared', False, 'function', 'old_name', None, None, False, None, 3, None),
        ('kept', False, 'constant', 'LIMIT', 'shapes/src/lib.rs', True, False, None, 1, 1),
        ('kept', False, 'function', 'base', 'shapes/src/a.rs', False, False, None, 3, 3),
        ('kept', False, 'function', 'big', 'shapes/src/lib.rs', True, False, None, 9, 3),
        ('kept', False, 'function', 'dup_a', 'shapes/src/lib.rs', False, False, None, 7, 7),
        ('kept', False, 'function', 'dup_b', 'shapes/src/lib.rs', True, False, None, 7, 3),
        ('kept', False, 'function', 'main', 'app/src/main.rs', True, False, None, 3, 3),
        ('kept', False, 'function', 'ping', 'shapes/src/a.rs', False, False, None, 3, 3),
        ('kept', False, 'function', 'pong', 'shapes/src/b.rs', True, False, None, 3, 3),
        ('kept', False, 'function', 'show', 'shapes/src/ui.rs', False, False, None, 3, 3),
        ('kept', False, 'function', 'stable', 'shapes/src/lib.rs', False, False, None, 3, 3),
        ('kept', False, 'function', 'value', 'shapes/src/core.rs', True, False, None, 3, 3),
        ('kept', False, 'module', 'a', 'shapes/src/a.rs', False, False, None, 6, 6),
        ('kept', False, 'module', 'app', 'app/src/main.rs', True, True, 'path', 3, 3),
        ('kept', False, 'module', 'b', 'shapes/src/b.rs', True, False, None, 3, 3),
        ('kept', False, 'module', 'core', 'shapes/src/core.rs', True, False, None, 3, 3),
        ('kept', False, 'module', 'shapes', 'shapes/src/lib.rs', True, False, None, 42, 42),
        ('kept', False, 'module', 'ui', 'shapes/src/ui.rs', True, False, None, 3, 6),
        ('kept', True, 'function', 'integration', 'app/tests/it.rs', False, False, None, 4, 4),
        ('kept', True, 'function', 'tests.checks_big', 'shapes/src/lib.rs', False, False, None, 4, 4),
        ('kept', True, 'module', 'app', 'app/tests/it.rs', False, True, 'path', 4, 4),
        ('kept', True, 'module', 'tests', 'shapes/src/lib.rs', False, False, None, 7, 7),
    ],
    'compare_unit_axes changed': [
        ('a', 'cycle', 'independence', 'shapes/src/a.rs', None, None, 'changed', 'improved'),
        ('a', 'fan_in_files', 'coupling', '1', '0', -1.0, 'decreased', 'improved'),
        ('a', 'references', 'unreferenced', '2', '1', -1.0, 'decreased', None),
        ('b', 'cycle', 'independence', 'shapes/src/a.rs', None, None, 'changed', 'improved'),
        ('b', 'fan_out_files', 'coupling', '1', '0', -1.0, 'decreased', 'improved'),
        ('base', 'cycle', 'independence', 'shapes/src/a.rs', None, None, 'changed', 'improved'),
        ('base', 'fan_in_files', 'coupling', '1', '0', -1.0, 'decreased', 'improved'),
        ('base', 'reference_status', 'unreferenced', 'referenced', 'unreferenced', None, 'changed', 'regressed'),
        ('base', 'references', 'unreferenced', '1', '0', -1.0, 'decreased', None),
        ('big', 'loc', 'size', '9', '3', -6.0, 'decreased', 'improved'),
        ('core', 'cycle', 'independence', None, 'shapes/src/core.rs', None, 'changed', 'regressed'),
        ('core', 'fan_out_files', 'coupling', '0', '1', 1.0, 'increased', 'regressed'),
        ('dup_a', 'clone_pairs', 'duplication', '1', '0', -1.0, 'decreased', 'improved'),
        ('dup_a', 'exact_pairs', 'duplication', '1', '0', -1.0, 'decreased', 'improved'),
        ('dup_a', 'reference_status', 'unreferenced', 'unreferenced', 'referenced', None, 'changed', 'improved'),
        ('dup_a', 'references', 'unreferenced', '0', '1', 1.0, 'increased', None),
        ('dup_b', 'clone_pairs', 'duplication', '1', '0', -1.0, 'decreased', 'improved'),
        ('dup_b', 'exact_pairs', 'duplication', '1', '0', -1.0, 'decreased', 'improved'),
        ('dup_b', 'loc', 'size', '7', '3', -4.0, 'decreased', 'improved'),
        ('ping', 'cycle', 'independence', 'shapes/src/a.rs', None, None, 'changed', 'improved'),
        ('pong', 'cycle', 'independence', 'shapes/src/a.rs', None, None, 'changed', 'improved'),
        ('pong', 'fan_out_files', 'coupling', '1', '0', -1.0, 'decreased', 'improved'),
        ('shapes', 'references', 'unreferenced', '6', '7', 1.0, 'increased', None),
        ('show', 'cycle', 'independence', None, 'shapes/src/core.rs', None, 'changed', 'regressed'),
        ('stable', 'fan_in_crates', 'coupling', '0', '1', 1.0, 'increased', 'regressed'),
        ('stable', 'fan_in_files', 'coupling', '0', '1', 1.0, 'increased', 'regressed'),
        ('stable', 'reference_status', 'unreferenced', 'unreferenced', 'referenced', None, 'changed', 'improved'),
        ('stable', 'references', 'unreferenced', '0', '1', 1.0, 'increased', None),
        ('ui', 'cycle', 'independence', None, 'shapes/src/core.rs', None, 'changed', 'regressed'),
        ('ui', 'fan_in_files', 'coupling', '0', '1', 1.0, 'increased', 'regressed'),
        ('ui', 'loc', 'size', '3', '6', 3.0, 'increased', 'regressed'),
        ('ui', 'references', 'unreferenced', '1', '2', 1.0, 'increased', None),
        ('value', 'cycle', 'independence', None, 'shapes/src/core.rs', None, 'changed', 'regressed'),
        ('value', 'fan_out_files', 'coupling', '0', '1', 1.0, 'increased', 'regressed'),
    ],
    'compare_clones': [
        ('exact', 'dup_a', 'dup_b', 'disappeared', False, 1, 0),
    ],
    'compare_edges': [
        ('crate', 'app', 'shapes', 'kept', False, 2, 3, 1),
        ('module', 'app/src/main.rs', 'shapes/src/lib.rs', 'kept', False, 1, 2, 1),
        ('module', 'shapes/src/b.rs', 'shapes/src/a.rs', 'disappeared', False, 1, 0, -1),
        ('module', 'shapes/src/core.rs', 'shapes/src/ui.rs', 'appeared', False, 0, 1, 1),
    ],
    'compare_cycles': [
        ('appeared', None, 'shapes/src/core.rs', 0, 2, ['shapes/src/core.rs', 'shapes/src/ui.rs'], [], False, False),
        ('disappeared', 'shapes/src/a.rs', None, 2, 0, [], ['shapes/src/a.rs', 'shapes/src/b.rs'], False, False),
    ],
    'compare_layer_violations': [
        ('core', 'ui', 'shapes/src/core.rs', 'shapes/src/ui.rs', 'appeared', False, 0, 1, 1),
    ],
    'query summary': [
        ('units', 'units', 'production', 18.0, 21.0, 17, None, None, 11, 4, 1),
        ('units', 'units', 'test', 4.0, 4.0, 4, None, None, 0, 0, 0),
        ('coupling', 'fan_in_crates', 'production', 3.0, 4.0, 1, 0, 1, 0, 0, 0),
        ('coupling', 'fan_in_crates', 'test', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('coupling', 'fan_in_files', 'production', 10.0, 11.0, 4, 2, 2, 0, 1, 0),
        ('coupling', 'fan_in_files', 'test', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('coupling', 'fan_out_crates', 'production', 2.0, 2.0, 0, 0, 0, 0, 0, 0),
        ('coupling', 'fan_out_crates', 'test', 2.0, 2.0, 0, 0, 0, 0, 0, 0),
        ('coupling', 'fan_out_files', 'production', 8.0, 8.0, 4, 2, 2, 0, 0, 0),
        ('coupling', 'fan_out_files', 'test', 2.0, 2.0, 0, 0, 0, 0, 0, 0),
        ('duplication', 'clone_pairs', 'production', 2.0, 0.0, 2, 2, 0, 0, 0, 0),
        ('duplication', 'clone_pairs', 'test', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('duplication', 'exact_pairs', 'production', 2.0, 0.0, 2, 2, 0, 0, 0, 0),
        ('duplication', 'exact_pairs', 'test', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('duplication', 'normalized_pairs', 'production', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('duplication', 'normalized_pairs', 'test', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('independence', 'cycle', 'production', 5.0, 5.0, 9, 5, 4, 0, 1, 0),
        ('independence', 'cycle', 'test', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('independence', 'layer', 'production', None, None, 0, 0, 0, 0, None, None),
        ('independence', 'layer', 'test', None, None, 0, 0, 0, 0, None, None),
        ('intent', 'debt_lines', 'production', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('intent', 'debt_lines', 'test', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('intent', 'has_doc', 'production', 17.0, 20.0, 0, 0, 0, 0, 4, 1),
        ('intent', 'has_doc', 'test', 4.0, 4.0, 0, 0, 0, 0, 0, 0),
        ('interface', 'parameters', 'production', 3.0, 5.0, 0, 0, 0, 0, 2, 0),
        ('interface', 'parameters', 'test', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('interface', 'visibility', 'production', None, None, 0, 0, 0, 0, None, None),
        ('interface', 'visibility', 'test', None, None, 0, 0, 0, 0, None, None),
        ('size', 'loc', 'production', 108.0, 114.0, 3, 2, 1, 0, 4, 1),
        ('size', 'loc', 'test', 19.0, 19.0, 0, 0, 0, 0, 0, 0),
        ('unreferenced', 'reference_status', 'production', 6.0, 5.0, 3, 2, 1, 0, 1, 1),
        ('unreferenced', 'reference_status', 'test', 0.0, 0.0, 0, 0, 0, 0, 0, 0),
        ('unreferenced', 'references', 'production', None, None, 6, 0, 0, 6, None, None),
        ('unreferenced', 'references', 'test', None, None, 0, 0, 0, 0, None, None),
        ('duplication', 'clone pairs (exact)', 'production', 1.0, 0.0, 0, 0, 0, None, 0, 1),
        ('coupling', 'crate edges', 'production', 1.0, 1.0, 1, 0, 1, None, 0, 0),
        ('coupling', 'crate edges', 'test', 1.0, 1.0, 1, 0, 0, None, 0, 0),
        ('coupling', 'module edges', 'production', 4.0, 4.0, 3, 0, 1, None, 1, 1),
        ('coupling', 'module edges', 'test', 2.0, 2.0, 2, 0, 0, None, 0, 0),
        ('independence', 'module cycles', 'production', 1.0, 1.0, 0, 0, 0, 0, 1, 1),
        ('independence', 'layer violations', 'production', 0.0, 1.0, 0, 0, 0, None, 1, 0),
    ],
    'query units': [
        ('kept', 'production', 'shapes', 'function', 'big', 'shapes/src/lib.rs', None, True, False, 9, 3, -6, ['loc 9 -> 3']),
        ('appeared', 'production', 'shapes', 'function', 'part_a', 'shapes/src/lib.rs', None, None, False, None, 5, None, None),
        ('appeared', 'production', 'shapes', 'function', 'part_b', 'shapes/src/lib.rs', None, None, False, None, 5, None, None),
        ('kept', 'production', 'shapes', 'function', 'dup_b', 'shapes/src/lib.rs', None, True, False, 7, 3, -4, ['clone_pairs 1 -> 0', 'exact_pairs 1 -> 0', 'loc 7 -> 3']),
        ('kept', 'production', 'shapes', 'module', 'ui', 'shapes/src/ui.rs', None, True, False, 3, 6, 3, ['cycle NULL -> shapes/src/core.rs', 'fan_in_files 0 -> 1', 'loc 3 -> 6', 'references 1 -> 2']),
        ('appeared', 'production', 'shapes', 'function', 'new_name', 'shapes/src/lib.rs', None, None, False, None, 3, None, None),
        ('appeared', 'production', 'shapes', 'function', 'width', 'shapes/src/ui.rs', None, None, False, None, 3, None, None),
        ('disappeared', 'production', 'shapes', 'function', 'old_name', 'shapes/src/lib.rs', None, None, False, 3, None, None, None),
        ('kept', 'production', 'shapes', 'function', 'base', 'shapes/src/a.rs', None, False, False, 3, 3, 0, ['cycle shapes/src/a.rs -> NULL', 'fan_in_files 1 -> 0', 'reference_status referenced -> unreferenced', 'references 1 -> 0']),
        ('kept', 'production', 'shapes', 'function', 'dup_a', 'shapes/src/lib.rs', None, False, False, 7, 7, 0, ['clone_pairs 1 -> 0', 'exact_pairs 1 -> 0', 'reference_status unreferenced -> referenced', 'references 0 -> 1']),
        ('kept', 'production', 'shapes', 'function', 'stable', 'shapes/src/lib.rs', None, False, False, 3, 3, 0, ['fan_in_crates 0 -> 1', 'fan_in_files 0 -> 1', 'reference_status unreferenced -> referenced', 'references 0 -> 1']),
        ('kept', 'production', 'shapes', 'module', 'a', 'shapes/src/a.rs', None, False, False, 6, 6, 0, ['cycle shapes/src/a.rs -> NULL', 'fan_in_files 1 -> 0', 'references 2 -> 1']),
        ('kept', 'production', 'shapes', 'function', 'pong', 'shapes/src/b.rs', None, True, False, 3, 3, 0, ['cycle shapes/src/a.rs -> NULL', 'fan_out_files 1 -> 0']),
        ('kept', 'production', 'shapes', 'function', 'value', 'shapes/src/core.rs', None, True, False, 3, 3, 0, ['cycle NULL -> shapes/src/core.rs', 'fan_out_files 0 -> 1']),
        ('kept', 'production', 'shapes', 'module', 'b', 'shapes/src/b.rs', None, True, False, 3, 3, 0, ['cycle shapes/src/a.rs -> NULL', 'fan_out_files 1 -> 0']),
        ('kept', 'production', 'shapes', 'module', 'core', 'shapes/src/core.rs', None, True, False, 3, 3, 0, ['cycle NULL -> shapes/src/core.rs', 'fan_out_files 0 -> 1']),
        ('kept', 'production', 'shapes', 'function', 'ping', 'shapes/src/a.rs', None, False, False, 3, 3, 0, ['cycle shapes/src/a.rs -> NULL']),
        ('kept', 'production', 'shapes', 'function', 'show', 'shapes/src/ui.rs', None, False, False, 3, 3, 0, ['cycle NULL -> shapes/src/core.rs']),
        ('kept', 'production', 'shapes', 'module', 'shapes', 'shapes/src/lib.rs', None, True, False, 42, 42, 0, ['references 6 -> 7']),
        ('kept', 'production', 'app', 'function', 'main', 'app/src/main.rs', None, True, False, 3, 3, 0, None),
        ('kept', 'production', 'app', 'module', 'app', 'app/src/main.rs', None, True, True, 3, 3, 0, None),
        ('kept', 'production', 'shapes', 'constant', 'LIMIT', 'shapes/src/lib.rs', None, True, False, 1, 1, 0, None),
    ],
    'query unit_axes': [
        ('production', 'coupling', 'fan_in_crates', 'shapes', 'function', 'stable', '0', '1', 1.0, 'regressed', False),
        ('production', 'coupling', 'fan_in_files', 'shapes', 'function', 'base', '1', '0', -1.0, 'improved', False),
        ('production', 'coupling', 'fan_in_files', 'shapes', 'module', 'a', '1', '0', -1.0, 'improved', False),
        ('production', 'coupling', 'fan_in_files', 'shapes', 'function', 'stable', '0', '1', 1.0, 'regressed', False),
        ('production', 'coupling', 'fan_in_files', 'shapes', 'module', 'ui', '0', '1', 1.0, 'regressed', True),
        ('production', 'coupling', 'fan_out_files', 'shapes', 'function', 'pong', '1', '0', -1.0, 'improved', True),
        ('production', 'coupling', 'fan_out_files', 'shapes', 'module', 'b', '1', '0', -1.0, 'improved', True),
        ('production', 'coupling', 'fan_out_files', 'shapes', 'function', 'value', '0', '1', 1.0, 'regressed', True),
        ('production', 'coupling', 'fan_out_files', 'shapes', 'module', 'core', '0', '1', 1.0, 'regressed', True),
        ('production', 'duplication', 'clone_pairs', 'shapes', 'function', 'dup_a', '1', '0', -1.0, 'improved', False),
        ('production', 'duplication', 'clone_pairs', 'shapes', 'function', 'dup_b', '1', '0', -1.0, 'improved', True),
        ('production', 'duplication', 'exact_pairs', 'shapes', 'function', 'dup_a', '1', '0', -1.0, 'improved', False),
        ('production', 'duplication', 'exact_pairs', 'shapes', 'function', 'dup_b', '1', '0', -1.0, 'improved', True),
        ('production', 'independence', 'cycle', 'shapes', 'function', 'base', 'shapes/src/a.rs', None, None, 'improved', False),
        ('production', 'independence', 'cycle', 'shapes', 'function', 'ping', 'shapes/src/a.rs', None, None, 'improved', False),
        ('production', 'independence', 'cycle', 'shapes', 'function', 'pong', 'shapes/src/a.rs', None, None, 'improved', True),
        ('production', 'independence', 'cycle', 'shapes', 'module', 'a', 'shapes/src/a.rs', None, None, 'improved', False),
        ('production', 'independence', 'cycle', 'shapes', 'module', 'b', 'shapes/src/a.rs', None, None, 'improved', True),
        ('production', 'independence', 'cycle', 'shapes', 'function', 'value', None, 'shapes/src/core.rs', None, 'regressed', True),
        ('production', 'independence', 'cycle', 'shapes', 'function', 'show', None, 'shapes/src/core.rs', None, 'regressed', False),
        ('production', 'independence', 'cycle', 'shapes', 'module', 'core', None, 'shapes/src/core.rs', None, 'regressed', True),
        ('production', 'independence', 'cycle', 'shapes', 'module', 'ui', None, 'shapes/src/core.rs', None, 'regressed', True),
        ('production', 'size', 'loc', 'shapes', 'function', 'big', '9', '3', -6.0, 'improved', True),
        ('production', 'size', 'loc', 'shapes', 'function', 'dup_b', '7', '3', -4.0, 'improved', True),
        ('production', 'size', 'loc', 'shapes', 'module', 'ui', '3', '6', 3.0, 'regressed', True),
        ('production', 'unreferenced', 'reference_status', 'shapes', 'function', 'dup_a', 'unreferenced', 'referenced', None, 'improved', False),
        ('production', 'unreferenced', 'reference_status', 'shapes', 'function', 'stable', 'unreferenced', 'referenced', None, 'improved', False),
        ('production', 'unreferenced', 'reference_status', 'shapes', 'function', 'base', 'referenced', 'unreferenced', None, 'regressed', False),
        ('production', 'unreferenced', 'references', 'shapes', 'function', 'base', '1', '0', -1.0, None, False),
        ('production', 'unreferenced', 'references', 'shapes', 'function', 'dup_a', '0', '1', 1.0, None, False),
        ('production', 'unreferenced', 'references', 'shapes', 'function', 'stable', '0', '1', 1.0, None, False),
        ('production', 'unreferenced', 'references', 'shapes', 'module', 'a', '2', '1', -1.0, None, False),
        ('production', 'unreferenced', 'references', 'shapes', 'module', 'shapes', '6', '7', 1.0, None, True),
        ('production', 'unreferenced', 'references', 'shapes', 'module', 'ui', '1', '2', 1.0, None, True),
    ],
    'query clones': [
        ('disappeared', 'production', 'exact', 'dup_a', 'shapes/src/lib.rs', 'dup_b', 'shapes/src/lib.rs', 1, 0, -1, 5, 0),
    ],
    'query edges': [
        ('appeared', 'production', 'module', 'shapes/src/core.rs', 'shapes/src/ui.rs', False, 0, 1, 1, 0, 1, 0, 0, 0, 1),
        ('disappeared', 'production', 'module', 'shapes/src/b.rs', 'shapes/src/a.rs', False, 1, 0, -1, 1, 0, 0, 0, 1, 0),
        ('kept', 'production', 'module', 'app/src/main.rs', 'shapes/src/lib.rs', False, 1, 2, 1, 1, 2, 0, 0, 1, 2),
        ('kept', 'production', 'crate', 'app', 'shapes', None, 2, 3, 1, 1, 2, 1, 1, 2, 3),
    ],
    'query cycles': [
        ('appeared', 'production', 'shapes', None, 'shapes/src/core.rs', 0, 2, 2, ['shapes/src/core.rs', 'shapes/src/ui.rs'], [], 0, 2, False, False),
        ('disappeared', 'production', 'shapes', 'shapes/src/a.rs', None, 2, 0, -2, [], ['shapes/src/a.rs', 'shapes/src/b.rs'], 2, 0, False, False),
    ],
    'query layer_violations': [
        ('appeared', 'production', 'core', 'ui', 'shapes/src/core.rs', 'shapes/src/ui.rs', 0, 1, 1),
    ],
}


if __name__ == "__main__":
    check_compare("--print" in sys.argv)
