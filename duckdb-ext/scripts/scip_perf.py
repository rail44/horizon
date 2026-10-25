"""Time one query over a SCIP index in a fresh process and report peak RSS.

uv run --with duckdb==2.0.0 python duckdb-ext/scripts/scip_perf.py EXTENSION INDEX FUNCTION count|table

`count` runs `SELECT count(*) FROM FUNCTION(INDEX)`, `table` runs
`CREATE TABLE t AS SELECT * FROM FUNCTION(INDEX)` in an in-memory database.
"""

import resource
import sys
import time

import duckdb

extension, index, function, mode = sys.argv[1:5]
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.sql(f"LOAD '{extension}'")
base = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
start = time.perf_counter()
if mode == "count":
    rows = con.execute(f"SELECT count(*) FROM {function}(?)", [index]).fetchall()[0][0]
else:
    con.execute(f"CREATE TABLE t AS SELECT * FROM {function}(?)", [index])
    rows = con.execute("SELECT count(*) FROM t").fetchall()[0][0]
elapsed = time.perf_counter() - start
peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
print(
    f"{function} {mode}: {rows} rows, {elapsed:.3f} s, "
    f"peak RSS {peak / 1024:.0f} MiB (after LOAD: {base / 1024:.0f} MiB)"
)
