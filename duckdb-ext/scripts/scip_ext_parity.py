"""Compare the scip_index extension's rows with scip-probe's TSV, in order.

uv run --with duckdb==2.0.0 python duckdb-ext/scripts/scip_ext_parity.py EXTENSION PROBE INDEX

The rows are formatted as scip-probe prints them (see its module doc), without the
extension's trailing index_path.
"""

import json
import subprocess
import sys

import duckdb

FUNCTIONS = {
    "occurrences": "read_scip",
    "symbols": "read_scip_symbols",
    "metadata": "read_scip_metadata",
}


def cell(value):
    if value is None:
        return "\\N"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, list):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)[1:-1]
    return str(value)


def main():
    extension, probe, index = sys.argv[1:4]
    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.sql(f"LOAD '{extension}'")
    failed = False
    for row_set, function in FUNCTIONS.items():
        expected = subprocess.run(
            [probe, index, row_set], check=True, capture_output=True, text=True
        ).stdout.splitlines()
        literal = "'" + index.replace("'", "''") + "'"
        result = con.execute(f"SELECT * EXCLUDE (index_path) FROM {function}({literal})")
        actual = []
        while batch := result.fetchmany(100_000):
            actual.extend("\t".join(cell(value) for value in row) for row in batch)
        same = actual == expected
        failed |= not same
        print(f"{function}: {len(actual)} rows, probe {len(expected)} rows, identical in order: {same}")
        if not same:
            for a, e in zip(actual, expected):
                if a != e:
                    print(f"  first difference:\n  extension {a!r}\n  probe     {e!r}")
                    break
    sys.exit(1 if failed else 0)


main()
