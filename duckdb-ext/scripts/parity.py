"""Compare the extension's rows with a code database built by scripts/code-db.

uv run --with duckdb==2.0.0 python duckdb-ext/scripts/parity.py EXTENSION REPO SPIKE_DB [SUFFIX]

Each table is compared as a multiset (EXCEPT ALL in both directions);
git_hunks is filtered to paths ending in SUFFIX (default `.rs`), which is
what the spike's `--languages rust` recorded.
"""

import sys

import duckdb

extension, repo, spike = sys.argv[1:4]
suffix = sys.argv[4] if len(sys.argv) > 4 else ".rs"
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.sql(f"LOAD '{extension}'")
con.sql(f"ATTACH '{spike}' AS spike (READ_ONLY)")
con.execute("CREATE TEMP TABLE ext_hunks AS SELECT * FROM git_hunks(?) WHERE path LIKE '%' || ?", [repo, suffix]).fetchall()
con.execute("CREATE TEMP TABLE ext_commits AS SELECT * FROM git_commits(?)", [repo]).fetchall()
con.execute("CREATE TEMP TABLE ext_file_changes AS SELECT * FROM git_file_changes(?)", [repo]).fetchall()

failed = False
for name in ["hunks", "commits", "file_changes"]:
    ours, theirs = f"ext_{name}", f"spike.{name}"
    n_ours = con.sql(f"SELECT count(*) FROM {ours}").fetchall()[0][0]
    n_theirs = con.sql(f"SELECT count(*) FROM {theirs}").fetchall()[0][0]
    only_ours = con.sql(f"SELECT * FROM {ours} EXCEPT ALL SELECT * FROM {theirs}").fetchall()
    only_theirs = con.sql(f"SELECT * FROM {theirs} EXCEPT ALL SELECT * FROM {ours}").fetchall()
    print(f"{name}: extension {n_ours} rows, spike {n_theirs} rows, "
          f"only in extension {len(only_ours)}, only in spike {len(only_theirs)}")
    for label, rows in [("  extension only:", only_ours), ("  spike only:", only_theirs)]:
        if rows:
            print(label)
            for row in sorted(rows)[:20]:
                print("   ", row)
    failed |= bool(only_ours or only_theirs)
sys.exit(1 if failed else 0)
