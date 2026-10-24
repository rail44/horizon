"""Time one function over a repository in a fresh process and report peak RSS.

uv run --with duckdb==2.0.0 python duckdb-ext/scripts/perf.py EXTENSION REPO FUNCTION [RUNS]
"""

import resource
import sys
import time

import duckdb

extension, repo, function = sys.argv[1:4]
runs = int(sys.argv[4]) if len(sys.argv) > 4 else 3
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.sql(f"LOAD '{extension}'")
base = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
for _ in range(runs):
    start = time.perf_counter()
    rows = con.execute(f"SELECT count(*) FROM {function}(?)", [repo]).fetchall()[0][0]
    print(f"{function}: {rows} rows, {time.perf_counter() - start:.2f} s")
peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
print(f"peak RSS {peak / 1024:.0f} MiB (after LOAD: {base / 1024:.0f} MiB)")
