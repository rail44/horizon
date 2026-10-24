# DuckDB extensions

DuckDB extensions for the code database (`scripts/code-db/`), written in
Rust. This directory is a Cargo workspace of its own (own `Cargo.lock`, own
`target/`), excluded from the root workspace like `preview-plugin/`. The
root quality gate does not read it; `./check.sh` is its gate.

| Crate | Output |
| --- | --- |
| `git-history/` | `git_history.duckdb_extension`: the table functions `git_hunks`, `git_commits`, `git_file_changes` |
| `host-check/` | no output; a test that loads the built extension through the `duckdb` crate (a separate workspace, see below) |
| `gix-probe/` | a measurement binary: the same three row sets computed with gitoxide only, compared with the git CLI by `scripts/gix_parity.py` (findings: `docs/research/gix-parity-2026-10-24.md`) |

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
| `git2` | 0.20.4 with `vendored-libgit2` (libgit2 1.9.7 via `libgit2-sys` 0.18.8), no default features (no network transports) |
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

## Build

Requires a Rust toolchain, a C compiler (libgit2 is compiled from the
vendored source) and `python3`.

```sh
duckdb-ext/build.sh      # prints the path of the built extension
duckdb-ext/check.sh      # fmt, clippy, tests, build.sh, then the host check
```

Output: `duckdb-ext/target/extension/v2.0.0/linux_amd64/git_history.duckdb_extension`
(about 1.6 MB; links only `libc`, `libgcc_s` and `libz.so.1` dynamically).

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

All three take `repo VARCHAR` (any path inside a worktree, or a bare
repository; it must be a constant) and two optional named parameters:
`since VARCHAR` (any revision; history is `since..HEAD`, default all of
`HEAD`) and `include_merges BOOLEAN` (default `false`). Rows are computed
when the scan starts and emitted from one thread, so an unordered query
returns them in the order below. Column names and semantics are those of the
spike's tables (`scripts/code-db/schema.sql`, `history.py`).

### `git_hunks(repo, since := …, include_merges := …)`

`(sha VARCHAR, path VARCHAR, old_start INTEGER, old_lines INTEGER, new_start INTEGER, new_lines INTEGER)`

One row per `@@ -old_start,old_lines +new_start,new_lines @@` header of
`git log -p -U0 -M --no-ext-diff --no-textconv --diff-merges=off`
(`--diff-merges=first-parent` with `include_merges`), with git's 1-based
numbers (a count of 0 means a pure insertion or deletion, and its start is
the line before). `path` is the new path, or the old one for a deletion.
Unlike the spike, which passes language pathspecs to git, this covers all
files; filter in SQL (`WHERE path LIKE '%.rs'`). Binary files have no rows;
a file/symlink type change has the hunks of a deletion followed by a
creation, as git prints it.

### `git_commits(repo, since := …, include_merges := …)`

`(sha VARCHAR, author_time TIMESTAMP, author_name VARCHAR, subject VARCHAR, parent_count INTEGER, is_merge BOOLEAN)`

The commits `git log [since..]HEAD` lists, in its order. `author_time` is
the author timestamp in UTC (`%at`), `author_name` is `%an` (no mailmap),
`subject` is `%s` (the first paragraph, lines joined with spaces).
`include_merges` does not change this table.

### `git_file_changes(repo, since := …, include_merges := …)`

`(sha VARCHAR, path VARCHAR, old_path VARCHAR, status VARCHAR, added INTEGER, deleted INTEGER)`

One row per changed file of `git log --raw --numstat -M`: `status` is git's
letter (`A`, `M`, `D`, `R`, `T`; `C` cannot occur with `-M`), `old_path` is
set for renames, `added`/`deleted` are the `--numstat` counts and NULL for
binary files. Merges have no rows unless `include_merges`.

## How git's output is reproduced

libgit2 provides object access, the tree diff and the line diff (its xdiff
is git's; libgit2 1.9.7's copy differs from git v2.55.0's by refactoring,
a faster line hash and a histogram-only change, none of which changes a
Myers diff). Where libgit2's diff machinery behaves
differently from `git log`, the extension follows git's implementation
instead; each place names the git source it copies:

| Behavior | libgit2 | What the extension does |
| --- | --- | --- |
| `-U0` hunk positions | diffs whole files | applies git's `trim_common_tail` (`xdiff-interface.c`) before diffing, as git does whenever there are no context lines. Without it, 142 of git's 44,009 hunk rows on this repository came out differently placed or split (the trimmed tail bounds how far xdiff slides a change down) |
| Rename detection (`-M`) | `find_similar` with its own similarity metric and no basename pass | git's `diffcore-rename.c`: exact renames, then unique-basename pairs at 75 %, then the candidate matrix with git's span-hash similarity (`diffcore-delta.c`), 50 %, rename limit 1000. With `find_similar` (renames, 50 %, limit 1000), 6 commits of this repository paired files differently |
| Binary files | its own heuristic (also counts non-printable bytes) | git's rule: the `diff` attribute if set, else a NUL in the first 8000 bytes |
| Commit order | time-sorted walk with unstable ties | git's `revision.c` walk: a committer-time priority queue whose ties go to the commit queued first, and `limit_list` for `since..HEAD`. With libgit2's walk, 8 commits made within the same second as a neighbor were reordered |
| `--numstat` | — | counts from a diff without tail trimming, as git computes them with the default three context lines |

Not reproduced, because nothing here needs it: `.mailmap`, textconv and
external diff drivers (the spike disables both), `diff.*` configuration,
copy detection, pathspec history simplification, `diff.renameLimit` other
than 1000, submodule diffs beyond git's `Subproject commit` line.

## Parity

Measured on this repository (1,586 commits, 343 merges) at ed63e944 against
git 2.55.0 and the spike database built with
`uv run --locked --script scripts/code-db/build.py build . --out target/code-db/horizon.duckdb --languages rust`:

| Comparison (multisets) | Extension | Reference | Difference |
| --- | --- | --- | --- |
| `git_hunks` vs `git log -p -U0 -M` (all files) | 44,009 | 44,009 | none |
| same with `include_merges` vs `--diff-merges=first-parent` | 64,324 | 64,324 | none |
| `git_file_changes` vs `git log --raw --numstat -M` | 8,969 | 8,969 | none |
| same with `include_merges` | 13,133 | 13,133 | none |
| `git_commits` order vs `git log --format=%H`, for no `since` and nine `since` values | 1,586 | 1,586 | none |
| `git_hunks` filtered to `*.rs` vs spike `hunks` | 34,691 | 34,690 | 1 extra row (below) |
| `git_commits` vs spike `commits` | 1,586 | 1,586 | none |
| `git_file_changes` vs spike `file_changes` | 8,969 | 8,969 | none |

The extra row is the spike's, not a git difference: the spike runs
`git log -p … -- '*.rs'`, and a pathspec turns on git's history
simplification, which drops commits on a side branch whose merge is
TREESAME to the other parent for those paths. Commit e91103e8 ("Merge
fixup: latency probe follows the v6 Hello shape", reached through merge
4ba1ceff) deletes one line of `crates/horizon-sessiond/tests/latency_probe.rs`
(`@@ -142 +141,0 @@`); `git log -- '*.rs'` omits it, `git log --full-history
-- '*.rs'` and plain `git log -p` show it. Filtering an all-files result in
SQL cannot reproduce that simplification, and arguably should not.

Scripts (Python, run with `uv run --with duckdb==2.0.0 python …`):
`scripts/parity.py` (against a spike database), `scripts/cli_hunks.py` and
`scripts/cli_file_changes.py` (the git CLI output, parsed like the spike),
`scripts/load_smoke.py`, `scripts/perf.py`; the crate's `dump` example
prints any function's rows as TSV (`cargo run --release --example dump --
REPO hunks|commits|file_changes [SINCE] [--include-merges]`).

## Performance

This repository, Python `duckdb` 2.0.0, `SELECT count(*)`, three runs each in
a fresh process, on a shared 24-thread desktop:

| Function | Rows | Wall time | Peak RSS |
| --- | --- | --- | --- |
| `git_hunks` | 44,009 | 2.8–2.9 s | 145 MiB (68 MiB after `LOAD`) |
| `git_file_changes` | 8,969 | 3.2–4.5 s | 144 MiB |
| `git_commits` | 1,586 | 0.03 s | 81 MiB |

For comparison, the spike's whole history phase (including parsing blobs for
`unit_changes`) took 24 s in the build above, and the git CLI produces the
same hunks in 0.8 s and the same file changes in 0.9 s. Of the extension's
time, about 0.5 s is tree diffs and rename detection; the rest is reading
blobs through libgit2 (about 1.2 s, packed deltas) and libgit2's patch
generation (about 1.4 s). The scan is single-threaded; diffing commits in
parallel, or caching blobs (`GIT_OPT_SET_CACHE_OBJECT_LIMIT`, measured at
about −0.4 s), are the obvious next steps if this matters.

## Tests

- `git-history/src/*.rs` unit tests: `%s` subject folding, `trim_common_tail`,
  `basename_same`, span hashing.
- `git-history/tests/cli_parity.rs`: builds a fixture repository with the
  `git` CLI (fixed names and dates, no user or system configuration) with
  several commits, a multi-hunk change, an insertion-only and a
  deletion-only change, a rename with and without content change, a binary
  file, a file turned into a symlink and a merge commit, then compares all
  three functions with the CLI's output for four ranges (with and without
  `since`, with and without `include_merges`) and asserts each case's rows.
- `host-check/tests/load.rs`: loads the built extension into the `duckdb`
  crate with the machine's `libduckdb` 2.0.0 and checks `git_hunks` joined
  with `git_commits` on a fixture.
