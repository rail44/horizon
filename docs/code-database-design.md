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
| Reference facts | SCIP (`rust-analyzer scip`, `scip-go`, `scip-typescript`); reader form undecided: `scip print --json` with `read_json`, or a small Rust `read_scip` | — |

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

## Open points

- **Historical blobs for `read_ast`.** Unit changes per commit need the
  syntax of each changed file at that commit. The DuckDB-native way is a
  `git://<path>@<rev>` file system so that `read_ast`, `read_text` and
  `read_json` all read history; `duck_tails` registers one through the C++
  API. The C API registers none today: both v1 and v2 only let an extension
  *use* the host file system (checked against the 2.0.0 headers), and
  registration is being added to v2 in duckdb/duckdb#25916 (open). Until
  then the candidates are an interim table function returning blob
  contents (parsed with `sitting_duck`'s `parse_ast_list`, line ranges
  only), a C++ file-system extension, or `duck_tails` once its 2.0 binary
  exists.
- **Unit identity across commits** is `(path, kind, qualified name)`;
  renames and moves start a new history. No established system solves this
  with stored identifiers; method-level history tools treat it as a
  matching problem (90–99.5 % on Java).
- **Unit granularity**: nested units (impl blocks, modules) are credited
  with every change inside them; the views keep the nesting (`parent`
  columns) so the choice can be made per query.
- **Where the views live and how they are loaded** (SQL files read by the
  client, or registered by an extension), the DuckDB 2.0 client for
  interactive use (the system CLI is 1.5.0), and whether `duckdb-ext/` is
  extracted for use in other repositories.

## History

A Python spike that built these tables (2026-10-24) answered the
feasibility question in 22 s on this repository and was removed the same
day because its form had not been agreed with the owner
([spike record](research/code-db-spike-2026-10-24.md); code in history at
475599d5). The measurements it produced remain valid as a reference for
the views.
