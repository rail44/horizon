# DuckDB extensions

DuckDB extensions for the code database (a repository's structure and
history as language-independent tables, queried in SQL), written in Rust. This directory is a Cargo workspace of its own (own `Cargo.lock`, own
`target/`), excluded from the root workspace like `preview-plugin/`. The
root quality gate does not read it; `./check.sh` is its gate.

| Crate | Output |
| --- | --- |
| `git-history-core/` | a library: the rows of the six functions, computed with gitoxide (`gix`) only; no DuckDB dependency, unit-tested and compared with the git CLI on fixtures |
| `git-history/` | `git_history.duckdb_extension`: the DuckDB table functions `git_hunks`, `git_commits`, `git_file_changes`, `git_tracked_files`, `git_changed_blobs`, `git_tree` over `git-history-core` |
| `gix-probe/` | a binary printing any function's rows of `git-history-core` as TSV, compared with the git CLI by `scripts/gix_parity.py` |
| `scip-index-core/` | a library: a streaming decoder of SCIP indexes (protobuf) and the rows of the three SCIP functions; no dependencies, unit-tested and compared with `scip print --json` |
| `scip-index/` | `scip_index.duckdb_extension`: the DuckDB table functions `read_scip`, `read_scip_symbols`, `read_scip_metadata` over `scip-index-core` (see "`scip_index`" below) |
| `scip-probe/` | a binary printing any SCIP function's rows of `scip-index-core` as TSV, compared with the extension by `scripts/scip_ext_parity.py` |
| `host-check/` | no output; a test that loads both built extensions through the `duckdb` crate (a separate workspace, see below) |
| `code-db/` | `code-db`, the code database's entry point (`sql/README.md`, Running): reads the analysed repository's `.code-db.toml`, derives DuckDB's limits, runs the indexers, and loads the extensions and the SQL views through the `duckdb` crate (a separate workspace, like `host-check/`) |

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
| `scip.proto` (field numbers cited, not vendored) | `scip-code/scip` v0.10.0, commit `1c2b6db7e560d5233c944f36e4ac1377cc6963fc` |
| `scip` CLI (tests only, never linked) | v0.10.0 `scip-linux-amd64.tar.gz`, sha256 `eeb28ebbff443c01609fb1691809958f3f0c7a8651af689a5bc8560061cd8ab0`, fetched by `scripts/fetch-scip-cli.sh` into `target/tools/scip-v0.10.0/` |
| `rust-analyzer` (tests and measurements) | whatever is on `PATH`; the numbers below are from 1.99.1 (58cf8f9 2026-10-13) |

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

`scip_index` adds nothing to that set: `scip-index-core` has no
dependencies, so the extension's normal tree is `duckdb-neo` and
`libduckdb-sys` (both MIT) plus the proc macros of
`duckdb-loadable-macros` (MIT, MIT OR Apache-2.0, and `unicode-ident`'s
`(MIT OR Apache-2.0) AND Unicode-3.0`), 14 crates besides the two of this
workspace. `scip.proto` (Apache-2.0) is not copied; the decoder's module
doc cites its field numbers and enum values. `serde`, `serde_json` and
`tempfile` are dev-dependencies of `scip-index-core`'s tests.

## Build

Requires a Rust toolchain, `python3`, and a C compiler for `ring`, which
`libduckdb-sys`'s build script uses (the extension itself links no C).

```sh
duckdb-ext/build.sh      # builds and stamps both extensions, then code-db; prints their paths
duckdb-ext/check.sh      # fetches the scip CLI, fmt, clippy, tests of every member, build.sh, then the host check and code-db's
```

Output, in `duckdb-ext/target/extension/v2.0.0/linux_amd64/`:
`git_history.duckdb_extension` (about 5.9 MB) and
`scip_index.duckdb_extension` (about 0.6 MB), each with its own footer
(name, `v<crate version>`, the same platform, DuckDB version and ABI type);
both link only `libc` and `libgcc_s` dynamically. `check.sh` also needs
`curl` and `sha256sum` (for `scripts/fetch-scip-cli.sh`) and
`rust-analyzer` on `PATH` (`rustup component add rust-analyzer`).

## Loading

The extensions are unsigned, so the host must allow unsigned extensions and
`LOAD` them by absolute path. They are independent: load either or both
into the same connection.

Python (`duckdb==2.0.0`):

```python
import duckdb
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.sql("LOAD '/abs/path/git_history.duckdb_extension'")
con.sql("LOAD '/abs/path/scip_index.duckdb_extension'")
con.sql("SELECT * FROM git_hunks('.') WHERE path LIKE '%.rs'")
con.sql("SELECT * FROM read_scip('target/code-db/index.scip') WHERE is_definition")
```

Rust, with the `duckdb` crate linked against the machine-installed
`libduckdb` 2.0.0 the way Horizon links it (AGENTS.md "Build setup"):

```rust
let config = duckdb::Config::default().allow_unsigned_extensions()?;
let conn = duckdb::Connection::open_in_memory_with_flags(config)?;
conn.execute_batch("LOAD '/abs/path/git_history.duckdb_extension'")?;
```

`host-check/tests/load.rs` does exactly this on a fixture repository, and
loads both extensions into one connection for the SCIP functions on a
hand-encoded index. It is
a workspace of its own because cargo unifies features within a workspace:
next to `duckdb-neo`, `libduckdb-sys` would get `loadable-extension-v2`,
which stops it from linking `libduckdb`, and the host would not link.
Horizon's daemon could load the extension the same way; nothing in Horizon
does so yet. `code-db/` is a host too, in a workspace of its own for the
same reason: it opens an in-memory database through the crate's C API
(`duckdb::ffi`; the crate's `Connection` does not expose the raw connection,
and DuckDB variables belong to one connection), and lets `00_load.sql` load
the extensions. Its dependencies besides `duckdb` are `serde` and `toml`
(the settings file), `libc` (`statvfs`, the spill file system's free
space) and `unicode-width` (the markdown renderer's column widths).

The DuckDB CLI: `duckdb -unsigned -c "LOAD '/abs/path/git_history.duckdb_extension'"`,
only with a v2.0.0 CLI (`~/.local/bin/duckdb2 -unsigned -no-agent` loads
both here). The system CLI here is v1.5.0 and refuses it:

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

### `git_tree(repo, rev := 'HEAD')`

`(path VARCHAR, mode VARCHAR, blob_id VARCHAR)`

The tree of one revision, not a history range and not the worktree: one
row per leaf entry of `git ls-tree -r <rev>`, reached recursively from the
tree `rev` names, sorted by path bytes. `repo` is resolved like the other
functions' (any path inside a worktree, or a bare repository). `rev` is any
revision gix's `rev_parse_single` accepts: a commit-ish (a full or
abbreviated id, a branch, a lightweight or annotated tag, `HEAD~2`), peeled
to its tree, or a tree id, taken as is (`HEAD:src` names a subtree, whose
paths are then relative to it, as with `git ls-tree -r HEAD:src`).

| Entry | `mode` | `blob_id` |
| --- | --- | --- |
| regular file | `100644`, or `100755` if executable | the blob |
| symlink | `120000` | the blob holding the link's target path |
| gitlink (submodule) | `160000` | the commit the tree records; submodules are never opened |
| directory | no row | |

`path` is repository-relative (relative to the tree `rev` names) and
unquoted (`core.quotePath` does not apply). `mode` is git's canonical mode
string, as `git ls-tree` prints it. The revision is resolved at bind time:
an unknown one, or one naming a blob, is an error that names it
(`git_tree: cannot resolve revision 'nope': …`), and a NULL `rev` is an
error, not `HEAD`. The scan lists the tree resolved at bind time, so a
branch that moves in between does not change the result of a query.

With `git_changed_blobs`, the units of an earlier commit come from the
blob cache without a checkout: the `.rs` blobs of `git_tree(repo, rev)`
are files `git_changed_blobs(repo, dir, extensions := ['rs'])` has
written, provided the blob entered history through a non-merge commit (see
"Parity" for the one exception here and `include_merges`).

```sql
SELECT t.path, b.file
FROM git_tree('/repo', rev := 'e223bc20') t
JOIN (SELECT DISTINCT blob_id, file FROM blobs) b USING (blob_id)
WHERE t.path LIKE '%.rs';
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

`git_tree` is `git-history-core/src/tree.rs`:

| Step | gix call and settings |
| --- | --- |
| Revision | `repo.rev_parse_single(rev)`, `.object()`, `.peel_to_tree()` (a tag or commit is peeled, a tree kept), at bind time |
| Entries | `repo.find_tree(id).traverse().breadthfirst(&mut gix::traverse::tree::Recorder::default())`, which records every entry with its full path and does not enter gitlinks |
| Mode | the entry's `EntryMode::kind()`: `Blob` `100644`, `BlobExecutable` `100755`, `Link` `120000`, `Commit` `160000`, `Tree` dropped; a non-canonical mode stored in an old tree (such as `100664`) maps to its canonical string by the owner's executable bit, the rule git applies when reading a tree (by reading both sources; no fixture has such a tree) |
| Order | the records sorted by path bytes (the same order as `git ls-tree -r`, whose tree order compares a directory as `name/`) |

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
`gix-probe REPO hunks|commits|file_changes|tracked_files|changed_blobs|tree
[--since REV] [--include-merges] [--context N] [--include-untracked]
[--dir DIR] [--extensions A,B] [--rev REV] [--stats]` prints any function's
rows as TSV (`tree` lists `--rev`, default `HEAD`); for `changed_blobs`, `--stats` also prints the files written (with
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

`git_tree` on this repository at d7441359 (the commit that added it; clean
worktree), compared with `git ls-tree -r -z <rev>` (mode, type, id, path):

| Comparison | git_tree | Reference | Only in git_tree / only in reference |
| --- | --- | --- | --- |
| `rev := 'HEAD'` vs `git ls-tree -r HEAD` (864 `100644`, 18 `100755`) | 882 | 882 | 0 / 0, same order |
| `rev := 'e223bc20'` (558 `100644`, 9 `100755`) | 567 | 567 | 0 / 0, same order |
| `rev := '46c37290'` (732 `100644`, 9 `100755`) | 741 | 741 | 0 / 0, same order |
| `(path, mode, blob_id)` at HEAD vs `git_tracked_files(repo)` (0 modified) | 882 | 882 | 0 / 0 |

This history has no symlink or gitlink in these trees; the fixture test
covers both. The extension's rows are identical, in order, to `gix-probe
tree`'s TSV.

Blob coverage, the precondition for taking an earlier commit's units from
the blob cache: of the 379 `.rs` entries of `git_tree(repo, rev :=
'e223bc20')`, 378 blob ids occur among the rows of
`git_changed_blobs(repo, dir, extensions := ['rs'])` over the whole
history (at d7441359), and 379 with `include_merges := true`. The missing
blob is `src/workspace/bindings.rs` at 92254c9a, which first appears in the
merge 314e18db ("Merge pull request #57") and differs from the file in
both of its parents: a content made by the merge itself, which only a diff
of the merge (against its first parent) lists. Every file of the 379 is
present in `dir` after the `include_merges` run.

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

`git_tree` on this repository at d7441359, Python `duckdb` 2.0.0, `SELECT
count(*) FROM git_tree('.', rev := …)`, five fresh processes each (the time
of the query after `LOAD`, bind included):

| `rev` | Rows | Wall time |
| --- | --- | --- |
| `HEAD` | 882 | 8.4–21.9 ms |
| `e223bc20` | 567 | 6.7–10.1 ms |
| `46c37290` | 741 | 8.0–9.8 ms |

`gix-probe tree` takes 2–4 ms for each.

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
- `git-history-core/tests/tree.rs`: builds a fixture (same isolation) with
  two commits on `main`, a branch and a lightweight and an annotated tag
  on the first: files in nested directories, names that sort differently
  as a directory (`s-x.txt`, `s.txt`, `s/y.txt`), non-ASCII and space in
  names, an executable, a symlink, a gitlink added with `update-index`, and
  in the second commit a deletion, a rename into a new directory and a
  modification. It compares `tree` with `git ls-tree -r -z` (mode, type,
  id, path, in order) for `HEAD`, `main`, `HEAD~1`, the branch, both tags,
  the full and an abbreviated commit id, both commits' tree ids and the
  subtree `HEAD:src`; asserts the order is by path bytes, that two runs
  agree, each entry kind's mode and id (a symlink's blob is its target, a
  gitlink's id the recorded commit), that no directory has a row, and the
  deletion, rename and modification between the commits; and asserts that
  an unknown name, a revision past the root, a missing full id and a blob
  (`HEAD:a.txt`) are errors naming the revision.
- `host-check/tests/load.rs`: loads the built extension into the `duckdb`
  crate with the machine's `libduckdb` 2.0.0 and checks `git_hunks` joined
  with `git_commits`, `git_tree` at `HEAD~1` and `HEAD` with the
  bind-time error for an unknown `rev`, and `git_tracked_files` with
  `include_untracked`, on a fixture; a second test loads both extensions into one connection and
  checks the three SCIP functions on a hand-encoded index (packed and typed
  ranges, a NULL enclosing range, the role booleans, a list column, an
  external symbol, the metadata counts) and the bind-time error for a
  missing file.
- `code-db/`: unit tests of the settings file, the limits, the markdown
  renderer and the statement splitter, and `tests/end_to_end.rs`, which runs
  the built command on a fixture repository with a `.code-db.toml`
  (`sql/README.md`, Check).

## `scip_index`

Three table functions over a [SCIP](https://github.com/scip-code/scip)
index (the protobuf file `rust-analyzer scip . --output
target/code-db/index.scip` writes), for the code database's fan-in: how
many files reference a unit's definitions. They return the facts as SCIP
encodes them; converting to 1-based positions, resolving symbols to units
and counting belong to the SQL layer.

Each takes `path VARCHAR`, a constant: a local file opened by the extension
itself (not through DuckDB's file system, so no `s3://` or globs), relative
paths resolved against the DuckDB process's working directory. A missing or
unreadable file is an error at bind time. Rows are emitted from one thread
in file order: documents in index order, each document's occurrences or
symbols in its order, external symbols where they occur in the file
(rust-analyzer writes them after the documents). A malformed index (a
truncated field, invalid UTF-8 in a string, a range of other than three or
four elements) fails the query with an error naming the document. The Go
reference decoder rejects truncation and invalid UTF-8 too; it does not
check range lengths, but `scip.proto` says a packed range "must be
exactly three or four elements", and a row with no position cannot fill
the range columns.

### `read_scip(path)`

`(document VARCHAR, symbol VARCHAR, start_line INTEGER, start_char INTEGER, end_line INTEGER, end_char INTEGER, roles INTEGER, is_definition BOOLEAN, is_import BOOLEAN, is_write BOOLEAN, is_read BOOLEAN, is_generated BOOLEAN, is_test BOOLEAN, is_forward_definition BOOLEAN, enclosing_start_line INTEGER, enclosing_start_char INTEGER, enclosing_end_line INTEGER, enclosing_end_char INTEGER)`

One row per `Occurrence` of each `Document`.

- `document` is the document's `relative_path` (relative to the metadata's
  `project_root`); `''` if the document has none.
- `symbol` is the raw symbol string, `''` for an occurrence without one
  (SCIP allows syntax-highlighting-only occurrences; this repository's
  index has none).
- The range is half-open, `[start, end)`, **0-based** lines and characters.
  A typed range (`single_line_range`, `multi_line_range`) takes precedence
  over the deprecated packed `range`, as `scip.proto` specifies; a
  single-line range, typed or a three-element packed `[line, start, end]`,
  has `end_line = start_line`. Characters are in the unit of the document's
  `position_encoding`, which is not a column: rust-analyzer writes
  `UTF8CodeUnitOffsetFromLineStart` (byte offsets within the line) for every
  document.
- `roles` is the raw `symbol_roles` bitmask, bits beyond the defined roles
  included; the booleans test one bit each: `is_definition` 0x1,
  `is_import` 0x2, `is_write` 0x4 (`WriteAccess`), `is_read` 0x8
  (`ReadAccess`), `is_generated` 0x10, `is_test` 0x20,
  `is_forward_definition` 0x40.
- The enclosing range (for a definition, the whole definition node with its
  documentation) follows the same rules from `typed_enclosing_range` or the
  packed `enclosing_range`; all four columns are NULL when the occurrence
  has neither.

**Local symbols.** A symbol `local N` is scoped to its document: the same
string in two documents names two unrelated symbols, so locals join on
`(document, symbol)` and global symbols on `symbol` alone. Global symbols
follow SCIP's grammar `<scheme> <manager> <package> <version>
<descriptors>`; rust-analyzer's are `rust-analyzer cargo <crate> <version>
<path>`, such as `rust-analyzer cargo horizon 0.1.0 workspace/WorkspaceShell#`.
On this repository 82,970 of the 301,687 occurrences are locals.

**Roles.** An occurrence without the definition bit is a reference. The
other bits are only as good as the indexer: rust-analyzer 1.99.1 sets the
definition bit and nothing else (on this repository 42,376 occurrences
have `roles = 1`, all others `0`; no import, read, write, test or
generated bit occurs), so `is_read`/`is_write` cannot separate reads from
writes for Rust, and `NOT is_definition` is the reference test. A file's
module or crate symbol has a definition occurrence spanning the file (a
four-element packed range such as `[0, 0, 14, 0]`).

### `read_scip_symbols(path)`

`(document VARCHAR, symbol VARCHAR, kind VARCHAR, display_name VARCHAR, enclosing_symbol VARCHAR, documentation VARCHAR[], relationship_count INTEGER)`

One row per `SymbolInformation`: those of each document (`document` is its
`relative_path`) and those of `Index.external_symbols` (`document` NULL).
`kind` is the `Kind` enum's name (`Function`, `Struct`, ...;
`UnspecifiedKind` for 0, the value in decimal for a number this
`scip.proto` does not define). `display_name` and `enclosing_symbol` are
`''` when absent. `documentation` is the list in order, empty when absent;
rust-analyzer writes the doc comment as one element (with its newlines) and
the signature in `signature_documentation`, which is not read.
`relationship_count` is the number of `Relationship` entries
(rust-analyzer 1.99.1 writes none; the relationships themselves are not
returned).

### `read_scip_metadata(path)`

`(version INTEGER, tool_name VARCHAR, tool_version VARCHAR, tool_arguments VARCHAR[], project_root VARCHAR, text_document_encoding VARCHAR, document_count BIGINT, occurrence_count BIGINT, external_symbol_count BIGINT)`

One row. `version` is the raw `ProtocolVersion` value (0,
`UnspecifiedProtocolVersion`, is the only one defined);
`tool_name`/`tool_version`/`tool_arguments` are `ToolInfo`'s;
`project_root` is the URI as written (`file:///...`);
`text_document_encoding` is the `TextEncoding` name
(`UnspecifiedTextEncoding`, `UTF8`, `UTF16`, or the number). The counts are
the index's `Document`, `Occurrence` (over all documents) and
`external_symbols` entries, so this function reads the whole file;
`occurrence_count` equals `count(*)` of `read_scip`.

### Decoding

`scip-index-core` is a hand-written protobuf wire-format reader (`wire.rs`:
varints, field keys, length-delimited fields, skipping by wire type), not
`prost` with a vendored `scip.proto`. Generated code needs a build step
(`prost-build` with `protox` to avoid `protoc`) with their dependency
trees, and `prost`'s `Index::decode` materializes the whole index: to
stream, the top-level loop would have been hand-written anyway. The cost is
that the field numbers are maintained by hand: the module doc of
`scip-index-core/src/lib.rs` lists every field read with its number at the
pinned commit, and a `scip.proto` change has to be applied there; the
parity tests against the Go reference catch a mismatch on the encodings
they exercise.

Protobuf's rules are followed where they matter: unknown fields, and known
fields with an unexpected wire type, are skipped (groups, which proto3 does
not have, are an error); `repeated int32` is read packed or unpacked; a
singular message field that occurs twice is merged; within a oneof the
last member wins; `int32` values are sign-extended ten-byte varints.

Streaming: `Index` is one message whose `metadata`, `documents` and
`external_symbols` are top-level length-delimited fields, so the reader
reads one top-level field at a time from a buffered file into a reused
buffer, decodes it, and emits its rows before reading the next; other
top-level fields are skipped without being read into memory. The largest
document of this repository's index is 775,295 bytes, and `scip-probe`
peaks at 11.5 MiB RSS over the 30 MB index; `SELECT count(*) FROM
read_scip(…)` adds 4–5 MiB to the 70 MiB of a DuckDB process after
`LOAD`.

### Parity

`scip-index-core/tests/cli_parity.rs` compares the three row sets with rows
built from `scip print --json` of the `scip` CLI v0.10.0, as multisets:

| Index | `read_scip` | `read_scip_symbols` | Metadata row | Only ours / only reference |
| --- | --- | --- | --- | --- |
| fixture crate indexed by `rust-analyzer scip` in a temp dir (2 documents) | 85 | 27 | equal | 0 / 0 |
| hand-encoded index (typed ranges, every role bit, negative values, unknown fields at every level, relationships, external symbols) | 5 | 4 | equal | 0 / 0 |
| this repository at 20941942, 30 MB (509 documents, 9 external symbols) | 301,687 | 42,839 | equal | 0 / 0 |

No difference to characterize. What the rust-analyzer indexes cannot
exercise, because rust-analyzer 1.99.1 never writes it: typed ranges (all
its ranges are packed: 3 elements, or 4 for the 683 multi-line ones),
role bits other than the definition bit, relationships, and syntax-only
occurrences; the hand-encoded index covers those against the same Go
decoder. The extension's rows are identical, in order, to `scip-probe`'s
TSVs for all three functions on all three indexes
(`scripts/scip_ext_parity.py`).

```sh
SCIP_INDEX=/abs/target/code-db/index.scip cargo test --release -p scip_index_core \
    --test cli_parity -- --ignored --nocapture
uv run --with duckdb==2.0.0 python duckdb-ext/scripts/scip_ext_parity.py \
    "$PWD/duckdb-ext/target/extension/v2.0.0/linux_amd64/scip_index.duckdb_extension" \
    duckdb-ext/target/release/scip-probe target/code-db/index.scip
```

`scip-probe INDEX occurrences|symbols|metadata [--stats]` prints the
extension's columns as TSV (NULL as `\N`, strings escaped as JSON string
content, lists as JSON arrays).

### Performance

The index of this repository at 20941942: `rust-analyzer scip .` took
202.6 s with a peak RSS of 7.0 GiB and wrote 30,385,713 bytes (and 32
"Duplicate symbol" lines, noise). Python `duckdb` 2.0.0, a fresh process
per query (`scripts/scip_perf.py`), three runs, on a shared 24-thread
desktop; `CREATE TABLE` is into an in-memory database:

| Query | Rows | Wall time | Peak RSS (70 MiB after `LOAD`) |
| --- | --- | --- | --- |
| `SELECT count(*) FROM read_scip(…)` | 301,687 | 0.074–0.076 s | 74 MiB |
| `CREATE TABLE t AS SELECT * FROM read_scip(…)` | 301,687 | 0.12–0.23 s | 121 MiB |
| `SELECT count(*) FROM read_scip_symbols(…)` | 42,839 | 0.026–0.027 s | 73 MiB |
| `CREATE TABLE t AS SELECT * FROM read_scip_symbols(…)` | 42,839 | 0.034–0.035 s | 83 MiB |
| `SELECT * FROM read_scip_metadata(…)` (count or table) | 1 | 0.008–0.013 s | 73 MiB |

For the other route (convert, then `read_json`): `scip print --json` of the
same index takes 0.36–0.43 s with a peak RSS of 288–303 MiB and writes
59,372,955 bytes of JSON, about twice the index; reading that JSON into
tables was not measured. `scip-probe` takes 0.15 s for the occurrences (TSV
writing included), 0.04 s for the symbols, at 11.5 MiB peak RSS.

### Tests

- `scip-index-core/src/tests.rs`, on hand-made protobuf bytes: packed
  legacy ranges of three and four elements (also unpacked, and split over
  two fields), typed single- and multi-line ranges (including an empty
  one), typed-over-packed precedence, oneof switching and same-member
  merging, malformed ranges as errors, every role bit and bits beyond them,
  negative `int32`s, missing, packed and typed enclosing ranges, unknown
  fields and wrong wire types skipped at every level, file order with the
  path after the occurrences and an empty document, `local` symbols kept
  raw, document and external symbols with documentation lists,
  relationships and unknown kinds, metadata with merged fields and counts,
  the kind table, and truncated or invalid input.
- `scip-index-core/tests/cli_parity.rs`: the parity above for the fixture
  crate (written to a temp dir and indexed with `rust-analyzer scip`; it
  also asserts that the fixture produces locals, definitions and references,
  rows with and without an enclosing range, a multi-line range and
  multi-line documentation) and the hand-encoded index; the ignored
  `repository_index_parity` takes an index from `SCIP_INDEX`.
- `host-check/tests/load.rs` (above).
