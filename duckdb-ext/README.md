# DuckDB extensions

DuckDB extensions for the code database (a repository's current tree as
language-independent tables, queried in SQL; `sql/README.md`), written in Rust.
This directory is a Cargo workspace of its own (own `Cargo.lock`, own
`target/`), excluded from the root workspace like `preview-plugin/`. The
root quality gate does not read it; `./check.sh` is its gate.

| Crate | Output |
| --- | --- |
| `git-history-core/` | a library: the rows of `git_tracked_files`, computed with gitoxide (`gix`) only; no DuckDB dependency, compared with the git CLI on a fixture |
| `git-history/` | `git_history.duckdb_extension`: the DuckDB table function `git_tracked_files` over `git-history-core` |
| `scip-index-core/` | a library: a streaming decoder of SCIP indexes (protobuf) and the rows of the three SCIP functions; no dependencies, unit-tested and compared with `scip print --json` |
| `scip-index/` | `scip_index.duckdb_extension`: the DuckDB table functions `read_scip`, `read_scip_symbols`, `read_scip_metadata` over `scip-index-core`, each over one index or a list (see "`scip_index`" below) |
| `scip-probe/` | a binary printing any SCIP function's rows of `scip-index-core` as TSV, compared with the extension by `scripts/scip_ext_parity.py` |
| `host-check/` | no output; a test that loads both built extensions through the `duckdb` crate (a separate workspace, see below) |
| `code-db/` | `code-db`, the code database's entry point (`sql/README.md`, Running): reads the analysed repository's `.code-db.toml`, derives DuckDB's limits, runs the indexers (one SCIP index per Cargo workspace, jscpd over the files in scope), and loads the extensions and the SQL views through the `duckdb` crate (a separate workspace, like `host-check/`) |

The history functions (`git_hunks`, `git_commits`, `git_file_changes`,
`git_changed_blobs`, `git_tree`), their probe `gix-probe` and parity scripts
were removed on 2026-10-29 with the code database's history (owner decision;
`docs/research/code-db-scip-rebuild-2026-10-29.md`); they are in the history
at af0d26a7, with their parity and performance records.

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
| `gix` | 0.90.0, no default features, features `sha1`, `max-performance`, `status` (resolves `gix-status` 0.37.0, `gix-dir` 0.32.0, `gix-filter` 0.36.1, `gix-odb` 0.87.0, `gix-pack` 0.77.0; `status` still pulls `gix-diff` 0.70.0 and `gix-imara-diff` 0.3.2) |
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
normal --prefix none -f "{p} {l}"`, 120 crates besides the two of this
workspace since the history functions' `blob-diff` and `revision` features
were dropped; the license set did not change) has these licenses:

| License | Crates |
| --- | --- |
| MIT OR Apache-2.0 (and spellings such as `MIT/Apache-2.0`), including every `gix-*` crate but one | most |
| Apache-2.0 only | `gix-imara-diff` (gix's line diff), `unicode-bom` |
| MIT only | `clru`, `darling`, `darling_core`, `dashmap`, `duckdb-neo`, `faster-hex`, `libduckdb-sys`, `nonempty`, `prodash`, `strsim` |
| Unlicense OR MIT | `byteorder`, `jiff`, `jiff-core`, `memchr`, `same-file`, `walkdir` |
| Zlib | `foldhash`, `zlib-rs` |
| MPL-2.0 | `uluru` (the static pack cache that gix's `max-performance` enables) |
| others | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT`, `(MIT OR Apache-2.0) AND Unicode-3.0`, `(Apache-2.0 OR MIT) AND BSD-3-Clause`, `Zlib OR Apache-2.0 OR MIT` |

`max-performance` was kept for the history functions, whose pack reads it
sped up (measured then: 0.98–1.03 s without it against 0.86–0.93 s for the
hunks of this repository); `git_tracked_files` reads few objects, and the
feature was left as it was. MPL-2.0 is file-level copyleft and binds only changes to `uluru`'s own
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
`git_history.duckdb_extension` (about 4.7 MB) and
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
con.sql("SELECT * FROM git_tracked_files('.') WHERE path LIKE '%.rs'")
con.sql("SELECT * FROM read_scip(['target/code-db/scip/root.scip']) WHERE is_definition")
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
hand-encoded index (one path and a list). It is
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

### `git_tracked_files(repo, include_untracked := …)`

`(path VARCHAR, mode VARCHAR, blob_id VARCHAR, is_tracked BOOLEAN, is_modified BOOLEAN)`

The worktree's files now: one row per path of `git ls-files`, sorted by path
bytes. `repo` is any path inside a worktree (a bare repository is an error;
it must be a constant); `include_untracked BOOLEAN` (default `false`) adds
the files of `git ls-files -o --exclude-standard`. Rows are computed when the
scan starts and emitted from one thread. `path` is repository-relative and
unquoted (`core.quotePath` does not apply). `mode` is git's mode string
(`100644`, `100755`, `120000`, `160000`) and `blob_id` the object id of the
**current** content, both as `git add` would record them now:

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

## How the rows are computed

Everything is gitoxide. `git_tracked_files` is `git-history-core/src/tracked.rs`:

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

Measured before the probe and its script were removed (they are in the
history at af0d26a7): `git_tracked_files`, with `scripts/tracked_parity.py`
(rows as sets; the
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

## Performance

`git_tracked_files` on this repository at 2a0de30a (824 tracked files, 6
modified, 3 untracked), Python `duckdb` 2.0.0, `SELECT count(*)`, five fresh
processes each, on a shared 24-thread desktop:

| Call | Rows | Wall time | Peak RSS (one scan) |
| --- | --- | --- | --- |
| `git_tracked_files('.')` | 824 | 9–31 ms | 77 MiB (70 MiB after `LOAD`) |
| `git_tracked_files('.', include_untracked := true)` | 827 | 10–51 ms | 76 MiB |

`gix-probe tracked_files` (since removed) took 5.6–6.2 ms (min–median of 20
runs; 5.7–6.9 ms with `--include-untracked`), against 3.7–3.9 ms for `git ls-files -s`,
4.1–5.5 ms for `git ls-files -o --exclude-standard`, and 4.9–7.7 ms for
`git status --porcelain=v2`. Files whose index stat is current are not
read; a file is hashed only when its stat differs from the index (or is
racily clean), and gix runs that check on several threads.

## Tests

- `git-history-core/tests/tracked_files.rs`: builds a worktree with the
  `git` CLI (fixed names and dates, no user or system configuration) holding a tracked unchanged file, a file
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
- `host-check/tests/load.rs`: loads the built extension into the `duckdb`
  crate with the machine's `libduckdb` 2.0.0 and checks `git_tracked_files`
  with `include_untracked` on a fixture; a second test loads both extensions
  into one connection and checks the three SCIP functions on a hand-encoded
  index (packed and typed ranges, a NULL enclosing range, the role booleans,
  a list column, the signature, an external symbol, the metadata counts),
  the same index twice as a list (rows in order, `index_path`), an empty
  list, and the bind-time error for a missing file.
- `code-db/`: unit tests of the settings file, the limits, the markdown
  renderer, the statement splitter and workspace discovery, and
  `tests/end_to_end.rs`, which runs the built command (`index` included) on a
  fixture repository with a `.code-db.toml` (`sql/README.md`, Check).

## `scip_index`

Three table functions over [SCIP](https://github.com/scip-code/scip)
indexes (the protobuf files `rust-analyzer scip . --output FILE` writes;
`code-db index` writes one per Cargo workspace), the code database's source
of units and references. They return the facts as SCIP encodes them;
converting to 1-based positions, mapping documents to repository paths,
resolving symbols to units and counting belong to the SQL layer.

Each takes `path`, a constant: a `VARCHAR` or a `VARCHAR[]` list of paths
(an empty list gives no rows), each a local file opened by the extension
itself (not through DuckDB's file system, so no `s3://` or globs), relative
paths resolved against the DuckDB process's working directory. A missing or
unreadable file is an error at bind time. Every function ends with the
column `index_path VARCHAR`, the path of the index the row came from, as
given. Rows are emitted from one thread in list order and, within an index,
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

`(document VARCHAR, symbol VARCHAR, start_line INTEGER, start_char INTEGER, end_line INTEGER, end_char INTEGER, roles INTEGER, is_definition BOOLEAN, is_import BOOLEAN, is_write BOOLEAN, is_read BOOLEAN, is_generated BOOLEAN, is_test BOOLEAN, is_forward_definition BOOLEAN, enclosing_start_line INTEGER, enclosing_start_char INTEGER, enclosing_end_line INTEGER, enclosing_end_char INTEGER, index_path VARCHAR)`

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

`(document VARCHAR, symbol VARCHAR, kind VARCHAR, display_name VARCHAR, enclosing_symbol VARCHAR, documentation VARCHAR[], relationship_count INTEGER, signature VARCHAR, index_path VARCHAR)`

One row per `SymbolInformation`: those of each document (`document` is its
`relative_path`) and those of `Index.external_symbols` (`document` NULL).
`kind` is the `Kind` enum's name (`Function`, `Struct`, ...;
`UnspecifiedKind` for 0, the value in decimal for a number this
`scip.proto` does not define). `display_name` and `enclosing_symbol` are
`''` when absent. `documentation` is the list in order, empty when absent;
rust-analyzer writes the doc comment as one element (with its newlines), and
gives a trait impl's member the trait item's. `signature` is the `text` of
`signature_documentation` (a `Document` message, field 7; its other fields
are not read), NULL when absent: rust-analyzer writes the declaration's head
for every symbol (`pub(crate) fn spawn(…) -> T` with its `where` clause,
`pub struct S`), attributes and doc comments left out.
`relationship_count` is the number of `Relationship` entries
(rust-analyzer 1.99.1 writes none; the relationships themselves are not
returned).

### `read_scip_metadata(path)`

`(version INTEGER, tool_name VARCHAR, tool_version VARCHAR, tool_arguments VARCHAR[], project_root VARCHAR, text_document_encoding VARCHAR, document_count BIGINT, occurrence_count BIGINT, external_symbol_count BIGINT, index_path VARCHAR)`

One row per index. `version` is the raw `ProtocolVersion` value (0,
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
| the same tree at af0d26a7 (the signature column added), 30 MB | 301,687 | 42,839 | equal | 0 / 0 |

No difference to characterize. What the rust-analyzer indexes cannot
exercise, because rust-analyzer 1.99.1 never writes it: typed ranges (all
its ranges are packed: 3 elements, or 4 for the 683 multi-line ones),
role bits other than the definition bit, relationships, and syntax-only
occurrences; the hand-encoded index covers those against the same Go
decoder. The extension's rows are identical, in order, to `scip-probe`'s
TSVs for all three functions on all three indexes
(`scripts/scip_ext_parity.py`).

```sh
SCIP_INDEX=/abs/target/code-db/scip/root.scip cargo test --release -p scip_index_core \
    --test cli_parity -- --ignored --nocapture
uv run --with duckdb==2.0.0 python duckdb-ext/scripts/scip_ext_parity.py \
    "$PWD/duckdb-ext/target/extension/v2.0.0/linux_amd64/scip_index.duckdb_extension" \
    duckdb-ext/target/release/scip-probe target/code-db/scip/root.scip
```

`scip-probe INDEX occurrences|symbols|metadata [--stats]` prints the
extension's columns but `index_path` as TSV (NULL as `\N`, strings escaped
as JSON string content, lists as JSON arrays).

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
