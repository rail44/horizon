"""Load the built extension into Python DuckDB and query each function.

uv run --with duckdb==2.0.0 python duckdb-ext/scripts/load_smoke.py EXTENSION REPO
"""

import sys
import time

import duckdb

extension, repo = sys.argv[1], sys.argv[2]
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
print("PRAGMA version:", con.sql("PRAGMA version").fetchall())
con.sql(f"LOAD '{extension}'")
print(con.sql(f"DESCRIBE SELECT * FROM git_hunks('{repo}')"))
for function in ["git_hunks", "git_commits", "git_file_changes"]:
    start = time.perf_counter()
    count = con.execute(f"SELECT count(*) FROM {function}(?)", [repo]).fetchone()[0]
    print(f"{function}: {count} rows in {time.perf_counter() - start:.2f} s")
print(con.execute("SELECT * FROM git_commits(?) LIMIT 3", [repo]))
print(con.execute("SELECT * FROM git_hunks(?) LIMIT 3", [repo]).fetchall())
