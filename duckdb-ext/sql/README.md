# Code database views

A repository's syntax, history and clone reports as SQL views over table
functions, with no build step (`docs/code-database-design.md`, principle of
2026-10-24): facts come from `read_ast` (community extension
`sitting_duck`), `git_tracked_files`/`git_commits`/`git_file_changes`/`git_hunks`
and `git_changed_blobs` (this workspace's `git_history`, `../README.md`),
`read_scip`/`read_scip_symbols`/`read_scip_metadata` (this workspace's
`scip_index`, over a rust-analyzer SCIP index) and jscpd's JSON read with the
core `read_json`; everything else is a view. Nothing is stored: every
query reads the sources again, unless the client also reads `cache.sql`,
which copies the expensive layers into temporary tables for the session, or
`refresh.sql`, which keeps them in a database file and updates only what
changed (both below).

Measurements of what this costs on this repository are in
`docs/research/code-db-views-2026-10-24.md` (first increment),
`docs/research/code-db-views-single-scan-2026-10-24.md` (one `read_ast` scan
per query, the session cache) and
`docs/research/code-db-incremental-cache-2026-10-25.md` (tracked files, the
persistent cache), `docs/research/code-db-unit-changes-2026-10-25.md` (the
blob layer, `unit_changes` and the two history-joined queries) and
`docs/research/code-db-scip-2026-10-25.md` (the SCIP views, `unit_fan_in_churn`
and their cache).

## Running

Every client needs DuckDB **2.0.0** (git_history loads only into that
version), unsigned extensions allowed, network access the first time (to
`INSTALL sitting_duck`), and the git_history and scip_index binaries built
(`duckdb-ext/build.sh` builds both). Run from the root of this Horizon checkout:
`00_load.sql` loads both by paths relative to it. The analyzed
repository is a variable and can be anywhere.

The files are executed in this order:

```
duckdb-ext/sql/00_load.sql
duckdb-ext/sql/views/files.sql
duckdb-ext/sql/views/nodes.sql
duckdb-ext/sql/views/blobs.sql
duckdb-ext/sql/lang/rust.sql
duckdb-ext/sql/views/units.sql
duckdb-ext/sql/views/history.sql
duckdb-ext/sql/views/unit_changes.sql
duckdb-ext/sql/views/clones.sql
duckdb-ext/sql/views/scip.sql      (optional)
```

`views/scip.sql` is optional and needs the SCIP index to exist (see SCIP);
nothing in the other files depends on it, and `cache.sql` and `refresh.sql`
work with or without it. `views/clones.sql` needs the jscpd reports to exist (below); the others do
not read any source when they are executed, except that `00_load.sql` takes
the file snapshot from `git_tracked_files` and `views/blobs.sql` takes the
blob snapshot from `git_changed_blobs`, which writes the blob files that are
missing (see "Historical blobs"). `views/blobs.sql` comes before
`lang/rust.sql` because the language views are bound against its views. Either `duckdb-ext/sql/cache.sql`
or `duckdb-ext/sql/refresh.sql` may follow them (see Cache).

DuckDB CLI 2.0.0 (here `~/.local/bin/duckdb2`; the system `duckdb` is
1.5.0 and refuses git_history):

```sh
duckdb-ext/sql/jscpd.sh . target/code-db/jscpd src crates preview-plugin/src
rust-analyzer scip . --output target/code-db/index.scip   # for views/scip.sql
duckdb2 -unsigned -cmd "SET VARIABLE repo = '$PWD'"
```
```
.read duckdb-ext/sql/00_load.sql
.read duckdb-ext/sql/views/files.sql
.read duckdb-ext/sql/views/nodes.sql
.read duckdb-ext/sql/views/blobs.sql
.read duckdb-ext/sql/lang/rust.sql
.read duckdb-ext/sql/views/units.sql
.read duckdb-ext/sql/views/history.sql
.read duckdb-ext/sql/views/unit_changes.sql
.read duckdb-ext/sql/views/clones.sql
.read duckdb-ext/sql/views/scip.sql
SET VARIABLE min_coupling_pct = 50;
.read duckdb-ext/sql/queries/file_cochange.sql
```

When stdout is not a terminal and `AI_AGENT` is set, the 2.0.0 CLI switches
to an "agent mode" with Markdown tables; `-no-agent` turns it off. The CLI
goes on after a failing statement unless started with `-bail` (or after
`.bail on`); the checks in `cache.sql` and `refresh.sql` stop the file only
then.

## Historical blobs and unit changes

The syntax of a file at a past commit is the syntax of its blob. The owner
chose (2026-10-25) to write historical blobs to files, so that `read_ast`,
`read_text` and the per-language rules apply to history unchanged.

- **`blob_files(sha, path, old_path, side, blob_id, mode, file)`** is
  `git_changed_blobs(repo, blob_dir, since, include_merges, extensions)`:
  the (commit, file change) pairs of `git_file_changes` for regular files,
  one row per side that has a blob (`side` `old` or `new`; no `old` row for
  an addition, no `new` row for a deletion), `path`/`old_path` as in
  `file_changes`, and `file` = `<blob_dir>/<blob_id>.<ext>` (the extension
  of that side's path), written by the function when it is missing.
  Changes with the same content on both sides (a pure rename, a mode
  change), binary sides and symlink or submodule sides have no rows.
  `extensions` is the keys of `languages`; `blob_dir` must be absolute.
- **`blob_nodes(blob_id, …)`** is one `read_ast` over the distinct blob
  files, with the parameters of `nodes`: one parse per blob, however many
  commits reference it. The language is what `read_ast` detects from the
  file's extension, the extension of the path the blob was recorded under.
- **`blob_units(blob_id, unit_ord, parent_unit_ord, node_id, kind, name,
  qualified_name, start_line, end_line, loc)`** applies the same rules as
  `units`: `lang/rust.sql` defines them once, as the table macro
  `rust_units_over(nodes_t, bytes_t, lines_t)` over relations named by its
  arguments (read with `query_table`), and applies it to the current files
  (`rust_units`) and to the blobs (`rust_blob_units`). The text views
  `source_bytes` and `source_lines` are likewise the macros
  `source_bytes_of`/`source_lines_of` over `sources` or `blob_sources`.
  `unit_ord` is the unit's position within its blob (what `unit_id` is
  within a file), so a blob's rows do not depend on the other blobs. For
  every current file whose blob is in `blob_files`, its blob's units equal
  its `units` row for row apart from the ids (`check.py fixture`, and on
  this repository). Blobs have no tags yet.
- **`unit_changes(sha, path, unit_kind, unit_qualified_name,
  change_kind)`**: for each `(sha, path)` with hunks, the units of the new
  side's blob whose lines `[start_line, end_line]` intersect a hunk's new
  range `[new_start, new_start + new_lines - 1]` are `modified` when the old
  side has a unit with the same `(kind, qualified_name)`, else `added`; the
  units of the old side's blob that intersect a hunk's old range
  `[old_start, old_start + old_lines - 1]` and are not already classified
  are `modified` when they still exist on the new side (recorded under
  `path`), else `removed` (recorded under `old_path`, or `path`). A hunk
  with `new_lines = 0` (`old_lines = 0`) intersects nothing on that side:
  git's start is then the line before the insertion or deletion. One row per
  distinct tuple. This is the spike's definition (`git show
  475599d5:scripts/code-db/history.py`, `unit_changes`) with 1-based lines.

`views/blobs.sql` takes a snapshot when it is read, as `00_load.sql` does
for the current files: `read_ast` and `read_text` need a constant list of
files that exist when the list is bound, so the file scans `blob_files` once
into `blob_snapshot` (a list of `{blob_id, file}`) and `blob_paths`. That
scan runs `git_changed_blobs` over `since..HEAD`, which exports the missing
blobs. Re-read the file (or `refresh.sql`) after new commits. With no blob in
the range, the list holds one file that does not exist (`ignore_errors` for
`read_ast`; `read_text` returns no row for a missing file).

Limits:

- **Unit identity is `(kind, qualified_name)` within a path.** Two units
  with the same kind and name in one blob (two `impl Foo` blocks) are one
  unit; a rename or move starts a new history under the new path, and the
  queries do not follow it.
- **Nested units** are credited with every change inside them (an impl
  with each change to one of its methods).
- **Placement**: `-U0` hunks are placed by gitoxide; where an insertion or
  deletion could sit at several lines, the unit it is attributed to follows
  that placement (`docs/research/gix-parity-2026-10-24.md` §12).
- **Merges** follow `include_merges`; without it, content introduced by a
  merge (a conflict resolution) is never a new side, so a current file
  whose blob comes from a merge has no blob units.
- A blob recorded under paths with different extensions would be one key
  with two files; with one language this cannot occur.
- Live, `blob_nodes` parses every blob of the range (on this repository
  6,586 blobs, 152 MB, 31 million nodes), so a live `blob_units` takes
  about 18 s and the two history-joined queries 14–20 s, at a peak RSS of
  about 8 GB; both caches below hold the blob layer.

## SCIP

`views/scip.sql` reads a SCIP index of the repository (`scip_index`, default
`<repo>/target/code-db/index.scip`), written by `rust-analyzer scip .
--output target/code-db/index.scip` from the repository root (160-280 s on
this repository, with many warnings). The functions open the file when the
view file is read, so a missing index fails `views/scip.sql` (and only it).
The index describes the tree it was generated from; nothing checks it
against `files`: regenerate it after edits.

- **`scip_index_occurrences`, `scip_index_symbols`, `scip_index_metadata`**
  are `read_scip`, `read_scip_symbols` and `read_scip_metadata` as they come
  (`../README.md`): 0-based lines and characters, raw `local N` symbols.
- **`scip_occurrences(file_id, document, symbol, start_line, start_column,
  end_line, end_column, roles, is_definition, is_reference, is_import,
  is_write, is_read, is_generated, is_test, is_forward_definition,
  enclosing_start_line, enclosing_start_column, enclosing_end_line,
  enclosing_end_column)`**: lines 1-based and inclusive, columns 1-based with
  the end the column after the last character (as `nodes`), in the unit of
  the document's position encoding (UTF-8 bytes for rust-analyzer).
  `file_id` is `document` joined to `files.path`, NULL for a document that
  is not a source file. `local N` symbols are qualified as `local N
  <document>`. `is_reference` is `NOT is_definition`: rust-analyzer 1.99.1
  sets no role bit but Definition. Occurrences without a symbol are dropped.
- **`scip_symbols(document, symbol, kind, display_name, enclosing_symbol,
  documentation, relationship_count)`**: `read_scip_symbols` with the same
  qualification (of `symbol` and `enclosing_symbol`); `document` is NULL for
  an external symbol. A non-local symbol can be listed under several
  documents (a package's `crate/` symbol under each target's root file),
  one row per listing.
- `scip_document_occurrences` and `scip_document_symbols` are the same rows
  without `file_id`; `cache.sql` and `refresh.sql` copy them and replace
  `scip_occurrences` and `scip_symbols`, never these.

`queries/unit_fan_in_churn.sql` is the spike's query on these views: a
unit's fan-in is the number of distinct files other than its own that hold
a reference to any symbol whose definition occurrence lies within the
unit's lines (a module's whole-file definition lies in no unit; enclosing
units count everything they contain), and its churn is its commits in
`unit_changes`. Parameters `kind`, `min_fan_in` (default 1) and
`include_merges`. On this repository its 2,915 rows equal the spike's code
run on the same tree and index, apart from the commits of two units that
git's and gitoxide's `-U0` placement attribute differently.

## Cache

Two ways to stop recomputing the expensive layers (`nodes`, `units`,
`tags`, `commits`, `file_changes`, `hunks`, the blob layer
`blob_files`, `blob_nodes`, `blob_units`, and `scip_occurrences`,
`scip_symbols`); every other view and every query
keeps its text and reads whichever is in place. `unit_changes` stays a view
in both: over the cached layers a query reading it takes 0.16–0.21 s on
this repository, against 0.05–0.13 s from a materialized copy that itself
takes 0.14–0.20 s to build.

| | `cache.sql` (session) | `refresh.sql` (persistent) |
| --- | --- | --- |
| Where | temporary tables, gone with the session | `cache_path`, a DuckDB file, attached as `cache` |
| Build | everything, 17–20 s on this repository (13–16 s of it parsing the blobs for `blob_units`; 3 s without the blob layer), peak RSS about 9 GB | everything the first time (20–26 s, 14–18 s of it parsing and storing the blobs; peak RSS about 6.8 GB; the file is about 620 MB); then only the files whose blob id changed, the new commits and their new blobs (0.2–0.7 s for nothing to 10 files or one commit; a rewritten history 3.6–4.2 s) |
| A changed file | not seen until the view files and `cache.sql` are read again | re-parsed by the next `refresh.sql` |
| Use for | one session of queries over a fixed state | repeated sessions, or a session that refreshes as the repository changes |

They are not mixed: `cache.sql` refuses to run while the views read the
persistent cache, and `refresh.sql` refuses while they read the session
copies. Re-reading the view files resets both to live.

### Session cache

`cache.sql`, read after the view files, copies `nodes`, `units`, `tags`,
`commits`, `file_changes`, `hunks`, `blob_files` and `blob_units` into
temporary tables `<view>_cache` and replaces each of those views with
`SELECT * FROM <view>_cache`; `scip_occurrences_cache` and
`scip_symbols_cache` are copies of `scip_document_occurrences` and
`scip_document_symbols` (`scip_occurrences` joins `file_id` to the copy at
query time). Without `views/scip.sql`, empty views of the same columns stand
in for those two (they stay until `views/scip.sql` replaces them), so the
SCIP views exist and are empty. Every other view and every query keeps its
text and reads the copies; `units` and `tags` are built from `nodes_cache`.
`blob_nodes` is not copied (about 31 million rows here) and stays live:
building `blob_units_cache` parses every blob once, which is most of the
build time.

- **Frozen**: the syntax and text of the source files, `languages`, and the
  history and blob layer for the `since` and `include_merges` set when
  `cache.sql` was read. Query parameters still apply at every query.
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
not create it) up to date and points the eleven views at it:

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
5. **Blob files**: `git_changed_blobs` with the same `since` as the history
   appends to `cache.blob_files` (and exports the new blobs); the table is
   rebuilt with the history, and also when every blob's units are
   re-derived, since that reads every blob file again and the rebuild
   exports any that are missing.
6. **Blob syntax**: the blobs of `cache.blob_files` that are not in
   `cache.blob_parsed` are parsed, by the live `blob_nodes` of
   `views/blobs.sql` with its snapshot narrowed to them, into
   `cache.blob_nodes`. Blobs never change, so parsed blobs are kept across
   history rebuilds; only a syntax rebuild drops them.
7. **Blob units**: for the blobs of the range not in `cache.blob_derived`,
   `derived_blob_units` is computed over the cached nodes (`blob_nodes`
   re-pointed at the cache, the snapshot narrowed to those blobs) and
   appended to `cache.blob_units`. The snapshot is then set to every blob of
   the range, and `blob_files`, `blob_nodes` and `blob_units` read the
   cache, showing only the blobs of the range (blobs that left it, after an
   amend, stay in the file and are not shown).
8. **SCIP**: `cache.scip_occurrences` and `cache.scip_symbols` (the rows of
   `scip_document_occurrences` and `scip_document_symbols`) are rebuilt whole
   when the key changes: the index's path, `hash()` of its content (read with
   `read_blob`, about 20 ms for 30 MB) and the text of the five views of
   `views/scip.sql` that shape the rows. Otherwise the insert's false
   condition folds to an empty result and the index is not read. Without
   `views/scip.sql` (the key is NULL) the tables are emptied, the same empty
   stand-ins as in `cache.sql` are created, and both views are empty. With
   `views/scip.sql` read and no index, the refresh fails before anything
   changes.
9. `CHECKPOINT cache`, and the `meta` row.

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

Layout of the file (schema version 3):

| Table | Rows |
| --- | --- |
| `meta` | one: `schema_version`, `repo`, `state` (`ok`, or `refreshing` while a refresh runs), `built_at`, `refreshed_at`, `syntax_key` (sitting_duck version), `derive_key` (md5 of the derived views' text), `history_key` (`since`, `include_merges`, `languages`, `blob_dir`), `head`; the last refresh: `syntax`/`derived` (`rebuilt` or `incremental`), `history` and `blob_history` (for `blob_files`; `rebuilt` or `appended`), `files_added`, `files_changed`, `files_removed`, `files_unchanged`, `nodes_parsed`, `units_inserted`, `tags_inserted`, `commits_added`, `blob_rows_added`, `blobs_parsed`, `blob_bytes_parsed`, `blob_nodes_parsed`, `blobs_derived`, `blob_units_inserted`, `blobs_cached` (all parsed blobs in the file), `scip_key` (NULL without `views/scip.sql`), `scip_cache` (`rebuilt` or `kept`), `scip_occurrences_inserted`, `scip_symbols_inserted`, `seconds` (per layer: `files`, `nodes`, `units`, `tags`, `commits`, `file_changes`, `hunks`, `blob_files`, `blob_nodes`, `blob_units`, `scip`, `checkpoint`, `total`) |
| `nodes_files` | one per cached file: `path`, `blob_id`, `language` |
| `nodes` | `path` plus `read_ast`'s columns (as in `views/nodes.sql`) |
| `units` | `path`, `unit_ord`, `parent_unit_ord`, then the columns of `units` without the ids |
| `tags` | `path`, then the columns of `tags` without `file_id` |
| `commits`, `file_changes`, `hunks`, `blob_files` | the columns of the views |
| `blob_parsed` | one per parsed blob: `blob_id`, `bytes` |
| `blob_derived` | one per blob whose units are in `blob_units` (a blob can have none) |
| `blob_nodes` | `blob_id` plus `read_ast`'s columns |
| `blob_units` | the columns of `blob_units` |
| `scip_occurrences`, `scip_symbols` | the columns of `scip_document_occurrences`, `scip_document_symbols` |

What forces a rebuild:

- **Syntax, units and tags of every file**: no `meta` row (a new file);
  `state` not `ok` (an interrupted refresh: statements run one by one, with
  no transaction, so that `now()` gives the per-layer times); a different
  `repo` (`nodes.file_path` is absolute); a different sitting_duck version.
- **Units and tags of every file, units of every blob** (no re-parse): a
  change in the text of `scope_files`, `sources`, `source_bytes`,
  `source_lines`, `scope_nodes`, the four `blob_*` inputs, any view named
  `*_units` (except `blob_units`, which `refresh.sql` replaces) or
  `*_tags`, or of any macro that is not internal (the unit rules are the
  macro `rust_units_over`; `byte_slice` and `normalize_ws` are now covered
  too). Reading the macros through `duckdb_functions()` costs about 18 ms
  of every refresh.
- **Blob nodes**: only with the syntax of every file (above); a history
  rebuild keeps them.
- **History** (and `blob_files`): a different `since`, `include_merges`,
  `languages` or `blob_dir`; a cached head that is not among the commits of
  `since..HEAD`; no cached head (no commits).
- **SCIP**: a different key (another index file or content, a change in
  `views/scip.sql`'s shaping views, `views/scip.sql` read or not), and the
  first two cases of the syntax rebuild (no `meta` row, `state` not `ok`).
  Nothing else depends on it: reading `views/scip.sql` or not rebuilds no
  other layer.
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
  DuckDB reuses the freed blocks. Blobs that leave the range are never
  deleted.
- The blob files under `blob_dir` are read again only when units are
  re-derived (which first re-exports the range); deleting the directory is
  otherwise harmless.
- `scope_paths` is set and reset by `refresh.sql`; a refresh that fails in
  between leaves it set, and the live `units`/`tags` then cover only those
  paths until it is reset (`SET VARIABLE scope_paths = NULL`) or the next
  refresh ends.

Python (`duckdb==2.0.0`):

```python
import duckdb, pathlib
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("SET VARIABLE repo = '/abs/path/to/repo'")
for f in ["00_load.sql", "views/files.sql", "views/nodes.sql", "views/blobs.sql",
          "lang/rust.sql", "views/units.sql", "views/history.sql",
          "views/unit_changes.sql", "views/clones.sql",
          "views/scip.sql"]:  # the last one optional, needs the index
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
uv run --locked --script duckdb-ext/sql/check.py scip-fixture     # only the SCIP fixture
uv run --locked --script duckdb-ext/sql/check.py repo .           # counts and every query on a repository
uv run --locked --script duckdb-ext/sql/check.py refresh .        # refresh.sql twice, every view compared with live
uv run --locked --script duckdb-ext/sql/check.py cli .            # the same files through the CLI's .read, live and refreshed
uv run --locked --script duckdb-ext/sql/check.py measure . --runs 3  # live and cached, and the cache build
uv run --locked --script duckdb-ext/sql/check.py measure-refresh CLONE --runs 3  # refresh.sql's cases (edits and commits CLONE)
uv run --locked --script duckdb-ext/sql/check.py explain .        # read_ast/read_text scans per plan
```

Every command given a repository reads `views/scip.sql` too when
`REPO/target/code-db/index.scip` exists, and then covers the SCIP views and
`unit_fan_in_churn` (`measure` adds the three source views and a baseline,
`measure-refresh` a changed index). `refresh`, `cli` and `measure-refresh` write the cache to
`REPO/target/code-db/cache.duckdb`; every command that reads the view files
writes the blob files to `REPO/target/code-db/blobs` (`git_changed_blobs`). `refresh` compares
`blob_nodes` with live through a per-blob digest (count and sum of row
hashes) rather than row by row: a copy of its 31 million rows plus the
comparison peaked above 30 GB. `measure-refresh` edits, commits to and
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

A second fixture adds six commits to the first one's four: one clone
side edited, then both (by a second author), a unit added, a rename with
its function replaced, a unit removed, and a pure rename. It asserts the
rows of `blob_files` (sides, a rename with an edit, no row for a pure
rename, file names), one parse per distinct
blob, the units of every current file's blob equal to its `units` (modulo
ids), every row of `unit_changes` per commit (`added`, `modified`,
`removed` under the old path of a rename, nothing for README-only and
pure-rename commits), `unit_churn` (commits and authors, `since_time`,
`kind`), `clone_change_consistency` (one side only once, both twice; an
unresolved side; `clone_source`), and `since` narrowing `blob_files` and
`unit_changes`.

A SCIP fixture runs rust-analyzer (`RUST_ANALYZER`, default
`rust-analyzer` on `PATH`; skipped with a message when missing) on a copy of
the first fixture's `src/` and `tests/` with a `Cargo.toml` for the package
`fixture`, into `REPO/target/code-db/index.scip` (about 6 s), and asserts
the metadata, every document joined to its file, 1-based lines and columns
of the trait's definition and its enclosing lines (the unit's), its three
references, local symbols qualified per document (in `scip_symbols`'
`enclosing_symbol` too), the package's `crate/` symbol listed under two
documents, every row of `unit_fan_in_churn` (a function called from the
integration test, the trait named from `rect.rs`; `kind` and `min_fan_in`),
the same rows through `cache.sql` with no index scan in the query's plan, a
document excluded from the snapshot (no `file_id`, its references not
counted), and `cache.sql` without `views/scip.sql` (empty views).

On a third fixture it reads `refresh.sql` after each of these steps and
asserts every refreshed view equal to live row for row (the blob layer
and `unit_changes` included), the six queries equal, and the `meta`
statistics (blob rows, blobs parsed and derived among them): the first refresh from an empty cache;
nothing changed (nothing parsed or appended, under 500 ms in the same
session; 0.14–0.30 s with the blob steps, under a load average of 5–22); one file edited (only its rows replaced, every other path's rows
unchanged); a file added, deleted, renamed; a new commit (history
appended, one new blob parsed); an amended commit (history and
`blob_files` rebuilt, no blob parsed); an untracked file toggled
by `include_untracked`; an interrupted refresh (rebuilt); `views/scip.sql`
read (SCIP rebuilt, no other layer), nothing changed (kept, the insert's plan
reads no index), the index changed (rebuilt) and missing (the refresh fails,
`state` stays `ok`); `cache.sql` and
`refresh.sql` refusing each other's views; and the cache file in a new
session without `views/scip.sql` (SCIP emptied, nothing else rebuilt; the view
files reset it to live). It needs `git` and rust-analyzer but no jscpd.

## Layout

| File | Defines |
| --- | --- |
| `00_load.sql` | `INSTALL`/`LOAD` of the three extensions; parameter defaults; `tracked_source_files(path, mode, blob_id)`; the snapshot `source_files`/`source_paths` |
| `views/files.sql` | `files(file_id, path, language, mode, blob_id)`, `scope_files`, `sources(file_id, content)`, `source_bytes(file_id, bytes)`, `source_lines(file_id, line, nonblank_through)` (over the table macros `source_bytes_of`, `source_lines_of`), macros `byte_slice`, `normalize_ws` |
| `views/nodes.sql` | `nodes`: `file_id` plus every column of `read_ast(…, source := 'full', peek := 'none', context := 'none')`; `scope_nodes` (the nodes of `scope_files`) |
| `views/blobs.sql` | `blob_files`, the snapshot `blob_snapshot`/`blob_paths`, `blob_list(blob_id, file, blob_ord)`, `blob_nodes(blob_id, …)`; the derivation inputs `blob_scope_nodes`, `blob_sources`, `blob_source_bytes`, `blob_source_lines` (keyed by `blob_ord`) |
| `lang/rust.sql` | the table macro `rust_units_over(nodes_t, bytes_t, lines_t)` and its two applications `rust_units`, `rust_blob_units`; `rust_tags(file_id, node_id, tag, start_line, end_line)`; the intermediate steps are common table expressions inside them |
| `views/units.sql` | `derived_units`, `derived_tags`, `derived_blob_units`: the union over languages (Rust only today); `units`, `tags`, `blob_units` over them |
| `views/history.sql` | `commits`, `file_changes`, `hunks` |
| `views/unit_changes.sql` | `unit_changes` |
| `views/clones.sql` | `clone_pairs(pair_id, source, file_a, start_line_a, end_line_a, file_b, start_line_b, end_line_b, tokens, lines)` |
| `views/scip.sql` (optional) | `scip_index_occurrences`, `scip_index_symbols`, `scip_index_metadata`; `scip_document_occurrences`, `scip_document_symbols`; `scip_occurrences`, `scip_symbols` (see SCIP) |
| `queries/*.sql` | one statement each: `file_cochange`, `file_cochange_thresholds`, `clone_site_tags`, `clone_site_tags_summary`, `unit_churn`, `clone_change_consistency`, `unit_fan_in_churn` (needs `views/scip.sql`); the header states the question, the parameters and the evidence |
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
- **A language view covers `scope_files`.** It reads `scope_nodes`, the
  nodes restricted to the files of `scope_files` (`getvariable('scope_paths')
  IS NULL OR file_id IN (SELECT file_id FROM scope_files)`, the NULL test
  first so that it folds away live), takes file-level facts from
  `scope_files` rather than `files`, and reads text through `sources`, which
  is restricted the same way: a file's rows depend on that file alone.
- **Unit rules are written once, as a table macro over named relations.**
  `rust_units_over(nodes_t, bytes_t, lines_t)` reads its inputs with
  `query_table`, so the same text derives the units of the current files
  and of the blobs; each application is still one `read_ast` scan
  (`MATERIALIZED` common table expressions are honored inside the macro).
  The blob inputs use an integer key (`blob_ord`) in `file_id`, not the
  blob id. `unit_id` numbers units in
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
| `since` | unset (all of HEAD) | history views, `blob_files`, `refresh.sql` |
| `include_merges` | `false` | history views, `blob_files`, co-change and history-joined queries, `refresh.sql` |
| `clone_reports` | `[repo/target/code-db/jscpd/exact/jscpd-report.json, repo/target/code-db/jscpd/normalized/jscpd-report.json]` | `clone_pairs` |
| `cache_path` | `repo/target/code-db/cache.duckdb` | `refresh.sql` |
| `scip_index` | `repo/target/code-db/index.scip` (must exist when `views/scip.sql` is read) | `views/scip.sql`, `refresh.sql` |
| `blob_dir` | `repo/target/code-db/blobs` (where `git_changed_blobs` writes the blob files) | `views/blobs.sql`, `refresh.sql` |
| `scope_paths` | NULL (all files); set only by `refresh.sql` | `scope_files` |
| `min_shared`, `max_commit_files`, `min_coupling_pct`, `basis` | 3, 50, 30, `'max'` | `file_cochange` (the first two also `file_cochange_thresholds`) |
| `match_mode`, `clone_source` | `'contain'`, NULL (all reports) | `clone_site_tags` (`match_mode` also the summary); `clone_source` also `clone_change_consistency` |
| `since_time`, `kind` | NULL (all commits; all unit kinds) | `unit_churn` (`kind` also `unit_fan_in_churn`) |
| `min_fan_in` | 1 | `unit_fan_in_churn` |

The source files are the regular files (mode `100644` or `100755`) that
`git_tracked_files` lists, with an extension in `languages`: ignored files,
files inside nested repositories (other clones, worktrees placed inside the
repository) and files deleted in the worktree never appear, symlinks and
submodules are skipped. `refresh.sql`'s variables named `refresh_*` are its
own intermediate values; `blob_snapshot`, `blob_paths` and
`blob_paths_empty` are the blob snapshot of `views/blobs.sql` (and
`refresh.sql`).

How parameters reach the table functions: `getvariable('…')` is accepted
as an argument of `read_ast`, `read_text`, `read_json`, `glob`, `ATTACH` and
the git and scip functions, including their named parameters, inside a view,
and is evaluated when the view is scanned. `LOAD` takes only a literal, so
the git_history path is fixed in `00_load.sql`. A list-valued variable
cannot be empty where a table function reads it (`read_ast` refuses an
empty list), and `read_json` must find the reports when `clones.sql` is
executed. A named parameter given an expression rather than a variable can
be parsed as a positional argument (`ignore_errors := len(x) = 0` is
`(ignore_errors := len(x)) = 0`): pass a variable.

## Deliberately not here yet

- **Tags of blobs**, and following a unit across renames and moves.
- Go and TypeScript, clone groups (the spike's `group_id`), and any query
  beyond the seven ported ones. SCIP relationships (`relationship_count`
  only; rust-analyzer 1.99.1 writes none) and an index of the historical
  blobs.
- **Per-layer choice.** Both caches hold all their layers; `clone_pairs`
  and `unit_changes` stay live in both, and `cache.sql` leaves
  `blob_nodes` live.
