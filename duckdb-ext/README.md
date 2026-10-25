# DuckDB extensions

DuckDB extensions for the code database (a repository's structure and
history as language-independent tables, queried in SQL), written in Rust. This directory is a Cargo workspace of its own (own `Cargo.lock`, own
`target/`), excluded from the root workspace like `preview-plugin/`. The
root quality gate does not read it; `./check.sh` is its gate.

| Crate | Output |
| --- | --- |
| `git-history-core/` | a library: the rows of the five functions, computed with gitoxide (`gix`) only; no DuckDB dependency, unit-tested and compared with the git CLI on fixtures |
| `git-history/` | `git_history.duckdb_extension`: the DuckDB table functions `git_hunks`, `git_commits`, `git_file_changes`, `git_tracked_files`, `git_changed_blobs` over `git-history-core` |
| `gix-probe/` | a binary printing any function's rows of `git-history-core` as TSV, compared with the git CLI by `scripts/gix_parity.py` |
| `host-check/` | no output; a test that loads the built extension through the `duckdb` crate (a separate workspace, see below) |

## Route and pin

The extension uses DuckDB's **C API v2** (`USE_UNSTABLE_C_API=1`, ABI type
`C_STRUCT_UNSTABLE`) through `duckdb-neo`, the route of the official Rust
template since its "Neo template" (PR #55). A `C_STRUCT_UNSTABLE` binary
loads only into the exact DuckDB version written in its footer, here
**v2.0.0**; any other version refuses it at `LOAD` (see below). Moving to a
new DuckDB release means bumping `duckdb-neo` and `duckdb-loadable-macros`
to the bindings for that release, fixing whatever the v2 API changed (it
changes without deprecation), changing `TARGET_DUCKDB_VERSION` in
`build.sh`, rebuilding, and shipping a new binary per DuckDB version used by
a host. Every host must be on the same version: Python's `duckdb` wheel, the
CLI and the `libduckdb` Horizon links are separate copies.

Pins:

| What | Pin |
| --- | --- |
| Template studied | `duckdb/extension-template-rs` at `bedafd45415531d32a595b52854c540697877b58` (2026-10-23, merge of PR #55 "Neo template") |
| CI tools studied | `duckdb/extension-ci-tools` at `2de802913caf5c9bba0ef267658b20f3cc0486c3` (2026-10-23); `scripts/append_extension_metadata.py` last changed in `7860c98d15ffaa7556522237bede317eb67a86a6` |
| DuckDB | v2.0.0 (`PRAGMA version`: `v2.0.0`, source id `d54cbbe3df`) |
| `duckdb-neo` | `=0.20000.0`, feature `build-extension` |
| `duckdb-loadable-macros` | `=1.20000.0`, feature `neo` |
| `gix` | 0.90.0, no default features, features `blob-diff`, `revision`, `sha1`, `max-performance`, `status` (resolves `gix-diff` 0.70.0, `gix-imara-diff` 0.3.2, `gix-traverse` 0.64.0, `gix-odb` 0.87.0, `gix-pack` 0.77.0, `gix-status` 0.37.0, `gix-dir` 0.32.0, `gix-filter` 0.36.1) |
| `duckdb` (host check only) | `=1.20000.0`, the version the root `Cargo.lock` pins |

What was kept from the template: the `cdylib` with the
`#[duckdb_entrypoint_c_api_v2]` entrypoint, the release profile (`lto`,
`strip`), and the Makefile's variables (`EXTENSION_NAME`,
`TARGET_DUCKDB_VERSION=v2.0.0`, `USE_UNSTABLE_C_API=1`, `CAPI_VERSION=2`).
What was dropped: the `core`/`native`/`wasm` crate split (one native crate
is enough; there is no Wasm build), the `extension-ci-tools` git submodule,
its Makefiles and venv, and the sqllogictest runner. `CAPI_VERSION=2` has no
footer field: it only selects the `_init_c_api_v2` entrypoint name, which
the macro already emits.

The footer is written by `append_metadata.py`, our own equivalent of
`extension-ci-tools/scripts/append_extension_metadata.py` (that repository
declares no license, so it is not vendored). For the same input and
arguments it produces a byte-identical file (checked with `cmp` against the
upstream script at the commit above). Fields: platform `linux_amd64`, DuckDB
version `v2.0.0`, extension version `v<crate version>`, ABI type
`C_STRUCT_UNSTABLE`.

## Licenses

The extension links no C code and no GPL code: git access is gitoxide only
(no libgit2), and nothing in the workspace is ported from git's sources.
The normal dependency tree of `git_history` (`cargo tree -p git_history -e
normal --prefix none -f "{p} {l}"`, 122 crates besides the two of this
workspace; `status` added `gix-status` and `gix-dir`, both MIT OR
Apache-2.0) has these licenses:

| License | Crates |
| --- | --- |
| MIT OR Apache-2.0 (and spellings such as `MIT/Apache-2.0`), including every `gix-*` crate but one | most |
| Apache-2.0 only | `gix-imara-diff` (gix's line diff), `unicode-bom` |
| MIT only | `clru`, `darling`, `darling_core`, `dashmap`, `duckdb-neo`, `faster-hex`, `libduckdb-sys`, `nonempty`, `prodash`, `strsim` |
| Unlicense OR MIT | `byteorder`, `jiff`, `jiff-core`, `memchr`, `same-file`, `walkdir` |
| Zlib | `foldhash`, `zlib-rs` |
| MPL-2.0 | `uluru` (the static pack cache that gix's `max-performance` enables) |
| others | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT`, `(MIT OR Apache-2.0) AND Unicode-3.0`, `(Apache-2.0 OR MIT) AND BSD-3-Clause`, `Zlib OR Apache-2.0 OR MIT` |

`max-performance` is kept: without it (no pack caches, no `parallel`),
the probe took 0.98–1.03 s instead of 0.86–0.93 s for the hunks and
0.95 s instead of 0.82–0.86 s for the file changes of this repository;
`pack-cache-lru-dynamic` alone (MIT `clru`, no `uluru`) was no faster than
no cache, because gix only uses the dynamic cache when configured to.
MPL-2.0 is file-level copyleft and binds only changes to `uluru`'s own
files. `cargo deny check licenses` (cargo-deny 0.19.6, a throwaway config
allowing the set above) also reports ISC (`ring`, `untrusted`,
`rustls-webpki`) and CDLA-Permissive-2.0 (`webpki-roots`); those are
build-time only, for `libduckdb-sys`'s build script, and are not linked
into the extension.

## Build

Requires a Rust toolchain, `python3`, and a C compiler for `ring`, which
`libduckdb-sys`'s build script uses (the extension itself links no C).

```sh
duckdb-ext/build.sh      # prints the path of the built extension
duckdb-ext/check.sh      # fmt, clippy, tests of every member, build.sh, then the host check
```

Output: `duckdb-ext/target/extension/v2.0.0/linux_amd64/git_history.duckdb_extension`
(about 5.9 MB; links only `libc` and `libgcc_s` dynamically).

## Loading

The extension is unsigned, so the host must allow unsigned extensions and
`LOAD` it by absolute path.

Python (`duckdb==2.0.0`):

```python
import duckdb
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.sql("LOAD '/abs/path/git_history.duckdb_extension'")
con.sql("SELECT * FROM git_hunks('.') WHERE path LIKE '%.rs'")
```

Rust, with the `duckdb` crate linked against the machine-installed
`libduckdb` 2.0.0 the way Horizon links it (AGENTS.md "Build setup"):

```rust
let config = duckdb::Config::default().allow_unsigned_extensions()?;
let conn = duckdb::Connection::open_in_memory_with_flags(config)?;
conn.execute_batch("LOAD '/abs/path/git_history.duckdb_extension'")?;
```

`host-check/tests/load.rs` does exactly this on a fixture repository. It is
a workspace of its own because cargo unifies features within a workspace:
next to `duckdb-neo`, `libduckdb-sys` would get `loadable-extension-v2`,
which stops it from linking `libduckdb`, and the host would not link.
Horizon's daemon could load the extension the same way; nothing in Horizon
does so yet.

The DuckDB CLI: `duckdb -unsigned -c "LOAD '/abs/path/git_history.duckdb_extension'"`,
only with a v2.0.0 CLI. The system CLI here is v1.5.0 and refuses it:

```
Invalid Input Error: Failed to load '…/git_history.duckdb_extension', The file was built specifically for DuckDB version 'v2.0.0' and can only be loaded with that version of DuckDB. (this version of DuckDB is 'v1.5.0')
```

## Functions

The three history functions and `git_changed_blobs` take `repo VARCHAR`
(any path inside a worktree, or a bare repository; it must be a constant)
and two optional named parameters:
`since VARCHAR` (any revision; history is `since..HEAD`, default all of
`HEAD`) and `include_merges BOOLEAN` (default `false`). Rows are computed
when the scan starts and emitted from one thread, so an unordered query
returns them in the order below. Column names and semantics are defined
below, with the deviations from `git log` listed under "How the rows are
computed".

### `git_hunks(repo, since := …, include_merges := …)`

`(sha VARCHAR, path VARCHAR, old_start INTEGER, old_lines INTEGER, new_start INTEGER, new_lines INTEGER)`

One row per `@@ -old_start,old_lines +new_start,new_lines @@` header of
`git log -p -U0 -M --no-ext-diff --no-textconv --diff-merges=off`
(`--diff-merges=first-parent` with `include_merges`), with git's 1-based
numbers (a count of 0 means a pure insertion or deletion, and its start is
the line before). `path` is the new path, or the old one for a deletion.
This covers all files, with no pathspec; filter in SQL (`WHERE path LIKE
'%.rs'`). Binary files and
submodules have no rows. A type change (file to symlink or back) has the
hunks of one diff from the old content to the new one, the symlink's
content being its target path. Where a change could be placed at several
equally valid lines, the placement is gix's and can differ from git's
(below).

### `git_commits(repo, since := …, include_merges := …)`

`(sha VARCHAR, author_time TIMESTAMP, author_name VARCHAR, subject VARCHAR, parent_count INTEGER, is_merge BOOLEAN)`

The commits `git log [since..]HEAD` lists, in its order. `author_time` is
the author timestamp in UTC (`%at`), `author_name` is `%an` (no mailmap),
`subject` is `%s` (the first paragraph, lines joined with spaces).
`include_merges` does not change this table.

### `git_file_changes(repo, since := …, include_merges := …)`

`(sha VARCHAR, path VARCHAR, old_path VARCHAR, status VARCHAR, added INTEGER, deleted INTEGER)`

One row per changed file of `git log --raw --numstat -M`: `status` is git's
letter (`A`, `M`, `D`, `R`, `T`; `C` cannot occur, copies are not
detected), `old_path` is set for renames, `added`/`deleted` are the
`--numstat` counts and NULL for binary files and submodules. Merges have no
rows unless `include_merges`.

### `git_tracked_files(repo, include_untracked := …)`

`(path VARCHAR, mode VARCHAR, blob_id VARCHAR, is_tracked BOOLEAN, is_modified BOOLEAN)`

The worktree's files now, not history: one row per path of `git ls-files`,
sorted by path bytes. `repo` is any path inside a worktree (a bare
repository is an error); `include_untracked BOOLEAN` (default `false`) adds
the files of `git ls-files -o --exclude-standard`. `path` is
repository-relative and unquoted (`core.quotePath` does not apply). `mode`
is git's mode string (`100644`, `100755`, `120000`, `160000`) and `blob_id`
the object id of the **current** content, both as `git add` would record
them now:

| Row | `mode` | `blob_id` | `is_tracked` | `is_modified` |
| --- | --- | --- | --- | --- |
| tracked, unchanged in the worktree | the index entry's | the index entry's | true | false |
| tracked, content or executable bit changed (staged or not) | the worktree file's | the hash of the worktree content after the clean filters (`git hash-object PATH`) | true | true |
| tracked, deleted in the worktree | no row | | | |
| tracked symlink | `120000` | the blob of the link's target path | true | as for files |
| gitlink (submodule) | `160000` | the commit the index records; submodules are never opened | true | false |
| unmerged (stages 1–3) | the worktree file's | the hash of the worktree content (with conflict markers) | true | true; no row if the file is gone |
| `git add -N` (intent to add) | the worktree file's | the hash of the worktree content | true | when it is not the empty blob |
| untracked, not ignored | the worktree file's | the hash of the worktree content | false | false |

Never listed: ignored files (`.gitignore`, `.git/info/exclude`,
`core.excludesFile`), anything under `.git`, and anything inside a nested
repository: a directory holding a `.git` (another clone, a linked worktree
placed in this one, an unregistered `git init`) is neither descended nor
listed, where `git ls-files -o` prints it as one `sub/` line. Untracked
sockets and FIFOs are skipped. Sparse-checkout
(skip-worktree) entries keep their index row.

### `git_changed_blobs(repo, dir, since := …, include_merges := …, extensions := …)`

`(sha VARCHAR, path VARCHAR, old_path VARCHAR, side VARCHAR, blob_id VARCHAR, mode VARCHAR, file VARCHAR)`

Writes the pre- and post-image blobs of the changed files of a history range
to the directory `dir` and returns one row per written side. It is the
interim way to give historical file contents to `read_ast`: once DuckDB's C
API v2 can register a file system (duckdb/duckdb#25916, open), a
`git://` file system replaces it. Until then, `read_ast` reads history
through the same path as current files (a list of files on disk), so the
same unit and tag SQL applies to both.

The changes are those of `git_file_changes(repo, since, include_merges)`:
the same commits, merge handling and rename detection, enumerated by the
same code. Each change has up to two rows, `side = 'old'` (the pre-image,
at `old_path` for a rename, else at `path`) and `side = 'new'` (the
post-image at `path`), `old` first; rows follow `git_file_changes`'s row
order. `path` and `old_path` are `git_file_changes`'s (the new path; the
pre-image path for renames, else NULL); `blob_id` is the side's object id
and `mode` its mode string (`100644` or `100755`). A side has a row only
when all of these hold:

- it exists: no `old` row for an addition, no `new` row for a deletion;
- it is a regular file (`100644`/`100755`): symlinks and gitlinks have no
  row for that side, so a type change has a row for its regular side only;
- it is not binary by gix's rule, judged per side: the `diff`/`binary`
  attributes, else a NUL byte in the first 8,000 bytes, or a size above
  `core.bigFileThreshold`. A text file that turns binary keeps its `old`
  row; a change whose counts are NULL in `git_file_changes` because one
  side is binary therefore can still have a row for the other side;
- with `extensions` (a `VARCHAR[]` such as `['rs']`), the extension of
  that side's path is in the list. The extension is the part after the
  last `.` of the last path component (`.gitignore` has `gitignore`; `Makefile`
  and `name.` have none and never match); the comparison is exact (case
  sensitive). An empty list matches nothing, NULL (the default) everything.

A change whose two sides are regular files with the same blob has no rows
at all: a rename without content change (git lists it with status `R` and
`0 0` counts; there is no content to parse that the earlier commit did not
already provide, which is also what the spike's `unit_changes` did), and a
change of the executable bit alone.

Writing rules:

- `dir` must be an absolute path (a relative one is an error at bind time,
  since the DuckDB process's working directory need not be the
  repository's); it is created with its parents if missing.
- Each blob is written once per extension as `<dir>/<blob_id>.<ext>`, where
  `<ext>` is the extension of the side's path, or `<dir>/<blob_id>` when it
  has none, so `read_ast` can infer the language from the name. `file` is
  that absolute path. A blob referenced by several rows is one file.
- The bytes are the blob's exactly, read from the object database (no
  smudge filters, no end-of-line conversion).
- A file that already exists with the blob's size is not rewritten (the
  content is implied by the id); one with another size is replaced. Each
  file is written to a temporary name in `dir` (`.<name>.<pid>-<n>.tmp`)
  and renamed into place, so a reader never sees a partial file.
- The repository is only read. Running the function again with the same
  arguments writes nothing and returns the same rows; files from other
  ranges or older runs are left in `dir`, never deleted.

The files are written when the scan starts (any query that reads the
function, `SELECT count(*)` included), not at bind time. The function
returns only rows; the counts of written and skipped files are printed by
`gix-probe changed_blobs --stats`.

```sql
CREATE TEMP TABLE blobs AS
SELECT * FROM git_changed_blobs('/repo', '/repo/target/code-db/blobs', extensions := ['rs']);
SET VARIABLE files = (SELECT list(DISTINCT file) FROM blobs);
SELECT * FROM read_ast(getvariable('files'), 'rust', source := 'full', peek := 'none', context := 'none');
```

## How the rows are computed

Everything is gitoxide, called from `git-history-core/src/lib.rs`:

| Step | gix call and settings |
| --- | --- |
| Commits of `[since..]HEAD` | `repo.rev_walk([HEAD]).sorting(ByCommitTime(NewestFirst)).with_hidden([since])` |
| Commit fields | `author()` (name, time in seconds), the message's `summary()` for `%s`, `parent_ids()` |
| Changed files | `gix::diff::tree_with_rewrites` against the first parent (the empty tree for a root commit), with `Rewrites { copies: None, percentage: Some(0.5), limit: 1000, track_empty: false }`; tree entries are skipped; `T` when the entry kind (file, symlink, submodule) changes |
| Line diff | `repo.diff_resource_cache_for_tree_diff()`, `set_resource` for both sides, `prepare_diff()` (binary: hunks none, counts NULL), `Diff::compute(Myers)` |
| `added`/`deleted` | `count_additions()`/`count_removals()` of that diff |
| Hunks | the same diff after `postprocess_lines` (gix's indent-based slider heuristic), one row per `hunks()` range, numbered like a zero-context unified diff header |

Measured against `git log` (git 2.55.0) on this repository, the record
`docs/research/gix-parity-2026-10-24.md` has the detail; the deviations are:

- **Commits and file changes:** none. Commit order (including commits with
  equal committer times), every commit field, rename pairing, statuses,
  NULL counts and numstat are identical, with and without `include_merges`.
  gix's rename detection is a different algorithm from git's (no
  basename pass, a similarity from the line diff); the equality is
  observed on this history, not guaranteed for every history.
- **`-U0` hunks:** about 0.4 % of the rows differ (183 of git's 44,115
  at 0df26a53, in 73 files; see "Parity"). Where an insertion or deletion
  can be placed at several lines, or a change can be cut into hunks in
  several ways, gix and git choose differently: git trims the common tail
  of both sides before a zero-context diff, gix diffs whole files. Neither
  placement is more correct. With context lines (`-U1`, `-U3`) the hunks
  are identical, so the line diffs themselves agree.
- **Type changes:** git prints a file-to-symlink change as a deletion
  followed by a creation (two hunks); gix diffs the two contents as one
  modification (one hunk). `git_file_changes` agrees with git (`T`, same
  counts).
- **Submodules (gitlinks):** gix's blob platform does not diff them, so
  they have no hunks and NULL counts; git diffs a synthesized
  `Subproject commit <sha>` line (counts `1 0` for an addition, `1 1` for
  an update). The status letter is git's.

Not covered, because nothing here needs it: `.mailmap`, textconv and
external diff drivers (both disabled), the repository's `diff.*`
configuration, copy detection, pathspec history simplification,
`diff.renameLimit` other than 1000.

`git_changed_blobs` is `git-history-core/src/blobs.rs`, on the change
enumeration above (`for_each_change`, which `git_file_changes` uses):

| Step | gix call and settings |
| --- | --- |
| Binary check per side | `set_resource` of that side alone on the tree-diff blob platform, then `resource(kind).data`: `Data::Binary` means binary (gix's pipeline applies the `diff` attribute and its driver's `binary` setting, NUL detection and the big-file threshold) |
| Existing file | `repo.find_header(id).size()` against the file's `symlink_metadata` length |
| Bytes | `repo.find_blob(id).data`, written with `std::fs::write` to the temporary name and `std::fs::rename`d |

`git_tracked_files` is `git-history-core/src/tracked.rs`:

| Step | gix call and settings |
| --- | --- |
| Index | `repo.index_or_empty()`, entries in index order (by path bytes) |
| Worktree changes of tracked files | `repo.index_worktree_status(index, no patterns, Recorder, HashEq, BuiltinSubmoduleStatus(Ignore::All), …)` with no rewrite tracking: gix's stat check against the index (racy-git aware, `core.*` stat options), and for a file whose stat differs, `HashEq` hashes it through gix's to-git pipeline and reports the new id only if it differs from the index id |
| Untracked files | the same call with `repo.dirwalk_options().emit_untracked(Matching)`: gix's directory walk with the standard excludes, emitting each untracked file (no collapsed directories, no ignored entries); only entries whose disk kind is file or symlink are kept |
| Content ids not reported by the status (type changes, unmerged and intent-to-add paths, untracked files) | `repo.filter_pipeline(None)` then `convert_to_git` on the file and `gix::objs::compute_hash(Blob)`; a symlink is hashed as its `read_link` target |
| Worktree mode | `gix::index::fs::Metadata::from_path_no_follow`: symlink if `core.symlinks`, `100755` if the owner's executable bit is set and `core.fileMode`, else `100644` |

The clean filters applied are gix's to-git pipeline as the repository's
attributes and configuration select it: `text`/`eol`/`core.autocrlf`
conversion, `ident`, `working-tree-encoding`, and `filter.<driver>.clean`
or `.process` drivers (run as configured, like git). No filter is skipped.
`git hash-object PATH` itself differs from these ids in one case: it
follows a symlink and hashes the file it points to; the ids here are those
of `git add` (the link's target path), which the parity script obtains with
`git hash-object --stdin`.

## Parity

Measured on this repository at 0df26a53 (1,589 commits, 343 merges)
against git 2.55.0 with `scripts/gix_parity.py` (rows as multisets, commit
order as sequences):

| Comparison | git_history | Reference | Only in git_history / only in reference |
| --- | --- | --- | --- |
| `git_commits` order vs `git log --format=%H`, for no `since` and nine `since` values | 1,589 | 1,589 | identical |
| `git_commits` fields vs `git log --format=%H%x09%at%x09%an%x09%s%x09%P` | 1,589 | 1,589 | 0 / 0 |
| `git_file_changes` vs `git log --raw --numstat -M` | 9,008 | 9,008 | 0 / 0 |
| same with `include_merges` vs `--diff-merges=first-parent` | 13,172 | 13,172 | 0 / 0 |
| `git_hunks` vs `git log -p -U0 -M` (all files) | 44,098 | 44,115 | 166 / 183 (73 files) |
| same with `include_merges` | 64,402 | 64,430 | 239 / 267 (118 files) |
| hunks with one context line vs `git log -p -U1 -M` | 35,101 | 35,101 | 0 / 0 |
| same with `include_merges` | 51,361 | 51,361 | 0 / 0 |
| hunks with three context lines vs `git log -p -U3 -M` | 28,217 | 28,217 | 0 / 0 |

The context-line rows come from `gix-probe --context N`, which uses the
same diff as `git_hunks` and adds N lines of context through gix's
`UnifiedDiff`. The extension's rows are identical, in order, to
`gix-probe`'s TSVs for all five row sets above. Filtering the rows by path
in SQL is not the same as giving git a pathspec: `git log -p -- '*.rs'`
turns on history simplification, which drops commits on a side branch
whose merge is TREESAME to the other parent for those paths (commit
e91103e8, reached through merge 4ba1ceff, deletes one line of
`crates/horizon-sessiond/tests/latency_probe.rs`; `git log -- '*.rs'`
omits it, `git log --full-history -- '*.rs'` and plain `git log -p` show
it). The extension covers all files and never simplifies.

`git_tracked_files`, with `scripts/tracked_parity.py` (rows as sets; the
reference row for each path is built from `git ls-files -s -z`, `git
ls-files -o --exclude-standard -z`, `git status --porcelain=v2 -z` and `git
hash-object` as the table in "Functions" describes), at 2a0de30a with this
change's own edits in the worktree:

| Comparison | git_history | Reference | Only in git_history / only in reference |
| --- | --- | --- | --- |
| tracked rows vs `git ls-files -s` (824 lines), 6 of them modified | 824 | 824 | 0 / 0 |
| with `include_untracked` vs the above plus `git ls-files -o --exclude-standard` (3 lines) | 827 | 827 | 0 / 0 |
| same, with an unregistered nested repository `scratch-nested/` added | 827 | 827 | 0 / 0 (git lists `scratch-nested/`, which has no file row by design) |
| unchanged rows whose index id differs from `git hash-object` | 0 | | |

On the main checkout, whose `.claude/worktrees/` holds the nested
worktrees that a core `glob()` walked in 20.9 s, `git_tracked_files` with
`include_untracked` returns 778 rows, all tracked, in 5 ms (the directory
is ignored, so no row comes from it). The extension's rows
are identical, in order, to `gix-probe tracked_files`'s TSV.

Scripts (Python, run with `uv run --with duckdb==2.0.0 python …` where they
load the extension): `scripts/gix_parity.py` (the probe against the git
CLI; `python3 duckdb-ext/scripts/gix_parity.py REPO duckdb-ext/target/release/gix-probe OUT_DIR`),
`scripts/cli_hunks.py` and `scripts/cli_file_changes.py` (the git CLI
output parsed into the same TSV rows), `scripts/load_smoke.py`,
`scripts/perf.py`, `scripts/tracked_parity.py` (`python3
duckdb-ext/scripts/tracked_parity.py REPO duckdb-ext/target/release/gix-probe`).
`gix-probe REPO hunks|commits|file_changes|tracked_files|changed_blobs
[--since REV] [--include-merges] [--context N] [--include-untracked]
[--dir DIR] [--extensions A,B] [--stats]` prints any function's rows as
TSV; for `changed_blobs`, `--stats` also prints the files written (with
their bytes) and skipped.

`git_changed_blobs` on this repository at 865e4045 (1,601 commits), with
`extensions := ['rs']`, compared with `git log --raw --no-abbrev -M
--diff-merges=off` (old/new ids and modes, with the rules above applied to
its lines; no `.rs` blob is binary): 12,490 rows (5,983 `old`, 6,507
`new`) and 6,581 distinct blobs on both sides, 0 / 0 rows only on one
side. The extension's rows are identical, in order, to `gix-probe
changed_blobs`'s TSV. The spike (`docs/research/code-db-spike-2026-10-24.md`)
parsed 6,558 blobs from 12,451 lookups at 4ad82864 (1,585 commits) with
`git log -- '*.rs'`; the CLI at that commit gives exactly those numbers
with the pathspec, and 12,453 rows (the same 6,558 blobs) without it: the
two extra rows are e91103e8's change to
`crates/horizon-sessiond/tests/latency_probe.rs`, which history
simplification drops (see above), and whose two blobs occur in other
commits too. The 23 further blobs (37 rows) come from the 16 commits
after 4ad82864.

## Performance

This repository at 0df26a53, Python `duckdb` 2.0.0, `SELECT count(*)`, each
function in a fresh process, on a shared 24-thread desktop (times vary by
about ±0.3 s between runs):

| Function | Rows | Wall time | Peak RSS (one scan) |
| --- | --- | --- | --- |
| `git_hunks` | 44,098 | 0.86–1.44 s | 208 MiB (70 MiB after `LOAD`) |
| `git_file_changes` | 9,008 | 0.82–1.41 s | 212 MiB |
| `git_commits` | 1,589 | 0.01–0.03 s | 79 MiB |

`gix-probe` alone takes 0.86–0.93 s for the hunks and 0.82–0.86 s for the
file changes (peak RSS 142 MiB), the git CLI 0.65–0.68 s for `git log -p
-U0 -M`. The libgit2 build this replaces took 2.8–2.9 s and 3.2–4.5 s.
The scan is single-threaded and keeps a 64 MiB object cache per
repository; diffing commits in parallel is the obvious next step if this
matters.

`git_tracked_files` on this repository at 2a0de30a (824 tracked files, 6
modified, 3 untracked), same setup, five fresh processes each:

| Call | Rows | Wall time | Peak RSS (one scan) |
| --- | --- | --- | --- |
| `git_tracked_files('.')` | 824 | 9–31 ms | 77 MiB (70 MiB after `LOAD`) |
| `git_tracked_files('.', include_untracked := true)` | 827 | 10–51 ms | 76 MiB |

`gix-probe tracked_files` takes 5.6–6.2 ms (min–median of 20 runs; 5.7–6.9
ms with `--include-untracked`), against 3.7–3.9 ms for `git ls-files -s`,
4.1–5.5 ms for `git ls-files -o --exclude-standard`, and 4.9–7.7 ms for
`git status --porcelain=v2`. Files whose index stat is current are not
read; a file is hashed only when its stat differs from the index (or is
racily clean), and gix runs that check on several threads.

`git_changed_blobs('.', dir, extensions := ['rs'])` on this repository at
865e4045 (12,490 rows, 6,581 files, 152,326,274 bytes written), Python
`duckdb` 2.0.0, `SELECT count(*), count(DISTINCT blob_id), count(DISTINCT
file)`, fresh processes: 1.10–1.12 s into an empty directory, 0.63–0.76 s
for a run that finds every file present (6,581 skipped). `gix-probe
changed_blobs --extensions rs` takes 0.93 s writing and 0.54–0.62 s
skipping.

## Tests

- `git-history-core/src/lib.rs` unit tests: `%s` subject folding, the
  `M`/`T` status rule, the zero-context `@@` numbering, and the empty-side
  correction of `UnifiedDiff` headers.
- `git-history-core/tests/cli_parity.rs`: builds a fixture repository with
  the `git` CLI (fixed names and dates, no user or system configuration)
  with several commits, a multi-hunk change, an insertion-only and a
  deletion-only change, a rename with and without content change, a binary
  file, a file turned into a symlink and a merge commit, then compares all
  three functions with the CLI's output for four ranges (with and without
  `since`, with and without `include_merges`): commits in order and file
  changes exactly, hunks exactly except for the type change, whose gix
  convention (one hunk) it asserts. It also asserts each case's rows, that
  two runs give identical rows, and, on a fixture with a submodule entry,
  that gitlinks get git's statuses, NULL counts and no hunks.
- `git-history-core/tests/tracked_files.rs`: builds a worktree with the
  `git` CLI (same isolation) holding a tracked unchanged file, a file
  modified in the worktree, one staged and then modified again, one deleted
  in the worktree, an executable, a file whose only change is its
  executable bit, a symlink, a gitlink without a submodule repository, a
  nested directory, names with a space and with non-ASCII characters, a
  CRLF file normalized by a `text eol=crlf` attribute, untracked files
  (plain, executable, symlink, CRLF, in a new directory), files ignored by
  `.gitignore` (by pattern and by directory), by `.git/info/exclude` and by
  `core.excludesFile`, and a nested repository with a commit. It compares `tracked_files` with
  rows built from `git ls-files -s -z`, `git ls-files -o --exclude-standard
  -z`, `git status --porcelain=v2 -z` and `git hash-object` for both values
  of `include_untracked`, asserts that every unchanged file's index id is
  its `git hash-object` id, and asserts each case's row (or its absence),
  the clean filter's effect on a CRLF id, and that two runs agree. A second
  fixture with a merge conflict asserts the one row of an unmerged path.
- `git-history-core/tests/changed_blobs.rs`: builds a fixture (same
  isolation) with additions (including an executable and an extensionless
  file), a modification, a deletion, a rename without and with content
  change, a retargeted symlink, a binary change, a text file turned
  binary, both type changes (file to symlink and back), an executable-bit
  change alone, two commits sharing a blob, and a merge. For no `since` and
  a `since`, with and without `include_merges`, and for no `extensions`,
  `['rs']` and `['rs', 'sh']`, it compares the rows with `git log --raw
  --no-abbrev -M` (git's binary rule applied to `git cat-file -p` of each
  side) and every file with `git cat-file -p` of its blob, checks the file
  names, the write counts and that `dir` holds nothing else; asserts that
  rows follow `file_changes`'s order with `old` first and which of its
  changes have no rows; asserts each case's rows, that a shared blob is one
  file, that extensions are judged per side and an empty list matches
  nothing, that a second run skips every file but rewrites a truncated one
  (and trusts a same-size one), and that a relative `dir` is an error.
- `host-check/tests/load.rs`: loads the built extension into the `duckdb`
  crate with the machine's `libduckdb` 2.0.0 and checks `git_hunks` joined
  with `git_commits`, and `git_tracked_files` with `include_untracked`, on
  a fixture.
