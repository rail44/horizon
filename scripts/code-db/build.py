#!/usr/bin/env python3
# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#     "duckdb==2.0.0",
#     "tree-sitter==0.25.2",
#     "tree-sitter-go==0.25.0",
#     "tree-sitter-rust==0.24.2",
#     "tree-sitter-typescript==0.23.2",
# ]
# ///
"""Build and query a language-neutral DuckDB code database (syntax, history, clones, SCIP)."""

import argparse
import csv
from datetime import date, datetime, timezone
from decimal import Decimal
import hashlib
import importlib.metadata
import json
from pathlib import Path
import platform
import re
import resource
import shlex
import subprocess
import sys
import tempfile
import time
import unittest

import duckdb

import clones
import history
import languages
import scip
import syntax

HERE = Path(__file__).resolve().parent
QUERIES = HERE / "queries"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def git_blob_sha(data):
    return hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()


def bulk_insert(con, table, rows, scratch):
    """Load rows through a CSV file; far faster than executemany for millions of rows."""
    path = Path(scratch) / f"{table}.csv"
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle, quoting=csv.QUOTE_STRINGS)
        count = 0
        for row in rows:
            writer.writerow(row)
            count += 1
    if count:
        con.execute(
            f"COPY {table} FROM '{path.as_posix()}' "
            "(FORMAT csv, HEADER false, QUOTE '\"', ESCAPE '\"', allow_quoted_nulls false)"
        )
    path.unlink()
    return count


def source_files(root, adapters):
    names = history.git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard").decode().split("\0")
    selected = []
    for name in sorted(set(names)):
        if not name or syntax.grammar_for(name, adapters) is None:
            continue
        path = root / name
        if path.is_symlink() or not path.is_file():
            continue
        selected.append(name)
    return selected


def load_syntax(con, root, adapters, scratch):
    node_rows, unit_rows, tag_rows, file_rows = [], [], [], []
    unit_id = 0
    for file_id, name in enumerate(source_files(root, adapters)):
        data = (root / name).read_bytes()
        g = syntax.grammar_for(name, adapters)
        tree = g.parse(data)
        file_rows.append((file_id, name, g.adapter.name, len(data), git_blob_sha(data), tree.root_node.has_error))
        rows, ids = syntax.nodes(tree, file_id)
        node_rows.extend(rows)
        first = unit_id
        for unit in syntax.units(g, tree, data):
            parent = None if unit["parent"] is None else first + unit["parent"]
            unit_rows.append((unit_id, file_id, ids[unit["node"].id], parent, unit["kind"], unit["name"],
                              unit["qualified_name"], unit["start_row"], unit["end_row"], unit["loc"]))
            unit_id += 1
        tag_rows.extend(sorted((file_id, ids[node], tag) for node, tag in syntax.tags(g, tree, name)))
    counts = {}
    for table, rows in (("files", file_rows), ("nodes", node_rows), ("units", unit_rows), ("tags", tag_rows)):
        counts[table] = bulk_insert(con, table, rows, scratch)
    return {name: file_id for file_id, name, *_ in file_rows}


def utc(seconds):
    return datetime.fromtimestamp(seconds, timezone.utc).strftime("%Y-%m-%d %H:%M:%S")


def load_history(con, root, adapters, since, include_merges, scratch, info):
    commits, changes, raw = history.commits_and_changes(root, since, include_merges)
    bulk_insert(con, "commits", ((s, utc(t), a, subj, p, m) for s, t, a, subj, p, m in commits), scratch)
    bulk_insert(con, "file_changes", changes, scratch)
    pathspecs = sorted({"*" + ext for adapter in adapters for ext in adapter.grammars})
    hunk_rows = history.hunks(root, pathspecs, since, include_merges)
    bulk_insert(con, "hunks", hunk_rows, scratch)
    cache = history.UnitCache(root, adapters)
    try:
        rows = history.unit_changes(raw, hunk_rows, cache)
    finally:
        cache.reader.close()
    bulk_insert(con, "unit_changes", rows, scratch)
    info["unit_changes.parse_lookups"] = cache.lookups
    info["unit_changes.parse_cache_hits"] = cache.hits
    info["unit_changes.blobs_parsed"] = len(cache.cache)


def load_scip(con, path, file_ids, scratch, info):
    documents, external = scip.read(path)
    symbols = {}
    occurrences = []
    skipped = 0
    for doc in documents:
        file_id = file_ids.get(doc["path"])
        for item in doc["symbols"]:
            symbols.setdefault(local_symbol(item["symbol"], doc["path"]), item)
        if file_id is None:
            skipped += 1
            continue
        for occ in doc["occurrences"]:
            if not occ["symbol"]:
                continue
            roles = occ["roles"]
            is_definition = bool(roles & scip.DEFINITION)
            occurrences.append((file_id, local_symbol(occ["symbol"], doc["path"]), *occ["range"],
                                is_definition, not is_definition, bool(roles & scip.IMPORT)))
    for item in external:
        symbols.setdefault(item["symbol"], item)
    bulk_insert(con, "scip_symbols", ((s, i["kind"], i["display_name"]) for s, i in symbols.items()), scratch)
    bulk_insert(con, "scip_occurrences", occurrences, scratch)
    info["scip.documents"] = len(documents)
    info["scip.documents_not_in_files"] = skipped


def local_symbol(symbol, path):
    """SCIP local symbols are only unique within a document; qualify them with its path."""
    return f"{symbol} {path}" if symbol.startswith("local ") else symbol


def build(args):
    started = time.monotonic()
    root = Path(history.git(Path(args.repo), "rev-parse", "--show-toplevel").decode().strip()).resolve()
    adapters = languages.select([name.strip() for name in args.languages.split(",") if name.strip()])
    out = Path(args.out).resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    for stale in (out, out.with_name(out.name + ".wal")):
        stale.unlink(missing_ok=True)
    info = {
        "git_head": history.git(root, "rev-parse", "HEAD").decode().strip(),
        "git_dirty": str(bool(history.git(root, "status", "--porcelain=v1").strip())).lower(),
        "repo_root": str(root),
        "command_line": shlex.join(sys.argv),
        "built_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "python_version": platform.python_version(),
        "duckdb_version": duckdb.__version__,
        "git_version": subprocess.run(["git", "--version"], capture_output=True, text=True).stdout.strip(),
        "languages": ",".join(a.name for a in adapters),
        "since": args.since or "",
        "include_merges": str(args.include_merges).lower(),
        "package.tree-sitter": importlib.metadata.version("tree-sitter"),
    }
    for adapter in adapters:
        for package in adapter.packages:
            info["package." + package] = importlib.metadata.version(package)
    for source in sorted(HERE.glob("*.py")) + [HERE / "schema.sql"]:
        info["implementation_sha256." + source.name] = sha256(source.read_bytes())
    con = duckdb.connect(str(out))
    with tempfile.TemporaryDirectory(prefix="code-db-") as scratch:
        con.execute((HERE / "schema.sql").read_text())
        phase = time.monotonic()
        file_ids = load_syntax(con, root, adapters, scratch)
        info["seconds.syntax"] = round(time.monotonic() - phase, 1)
        phase = time.monotonic()
        load_history(con, root, adapters, args.since, args.include_merges, scratch, info)
        info["seconds.history"] = round(time.monotonic() - phase, 1)
        phase = time.monotonic()
        if args.clones:
            bulk_insert(con, "clone_pairs", clones.load(args.clones, root, set(file_ids)), scratch)
            for label, path in clones.parse_specs(args.clones):
                info[f"clones.{label}"] = f"{path} sha256={sha256(Path(path).read_bytes())}"
        if args.scip:
            load_scip(con, Path(args.scip), file_ids, scratch, info)
            info["scip.index"] = f"{args.scip} sha256={sha256(Path(args.scip).read_bytes())}"
        info["seconds.clones_scip"] = round(time.monotonic() - phase, 1)
    tables = [r[0] for r in con.execute("SELECT table_name FROM information_schema.tables ORDER BY 1").fetchall()]
    for table in tables:
        if table != "build_info":
            info[f"rows.{table}"] = con.execute(f"SELECT count(*) FROM {table}").fetchone()[0]
    info["seconds.total"] = round(time.monotonic() - started, 1)
    own = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    info["peak_rss_kib.python"] = own
    con.executemany("INSERT INTO build_info VALUES (?, ?)", [(k, str(v)) for k, v in sorted(info.items())])
    con.execute("CHECKPOINT")
    con.close()
    for key in sorted(info):
        if key.startswith(("rows.", "seconds.", "peak_", "unit_changes.", "scip.documents")):
            print(f"{key}: {info[key]}")
    return 0


PARAM = re.compile(r"^--\s*@param\s+(\w+)\s+(.*?)\s*$")


def literal(text):
    if text.lower() == "null":
        return None
    for kind in (int, float):
        try:
            return kind(text)
        except ValueError:
            pass
    return text


def query_file(name):
    path = Path(name)
    if path.suffix == ".sql" and path.exists():
        return path
    path = QUERIES / f"{name}.sql"
    if not path.exists():
        known = sorted(p.stem for p in QUERIES.glob("*.sql"))
        raise ValueError(f"Unknown query {name!r}; known: {known}")
    return path


def run_query(database, name, params=(), limit=None):
    """Execute one query file with its declared defaults overridden by params; return (columns, rows)."""
    sql = query_file(name).read_text()
    declared = {m[1]: literal(m[2]) for line in sql.splitlines() if (m := PARAM.match(line))}
    for item in params:
        key, sep, value = item.partition("=")
        if not sep or key not in declared:
            raise ValueError(f"Unknown or malformed --param {item!r}; declared: {sorted(declared)}")
        declared[key] = literal(value)
    used = set(re.findall(r"\$(\w+)", re.sub(r"--[^\n]*", "", sql)))
    con = duckdb.connect(str(database), read_only=True)
    try:
        cursor = con.execute(sql, {k: v for k, v in declared.items() if k in used})
        columns = [d[0] for d in cursor.description]
        rows = cursor.fetchmany(limit) if limit else cursor.fetchall()
    finally:
        con.close()
    return columns, rows


def plain(value):
    if isinstance(value, (datetime, date)):
        return value.isoformat()
    if isinstance(value, Decimal):
        return float(value)
    if isinstance(value, (list, tuple)):
        return [plain(v) for v in value]
    return value


def render(columns, rows, fmt, out):
    if fmt == "json":
        json.dump([{c: plain(v) for c, v in zip(columns, row)} for row in rows], out, ensure_ascii=False, indent=1)
        out.write("\n")
    elif fmt == "csv":
        writer = csv.writer(out)
        writer.writerow(columns)
        writer.writerows([[json.dumps(plain(v)) if isinstance(v, (list, tuple)) else plain(v) for v in row] for row in rows])
    else:
        cells = [[("" if v is None else str(plain(v))) for v in row] for row in rows]
        widths = [max([len(c)] + [len(r[i]) for r in cells]) for i, c in enumerate(columns)]
        out.write("  ".join(c.ljust(w) for c, w in zip(columns, widths)).rstrip() + "\n")
        for r in cells:
            out.write("  ".join(v.ljust(w) for v, w in zip(r, widths)).rstrip() + "\n")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    b = commands.add_parser("build", help="Build the database from scratch")
    b.add_argument("repo")
    b.add_argument("--out", required=True)
    b.add_argument("--languages", default=",".join(languages.ADAPTERS))
    b.add_argument("--clones", nargs="+", default=[], metavar="[LABEL=]REPORT")
    b.add_argument("--scip", help="SCIP index (protobuf) or `scip print --json` output")
    b.add_argument("--since", help="Only history in SINCE..HEAD")
    b.add_argument("--include-merges", action="store_true", help="Record merge diffs against the first parent")
    q = commands.add_parser("query", help="Run a named query or a .sql file")
    q.add_argument("database")
    q.add_argument("query")
    q.add_argument("--param", action="append", default=[], metavar="NAME=VALUE")
    q.add_argument("--format", choices=["table", "json", "csv"], default="table")
    q.add_argument("--limit", type=int)
    commands.add_parser("verify", help="Run the unittest suite")
    args = parser.parse_args(argv)
    try:
        if args.command == "build":
            return build(args)
        if args.command == "query":
            columns, rows = run_query(args.database, args.query, args.param, args.limit)
            render(columns, rows, args.format, sys.stdout)
            return 0
        suite = unittest.defaultTestLoader.discover(str(HERE / "tests"), top_level_dir=str(HERE))
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
    except (ValueError, OSError, subprocess.CalledProcessError, duckdb.Error) as error:
        print(f"code-db: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
