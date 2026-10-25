# Code database — design

A repository's structure and history as DuckDB tables, queried in SQL, to
locate refactoring candidates with the signals the literature supports
(function-level change history, clone pairs that changed inconsistently,
clone sites classified by their syntactic context, high fan-in combined
with high churn — see
[the survey](research/code-quality-analysis-trends-2026-10-24.md)). It is
language-independent: Rust is the first language, Go and TypeScript are
intended. The owner's starting idea (2026-10-24) was to store the syntax
tree and the dependency graph as they are and query them with DuckDB.

## Principle (owner decision, 2026-10-24)

**Facts come from table functions; everything derived is a view;
materializing is a cache, not a build step.** The owner confirmed this
after two earlier shapes (a Python builder that loaded tables, and a SQL
build script that did the same) were judged not to use what DuckDB is
for. DuckDB's strength is querying sources where they live, lazily, with
projection and filter pushdown and parallel scans; a code database built
that way has no build, only `LOAD` and views. Fixed baselines (the audit's
before/after comparisons) are taken with `CREATE TABLE AS` or Parquet
exports when wanted; several repositories are compared by `ATTACH`.

Consequences:

- A table function returns raw facts and nothing else: commits, file
  changes, hunks, syntax nodes, clone pairs, SCIP occurrences. It does not
  decide what a "unit" or a "test" is.
- Unit definitions, tags, unit changes, churn, co-change, clone
  consistency and fan-in are SQL views or table macros over those facts.
  Language-specific knowledge lives only in the per-language unit and tag
  views.
- A table function is expected to stream, run in parallel and honor
  pushdown where the source allows it; computing everything at init on
  one thread is a known shortcoming of the first `git_history`, not the
  target.

## Components and decisions so far

| Layer | Decision | Record |
| --- | --- | --- |
| Extension route | Rust on the DuckDB C API v2 (official `extension-template-rs`, `duckdb-neo`), binaries pinned to DuckDB 2.0.0; accepted 2026-10-24 | [extension options](research/duckdb-extension-options-2026-10-24.md) |
| Placement | `duckdb-ext/`, a Cargo sub-workspace of this repository excluded from the root workspace (like `preview-plugin/`) | `duckdb-ext/README.md` |
| Git facts | Own extension `git_history` (`git_commits`, `git_file_changes`, `git_hunks`) on gitoxide; chosen over libgit2 plus ported git logic (GPL) after measuring parity with `git log`: commits and file changes exact, `-U0` hunk placement differs in about 0.4 % of rows with no effect found on unit attribution | [gix record](research/gix-parity-2026-10-24.md) |
| Syntax facts | The community extension `sitting_duck` (`read_ast`): its node rows matched an independent tree-sitter parse of all 515 Rust files exactly, and SQL views over them reproduced the reference units and tags exactly | [evaluation](research/sitting-duck-evaluation-2026-10-24.md) |
| Clone facts | jscpd's JSON report read with the core `read_json` (no extension) | — |
| Reference facts | SCIP indexes (`rust-analyzer scip`, `scip-go`, `scip-typescript`) read by our second extension `scip_index` (chosen 2026-10-25 over the `scip` CLI's JSON) | [SCIP record](research/code-db-scip-2026-10-25.md) |

## First increment: views only (2026-10-24)

`duckdb-ext/sql/` holds the views and the first two questions as plain
SQL files read in order by any DuckDB 2.0.0 client (`00_load.sql` with
parameters as variables, `views/`, `lang/rust.sql`, `queries/`), plus
`check.py` (fixture, counts, timings, CLI comparison) and `jscpd.sh`. No
Rust changed. Measured on this repository
([record](research/code-db-views-2026-10-24.md)): every reference to a
view over `read_ast` re-scans it (11–17 scans per query, no pushdown), so
live queries over units and tags take 6–18 s; with `nodes`, `units` and
`tags` materialized (about 8 s to build) the same queries take under
0.1 s, and with `commits`/`file_changes` materialized (1–2 s) the
co-change queries drop from 2–3 s to 0.02 s. `nodes`, `hunks` and
`clone_pairs` are cheap live for a single reference. Which layers to
cache, where the cache lives and when it is refreshed is the open "cache"
knob of the principle above. `glob()` reads no `.gitignore` and descends
hidden directories, so the file set needs explicit include and exclude
patterns (a tracked-files table function is the obvious fact to add).

## Second increment: one scan per query, session cache, tracked files (2026-10-24/25)

The owner chose the order "single-scan views → tracked files → session
cache → incremental cache". Done so far
([record](research/code-db-views-single-scan-2026-10-24.md)):

- `units` and `tags` are each one statement whose node scan is a
  `MATERIALIZED` CTE, which DuckDB 2.0.0 honors inside nested views: one
  `read_ast` per query instead of 11–17. Live on this repository: units
  0.7 s, tags 0.9–1.0 s, the clone-site queries 1.0 s (from 13–14 s). The
  earlier slowness of `tags` over a node table was not `sitting_duck` but
  text slicing: hex-encoding each whole file once per identifier row; the
  fix slices BLOB bytes. Two conventions follow from the first drafts
  (one peaked at 44 GB): join text spans, never whole nodes, to file
  contents, and keep spans in their own `MATERIALIZED` CTE.
- `cache.sql` materializes nodes, units, tags, commits, file_changes and
  hunks into temporary tables and re-points the views at them (about 3 s
  to build, 3.2 GB peak; every query then runs in 0.02–0.12 s). Re-reading
  the view files returns to live; with a database file the re-pointed
  views persist and must be re-read in the next session.
- `git_tracked_files(repo, include_untracked)` is the fourth `git_history`
  function: the worktree's files with their current content ids, matching
  `git ls-files` and `git hash-object` on this repository in about 6 ms.

## Third increment: tracked files and a persistent incremental cache (2026-10-25)

([record](research/code-db-incremental-cache-2026-10-25.md)) The file
set now comes from `git_tracked_files` (regular files whose extension is
in the `languages` map; `include_untracked` optional), so the `glob()`
caveats are gone and `files` carries each file's `blob_id`. `refresh.sql`
keeps a per-repository cache file (`target/code-db/cache.duckdb`,
attached as `cache`): `nodes`, `units` and `tags` are stored per file and
only the files whose `(path, blob_id)` changed are re-parsed and
re-derived (one `read_ast` over the changed paths; the per-language views
take a `scope_paths` variable); history is appended with
`since := <cached head>` and rebuilt when that head is no longer in the
range; a `meta` row records keys (sitting_duck version, the derived
views' text, history parameters) whose change forces the matching
rebuild, and an interrupted refresh rebuilds everything. Reproduced on a
throwaway clone of this repository: cold 3.7 s (2.1 GB peak), nothing
changed 0.14–0.17 s, one edited file 0.20–0.24 s, ten files 0.24–0.26 s,
one commit of three files 0.25 s, an amended head 2.8–3.3 s (history
rebuilt); every refreshed view equals its live counterpart row for row;
queries from the cache run in 0.02–0.08 s; the cache file is about 50 MB
and does not compact deleted rows. The session cache (`cache.sql`) stays
for one-off use; the two refuse to be mixed.

## Fourth increment: historical blobs and unit changes (2026-10-25)

([record](research/code-db-unit-changes-2026-10-25.md)) The owner chose
the interim route "a table function that writes blob files" (option B)
over "content as a column parsed by `parse_ast_list`" (option A): A
returns no byte offsets, and the unit names depend on byte slicing, so the
history's units would not match the current ones. `git_changed_blobs`
(fifth `git_history` function) writes each changed regular file's old and
new blob once as `<dir>/<blob_id>.<ext>` and returns the (commit, path,
side, blob) rows; `read_ast` then reads history through the same path as
current files, and the unit rules — now a table macro over named node,
bytes and lines relations — derive `blob_units` with the same SQL (for
every current file in range, the blob's units equal `units`). The
`unit_changes` view applies the spike's hunk-range rules and reproduced
its rows exactly on the same hunks (40,731 rows on this repository);
`unit_churn` and `clone_change_consistency` return the spike record's
top rows. Blobs are immutable, so the blob layer of the cache is
append-only. Reproduced on this worktree: cold refresh 27.6 s (blob
parsing 19.5 s for 6,586 blobs and 31.4 M nodes, peak about 7 GB), then
0.4 s with nothing changed; the cache file grows to about 600 MB. Live
(uncached) history queries take 14–20 s at 8–9 GB and are not meant to
be run that way.

## Fifth increment: SCIP references and fan-in (2026-10-25)

([record](research/code-db-scip-2026-10-25.md)) The owner chose to read
SCIP indexes with a Rust table function rather than through the `scip`
CLI's JSON, after the downsides were laid out (schema tracking by hand,
one more version-pinned extension, decisions fixed in Rust, the CLI still
needed as a test reference, index generation external either way). The
second extension `scip_index` (`read_scip`, `read_scip_symbols`,
`read_scip_metadata`; a dependency-free wire-format decoder of about 160
lines citing `scip.proto` at scip-code/scip v0.10.0; documents streamed
one at a time) matched `scip print --json` on three indexes with zero
differences and reads this repository's 30 MB index in 0.07 s at 11 MiB
peak (the JSON route: 59 MB of JSON, 0.4 s to convert, 0.3–0.5 s and
420–470 MiB to load with `read_json`; both were measured, the table
function won on every number). `views/scip.sql` shapes the facts
(1-based lines and columns, `local N` symbols qualified by document,
`is_reference` = not definition since rust-analyzer 1.99.1 sets only the
Definition bit) and is optional in the file order; `unit_fan_in_churn`
reproduced the spike's 2,915 rows and top entries; the cache keys the
SCIP layer by the index file's content hash (0.02 s to hash) and rebuilds
it whole on change. The index itself is still produced outside DuckDB
(`rust-analyzer scip`, 160–280 s on this repository at 7 GB).

All three literature-backed signals (unit-level churn, inconsistent clone
changes, fan-in × churn) are now computable from the cache in under
0.2 s each.

## Open points

- **A `git://` file system** remains the target for history access. The
  C API registers none today: both v1 and v2 only let an extension *use*
  the host file system (checked against the 2.0.0 headers), and
  registration is being added to v2 in duckdb/duckdb#25916 (open). When it
  lands, `git_changed_blobs` and the blob directory go away and
  `read_ast` reads `git://` paths directly.
- **Unit identity across commits** is `(path, kind, qualified name)`;
  renames and moves start a new history. No established system solves this
  with stored identifiers; method-level history tools treat it as a
  matching problem (90–99.5 % on Java).
- **Unit granularity**: nested units (impl blocks, modules) are credited
  with every change inside them; the views keep the nesting (`parent`
  columns) so the choice can be made per query.
- **How the views are loaded** (SQL files read by the client today;
  registration by an extension is not available on the C API v2 without a
  catalog surface), and whether `duckdb-ext/` is extracted for use in
  other repositories. The DuckDB 2.0.0 CLI is installed as
  `~/.local/bin/duckdb2` for interactive use.
- **Go and TypeScript** adapters exist only in the spike's record; the
  per-language SQL (`lang/<language>.sql`) and the SCIP indexers
  (`scip-go`, `scip-typescript`) are the pieces to add.

## History

A Python spike that built these tables (2026-10-24) answered the
feasibility question in 22 s on this repository and was removed the same
day because its form had not been agreed with the owner
([spike record](research/code-db-spike-2026-10-24.md); code in history at
475599d5). The measurements it produced remain valid as a reference for
the views.
