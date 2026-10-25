# Code database views

A repository's syntax, history and clone reports as SQL views over table
functions, with no build step (`docs/code-database-design.md`, principle of
2026-10-24): facts come from `read_ast` (community extension
`sitting_duck`), `git_tracked_files`/`git_commits`/`git_file_changes`/`git_hunks`
(this workspace's `git_history`, `../README.md`) and jscpd's JSON read with
the core `read_json`; everything else is a view. Nothing is stored: every
query reads the sources again, unless the client also reads `cache.sql`,
which copies the expensive layers into temporary tables for the session, or
`refresh.sql`, which keeps them in a database file and updates only what
changed (both below).

Measurements of what this costs on this repository are in
`docs/research/code-db-views-2026-10-24.md` (first increment),
`docs/research/code-db-views-single-scan-2026-10-24.md` (one `read_ast` scan
per query, the session cache) and
`docs/research/code-db-incremental-cache-2026-10-25.md` (tracked files, the
persistent cache).

## Running

Every client needs DuckDB **2.0.0** (git_history loads only into that
version), unsigned extensions allowed, network access the first time (to
`INSTALL sitting_duck`), and the git_history binary built
(`duckdb-ext/build.sh`). Run from the root of this Horizon checkout:
`00_load.sql` loads git_history by a path relative to it. The analyzed
repository is a variable and can be anywhere.

The files are executed in this order:

```
duckdb-ext/sql/00_load.sql
duckdb-ext/sql/views/files.sql
duckdb-ext/sql/views/nodes.sql
duckdb-ext/sql/lang/rust.sql
duckdb-ext/sql/views/units.sql
duckdb-ext/sql/views/history.sql
duckdb-ext/sql/views/clones.sql
```

`views/clones.sql` needs the jscpd reports to exist (below); the others do
not read any source when they are executed, except that `00_load.sql` takes
the file snapshot from `git_tracked_files`. Either `duckdb-ext/sql/cache.sql`
or `duckdb-ext/sql/refresh.sql` may follow them (see Cache).

DuckDB CLI 2.0.0 (here `~/.local/bin/duckdb2`; the system `duckdb` is
1.5.0 and refuses git_history):

```sh
duckdb-ext/sql/jscpd.sh . target/code-db/jscpd src crates preview-plugin/src
duckdb2 -unsigned -cmd "SET VARIABLE repo = '$PWD'"
```
```
.read duckdb-ext/sql/00_load.sql
.read duckdb-ext/sql/views/files.sql
.read duckdb-ext/sql/views/nodes.sql
.read duckdb-ext/sql/lang/rust.sql
.read duckdb-ext/sql/views/units.sql
.read duckdb-ext/sql/views/history.sql
.read duckdb-ext/sql/views/clones.sql
SET VARIABLE min_coupling_pct = 50;
.read duckdb-ext/sql/queries/file_cochange.sql
```

When stdout is not a terminal and `AI_AGENT` is set, the 2.0.0 CLI switches
to an "agent mode" with Markdown tables; `-no-agent` turns it off. The CLI
goes on after a failing statement unless started with `-bail` (or after
`.bail on`); the checks in `cache.sql` and `refresh.sql` stop the file only
then.

## Cache

Two ways to stop recomputing the expensive layers (`nodes`, `units`,
`tags`, `commits`, `file_changes`, `hunks`); every other view and every
query keeps its text and reads whichever is in place.

| | `cache.sql` (session) | `refresh.sql` (persistent) |
| --- | --- | --- |
| Where | temporary tables, gone with the session | `cache_path`, a DuckDB file, attached as `cache` |
| Build | everything, about 3 s on this repository | everything the first time (about 3.5 s); then only the files whose blob id changed and the new commits (0.1–0.7 s for nothing to 10 files or one commit; a rewritten history about 2.2–2.9 s) |
| A changed file | not seen until the view files and `cache.sql` are read again | re-parsed by the next `refresh.sql` |
| Use for | one session of queries over a fixed state | repeated sessions, or a session that refreshes as the repository changes |

They are not mixed: `cache.sql` refuses to run while the views read the
persistent cache, and `refresh.sql` refuses while they read the session
copies. Re-reading the view files resets both to live.

### Session cache

`cache.sql`, read after the view files, copies `nodes`, `units`, `tags`,
`commits`, `file_changes` and `hunks` into temporary tables `<view>_cache`
and replaces each of those views with `SELECT * FROM <view>_cache`. Every
other view and every query keeps its text and reads the copies; `units` and
`tags` are built from `nodes_cache`.

- **Frozen**: the syntax and text of the source files, `languages`, and the
  history for the `since` and `include_merges` set when `cache.sql` was
  read. Query parameters still apply at every query.
- **Live again**: re-read the view files, which replace the views with their
  live definitions (the tables stay until the next `cache.sql` or the end of
  the session).
- **Refresh**: the view files, then `cache.sql`. Reading `cache.sql` twice
  without the view files in between fails (`nodes_cache` is dropped while
  `nodes` still reads it) rather than copying the copies. Re-reading
  `00_load.sql` renumbers `file_id`, so refresh after it.
- Temporary tables named like the views would shadow them (DuckDB resolves
  `temp` before `main`), but re-reading a view file would then leave the
  shadow in place, so the views are replaced instead. With a database file,
  the replaced views persist and point at tables the next session does not
  have: read the view files in every session.

### Persistent cache

`refresh.sql`, read after the view files (in any session, as often as
wanted), brings the database file `cache_path` (default
`<repo>/target/code-db/cache.duckdb`; the directory must exist, ATTACH does
not create it) up to date and points the six views at it:

1. **File snapshot**: `source_files`/`source_paths` are taken again from
   `git_tracked_files`, as `00_load.sql` does.
2. **Syntax**: a file is *changed* when its `(path, blob_id, language)` is
   not in `cache.nodes_files`, *removed* when its cached path is no longer a
   source file. The rows of changed and removed paths are deleted from
   `cache.nodes`, and one `read_ast` over the changed paths inserts their
   nodes. A rename is a removal plus an addition (paths are keys).
3. **Units and tags**: their rows of the changed and removed paths are
   deleted, and `derived_units`/`derived_tags` (the language views, one scan
   each) are computed for the changed paths only, by setting `scope_paths`.
   unit ids are kept as positions within the file (`unit_ord`,
   `parent_unit_ord`).
4. **History**: when the cached head is among `git_commits(repo, since)`, the
   commits `cached_head..HEAD` are appended to `cache.commits`,
   `cache.file_changes` and `cache.hunks` (`since := cached_head`);
   otherwise (history rewritten, branch reset or switched) the three tables
   are rebuilt.
5. `CHECKPOINT cache`, and the `meta` row.

With nothing changed, `read_ast` is still called, because it refuses an
empty list when it is bound and no `CASE` around the call can avoid that:
the call gets one path that does not exist (`<cache_path>.no-such-file.rs`)
with `ignore_errors` on, reads no file and returns no rows, and the insert
keeps only rows of the changed paths in any case. `ignore_errors` is off
whenever there are files to parse. A split into two files run
conditionally would have needed a runner; this keeps `refresh.sql` one file
for every client (the CLI's `.read` included).

The views read the cache at query time: `file_id` comes from joining each
row's path to `files`, and only rows whose cached blob id is the file's
current one appear; `unit_id` is the row number over `(file_id, unit_ord)`
and `parent_unit_id` is derived from it. After a refresh every view equals
its live counterpart row for row (`check.py fixture`, `check.py refresh`).

Layout of the file (schema version 1):

| Table | Rows |
| --- | --- |
| `meta` | one: `schema_version`, `repo`, `state` (`ok`, or `refreshing` while a refresh runs), `built_at`, `refreshed_at`, `syntax_key` (sitting_duck version), `derive_key` (md5 of the derived views' text), `history_key` (`since`, `include_merges`, `languages`), `head`; the last refresh: `syntax`/`derived` (`rebuilt` or `incremental`), `history` (`rebuilt` or `appended`), `files_added`, `files_changed`, `files_removed`, `files_unchanged`, `nodes_parsed`, `units_inserted`, `tags_inserted`, `commits_added`, `seconds` (per layer: `files`, `nodes`, `units`, `tags`, `commits`, `file_changes`, `hunks`, `checkpoint`, `total`) |
| `nodes_files` | one per cached file: `path`, `blob_id`, `language` |
| `nodes` | `path` plus `read_ast`'s columns (as in `views/nodes.sql`) |
| `units` | `path`, `unit_ord`, `parent_unit_ord`, then the columns of `units` without the ids |
| `tags` | `path`, then the columns of `tags` without `file_id` |
| `commits`, `file_changes`, `hunks` | the columns of the views |

What forces a rebuild:

- **Syntax, units and tags of every file**: no `meta` row (a new file);
  `state` not `ok` (an interrupted refresh: statements run one by one, with
  no transaction, so that `now()` gives the per-layer times); a different
  `repo` (`nodes.file_path` is absolute); a different sitting_duck version.
- **Units and tags of every file** (no re-parse): a change in the text of
  `scope_files`, `sources`, `source_bytes`, `source_lines` or any view named
  `*_units`/`*_tags`. A change of the macros `byte_slice`/`normalize_ws` is
  not detected (reading `duckdb_functions()` would cost about 18 ms of
  every refresh): delete the file after changing them.
- **History**: a different `since`, `include_merges` or `languages`; a
  cached head that is not among the commits of `since..HEAD`; no cached
  head (no commits).
- **A different schema version** is an error: delete the file.

Limits:

- The history is cached for the `since` and `include_merges` of the last
  refresh; changing them takes effect at the next refresh (a rebuild).
- `files.blob_id` and the parsed content are read at different moments: a
  file edited during a refresh can have its new syntax cached under the
  blob id of the snapshot (the next refresh sees a different blob id and
  re-parses it). HEAD moving during a
  refresh can likewise leave the three history tables one commit apart.
- Re-reading `00_load.sql` after a refresh takes a new file snapshot while
  the views still read the cache: files whose blob id changed lose their
  rows until the next refresh, and new files have none.
- Deleted rows are not compacted: the file grows with every re-parse until
  DuckDB reuses the freed blocks.
- `scope_paths` is set and reset by `refresh.sql`; a refresh that fails in
  between leaves it set, and the live `units`/`tags` then cover only those
  paths until it is reset (`SET VARIABLE scope_paths = NULL`) or the next
  refresh ends.

Python (`duckdb==2.0.0`):

```python
import duckdb, pathlib
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("SET VARIABLE repo = '/abs/path/to/repo'")
for f in ["00_load.sql", "views/files.sql", "views/nodes.sql", "lang/rust.sql",
          "views/units.sql", "views/history.sql", "views/clones.sql"]:
    con.execute(pathlib.Path("duckdb-ext/sql", f).read_text())
con.execute(pathlib.Path("duckdb-ext/sql/refresh.sql").read_text())  # optional
print(con.sql(pathlib.Path("duckdb-ext/sql/queries/file_cochange.sql").read_text()))
```

Rust (`duckdb` crate linked against the machine's `libduckdb` 2.0.0, as in
`../host-check`): `Config::default().allow_unsigned_extensions()`, then
`execute_batch` each file's text in the same order (a file holds several
statements). This route is not exercised by `check.py`.

## Check

```sh
uv run --locked --script duckdb-ext/sql/check.py fixture          # assertions on a generated fixture
uv run --locked --script duckdb-ext/sql/check.py repo .           # counts and every query on a repository
uv run --locked --script duckdb-ext/sql/check.py refresh .        # refresh.sql twice, every view compared with live
uv run --locked --script duckdb-ext/sql/check.py cli .            # the same files through the CLI's .read, live and refreshed
uv run --locked --script duckdb-ext/sql/check.py measure . --runs 3  # live and cached, and the cache build
uv run --locked --script duckdb-ext/sql/check.py measure-refresh CLONE --runs 3  # refresh.sql's cases (edits and commits CLONE)
uv run --locked --script duckdb-ext/sql/check.py explain .        # read_ast/read_text scans per plan
```

`refresh`, `cli` and `measure-refresh` write the cache to
`REPO/target/code-db/cache.duckdb`. `measure-refresh` edits, commits to and
resets the repository it is given, so give it a throwaway clone (`git clone
-q . target/code-db/clone`, and `jscpd.sh` for its reports).

`fixture` builds a git repository in a temporary directory (a trait, two
trait impls, an inline test module, an integration test, an untracked file,
a build output and a hidden directory that `.gitignore` excludes, four
commits, one of them touching only non-Rust files) and a report in jscpd
5.3.1's JSON shape, then asserts the rows of `files` (with
`include_untracked`, and the blob id git records), `units` (kinds,
qualified names, lines, `loc`, parents), `tags`, the three history views,
`clone_pairs` and all four queries, including both match modes, a `since`
change and a file edit seen without reloading. It then reads `cache.sql` and
asserts the same rows from the copies, no source scan in a query's plan, an
edit and a `since` change not seen while cached, the failure of a second
`cache.sql`, and live rows again after the view files are re-read.

On a second fixture it reads `refresh.sql` after each of these steps and
asserts every refreshed view equal to live row for row, the four queries
equal, and the `meta` statistics: the first refresh from an empty cache;
nothing changed (nothing parsed or appended, under 100 ms in the same
session); one file edited (only its rows replaced, every other path's rows
unchanged); a file added, deleted, renamed; a new commit (history
appended); an amended commit (history rebuilt); an untracked file toggled
by `include_untracked`; an interrupted refresh (rebuilt); `cache.sql` and
`refresh.sql` refusing each other's views; and the cache file in a new
session (nothing rebuilt, the view files reset it to live). It needs `git`
but no jscpd.

## Layout

| File | Defines |
| --- | --- |
| `00_load.sql` | `INSTALL`/`LOAD` of both extensions; parameter defaults; `tracked_source_files(path, mode, blob_id)`; the snapshot `source_files`/`source_paths` |
| `views/files.sql` | `files(file_id, path, language, mode, blob_id)`, `scope_files`, `sources(file_id, content)`, `source_bytes(file_id, bytes)`, `source_lines(file_id, line, nonblank_through)`, macros `byte_slice`, `normalize_ws` |
| `views/nodes.sql` | `nodes`: `file_id` plus every column of `read_ast(…, source := 'full', peek := 'none', context := 'none')` |
| `lang/rust.sql` | `rust_units`, `rust_tags(file_id, node_id, tag, start_line, end_line)`; the intermediate steps are common table expressions inside them |
| `views/units.sql` | `derived_units`, `derived_tags`: the union over languages (Rust only today); `units`, `tags` over them |
| `views/history.sql` | `commits`, `file_changes`, `hunks` |
| `views/clones.sql` | `clone_pairs(pair_id, source, file_a, start_line_a, end_line_a, file_b, start_line_b, end_line_b, tokens, lines)` |
| `queries/*.sql` | one statement each: `file_cochange`, `file_cochange_thresholds`, `clone_site_tags`, `clone_site_tags_summary`; the header states the question, the parameters and the evidence |
| `jscpd.sh` | the two jscpd reports `clones.sql` reads |
| `cache.sql` | the session cache (above) |
| `refresh.sql` | the persistent cache (above) |
| `check.py` | the check above |

`views/units.sql` is an addition to the layout first proposed: the union of
the per-language views has to be read after every `lang/*.sql`, so it cannot
live in any of them.

## Conventions

- **Lines are 1-based and inclusive** everywhere (`start_line`, `end_line`),
  as in sitting_duck, git's hunk headers and jscpd. Columns are 1-based byte
  columns; bytes are 0-based with an exclusive end. A node's `end_line` is
  the line of its end point, so a line comment (which includes its newline)
  ends at column 1 of the next line.
- **Paths are repository-relative** with `/`. `files.file_id` is the
  position in the sorted file list.
- **Neutral vocabulary**: unit kinds `function`, `method`, `impl`, `type`,
  `module`, `macro`; tags `test`, `generated_or_macro`, and Rust's
  `trait_impl`. `qualified_name` joins enclosing unit names with `.`;
  `parent_unit_id` is the innermost enclosing unit; `loc` counts non-blank
  lines. Tags are not inherited: a range is tagged when a tagged node of the
  same file contains it.
- **Language knowledge lives only in `lang/`.**
- **One `read_ast` scan per view.** DuckDB expands a view, and a common table
  expression that is not `MATERIALIZED`, at every reference, and every
  expansion of `nodes` runs `read_ast` over all files again. A view that
  needs the nodes more than once reads `nodes` once into a `MATERIALIZED`
  expression and builds its steps as further expressions of the same
  statement, not as views of their own; steps referenced more than once are
  `MATERIALIZED` too. `check.py explain` counts the scans.
- **Slice source text in a step of its own.** `byte_slice(bytes, s, e)`
  takes `source_bytes.bytes`; join the bytes only to a `MATERIALIZED` set of
  spans. In a join of more inputs the planner may join the bytes to every
  node of the file first (one row per node carrying the file's bytes; a
  draft of `rust_units` peaked at 7 GB live and 44 GB over a node table in
  this repository).
- **`tags` carries the tagged node's lines**, so a query that needs only
  lines does not join `nodes` (a second scan).
- **A language view covers `scope_files`.** It reads `nodes` restricted to
  the files of `scope_files` (`getvariable('scope_paths') IS NULL OR file_id
  IN (SELECT file_id FROM scope_files)`, the NULL test first so that it
  folds away live), takes file-level facts from `scope_files` rather than
  `files`, and reads text through `sources`, which is restricted the same
  way: a file's rows depend on that file alone. `unit_id` numbers units in
  `file_id` order, so a file's units are consecutive. `refresh.sql` relies
  on both.

## Parameters

DuckDB variables (`SET VARIABLE name = value`). A view or query reads them
when it runs, so a change applies to the next query, except the file list,
which `00_load.sql` (and `refresh.sql`) take as a snapshot.

| Variable | Default | Read by |
| --- | --- | --- |
| `repo` | required; an absolute path | everything |
| `include_untracked` | `false` (also list untracked files that are not ignored) | `00_load.sql`, `refresh.sql` |
| `exclude_patterns` | `[]` (GLOB operator patterns on the relative path; `*` crosses `/`) | `00_load.sql`, `refresh.sql` |
| `languages` | `MAP {'rs': 'rust'}` (extension to language) | `00_load.sql`, `files`, `hunks`, `refresh.sql` |
| `since` | unset (all of HEAD) | history views, `refresh.sql` |
| `include_merges` | `false` | history views, co-change queries, `refresh.sql` |
| `clone_reports` | `[repo/target/code-db/jscpd/exact/jscpd-report.json, repo/target/code-db/jscpd/normalized/jscpd-report.json]` | `clone_pairs` |
| `cache_path` | `repo/target/code-db/cache.duckdb` | `refresh.sql` |
| `scope_paths` | NULL (all files); set only by `refresh.sql` | `scope_files` |
| `min_shared`, `max_commit_files`, `min_coupling_pct`, `basis` | 3, 50, 30, `'max'` | `file_cochange` (the first two also `file_cochange_thresholds`) |
| `match_mode`, `clone_source` | `'contain'`, NULL (all reports) | `clone_site_tags` (`match_mode` also the summary) |

The source files are the regular files (mode `100644` or `100755`) that
`git_tracked_files` lists, with an extension in `languages`: ignored files,
files inside nested repositories (other clones, worktrees placed inside the
repository) and files deleted in the worktree never appear, symlinks and
submodules are skipped. `refresh.sql`'s variables named `refresh_*` are its
own intermediate values.

How parameters reach the table functions: `getvariable('…')` is accepted
as an argument of `read_ast`, `read_text`, `read_json`, `glob`, `ATTACH` and
the four git functions, including their named parameters, inside a view,
and is evaluated when the view is scanned. `LOAD` takes only a literal, so
the git_history path is fixed in `00_load.sql`. A list-valued variable
cannot be empty where a table function reads it (`read_ast` refuses an
empty list), and `read_json` must find the reports when `clones.sql` is
executed. A named parameter given an expression rather than a variable can
be parsed as a positional argument (`ignore_errors := len(x) = 0` is
`(ignore_errors := len(x)) = 0`): pass a variable.

## Deliberately not here yet

- **Unit changes per commit** (needs each changed file's syntax at each
  commit; open point in the design document).
- **SCIP**, Go and TypeScript, clone groups (the spike's `group_id`), and
  any query beyond the four ported ones.
- **Per-layer choice.** Both caches hold all six layers; `clone_pairs`
  stays live in both.
