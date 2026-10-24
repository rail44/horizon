# Code database views

A repository's syntax, history and clone reports as SQL views over table
functions, with no build step (`docs/code-database-design.md`, principle of
2026-10-24): facts come from `read_ast` (community extension
`sitting_duck`), `git_commits`/`git_file_changes`/`git_hunks` (this
workspace's `git_history`, `../README.md`) and jscpd's JSON read with the
core `read_json`; everything else is a view. Nothing is stored: every query
reads the sources again. Materializing a view (`CREATE TABLE … AS SELECT * FROM
nodes`) is a cache the caller chooses, not part of this directory.

Measurements of what this costs on this repository are in
`docs/research/code-db-views-2026-10-24.md`.

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
not read any source when they are executed.

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
to an "agent mode" with Markdown tables; `-no-agent` turns it off.

Python (`duckdb==2.0.0`):

```python
import duckdb, pathlib
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute("SET VARIABLE repo = '/abs/path/to/repo'")
for f in ["00_load.sql", "views/files.sql", "views/nodes.sql", "lang/rust.sql",
          "views/units.sql", "views/history.sql", "views/clones.sql"]:
    con.execute(pathlib.Path("duckdb-ext/sql", f).read_text())
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
uv run --locked --script duckdb-ext/sql/check.py cli .            # the same files through the CLI's .read
uv run --locked --script duckdb-ext/sql/check.py measure . --runs 3
uv run --locked --script duckdb-ext/sql/check.py explain .        # read_ast/read_text scans per plan
```

`fixture` builds a git repository in a temporary directory (a trait, two
trait impls, an inline test module, an integration test, an untracked file,
a build-output and a hidden-directory file that must be excluded, four
commits, one of them touching only a non-Rust file) and a report in jscpd
5.3.1's JSON shape, then asserts the rows of `files`, `units` (kinds,
qualified names, lines, `loc`, parents), `tags`, the three history views,
`clone_pairs` and all four queries, including both match modes, a `since`
change and a file edit seen without reloading. It needs `git` but no jscpd.

## Layout

| File | Defines |
| --- | --- |
| `00_load.sql` | `INSTALL`/`LOAD` of both extensions; parameter defaults; `source_files`/`source_paths` computed from the globs |
| `views/files.sql` | `files(file_id, path, language)`, `sources(file_id, content)`, `source_hex`, `source_lines(file_id, line, nonblank_through)`, macros `byte_slice`, `normalize_ws` |
| `views/nodes.sql` | `nodes`: `file_id` plus every column of `read_ast(…, source := 'full', peek := 'none', context := 'none')` |
| `lang/rust.sql` | `rust_units`, `rust_tags` and their intermediate views (`rust_nodes`, `rust_unit_candidates`, …) |
| `views/units.sql` | `units`, `tags`: the union over languages (Rust only today) |
| `views/history.sql` | `commits`, `file_changes`, `hunks` |
| `views/clones.sql` | `clone_pairs(pair_id, source, file_a, start_line_a, end_line_a, file_b, start_line_b, end_line_b, tokens, lines)` |
| `queries/*.sql` | one statement each: `file_cochange`, `file_cochange_thresholds`, `clone_site_tags`, `clone_site_tags_summary`; the header states the question, the parameters and the evidence |
| `jscpd.sh` | the two jscpd reports `clones.sql` reads |
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

## Parameters

DuckDB variables (`SET VARIABLE name = value`). A view or query reads them
when it runs, so a change applies to the next query, except the file list,
which `00_load.sql` computes once.

| Variable | Default | Read by |
| --- | --- | --- |
| `repo` | required; an absolute path | everything |
| `include_globs` | `['**/*.rs']` (DuckDB `glob()` patterns relative to `repo`) | `00_load.sql` |
| `exclude_patterns` | `['target/*', '*/target/*', '.*', '*/.*']` (GLOB operator patterns on the relative path; `*` crosses `/`) | `00_load.sql` |
| `languages` | `MAP {'rs': 'rust'}` (extension to language) | `00_load.sql`, `files`, `hunks` |
| `since` | unset (all of HEAD) | history views |
| `include_merges` | `false` | history views, co-change queries |
| `clone_reports` | `[repo/target/code-db/jscpd/exact/jscpd-report.json, repo/target/code-db/jscpd/normalized/jscpd-report.json]` | `clone_pairs` |
| `min_shared`, `max_commit_files`, `min_coupling_pct`, `basis` | 3, 50, 30, `'max'` | `file_cochange` (the first two also `file_cochange_thresholds`) |
| `match_mode`, `clone_source` | `'contain'`, NULL (all reports) | `clone_site_tags` (`match_mode` also the summary) |

How parameters reach the table functions: `getvariable('…')` is accepted
as an argument of `read_ast`, `read_text`, `read_json`, `glob` and the
three git functions, including their named parameters, inside a view, and
is evaluated when the view is scanned. `LOAD` takes only a literal, so the
git_history path is fixed in `00_load.sql`. A list-valued variable cannot
be empty where a table function reads it (`read_ast` refuses an empty
list), and `read_json` must find the reports when `clones.sql` is executed.

## Deliberately not here yet

- **Unit changes per commit** (needs each changed file's syntax at each
  commit; open point in the design document).
- **Tracked files.** `files` comes from `glob()` over the working tree, so
  untracked files that match are included and `.gitignore` is not read; the
  default excludes cover build directories and hidden directories only. In
  a checkout holding nested worktrees or large build trees, set
  `include_globs` to the source roots: `glob()` walks everything below
  `repo` before the exclusions apply (about 21 s for `**/*.rs` in the main
  Horizon checkout with its worktrees, against 0.03 s in a single worktree).
- **SCIP**, Go and TypeScript, clone groups (the spike's `group_id`), and
  any query beyond the four ported ones.
- **Caching.** Nothing materializes anything; see the research record for
  which layers are too slow to leave live.
