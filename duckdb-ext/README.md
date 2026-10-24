# DuckDB extensions

DuckDB extensions for the code database (`scripts/code-db/`), written in
Rust. This directory is a Cargo workspace of its own (own `Cargo.lock`, own
`target/`), excluded from the root workspace like `preview-plugin/`. The
root quality gate does not read it; `./check.sh` is its gate.

| Crate | Output |
| --- | --- |
| `git-history-core/` | a library: the rows of the three functions, computed with gitoxide (`gix`) only; no DuckDB dependency, unit-tested and compared with the git CLI on a fixture |
| `git-history/` | `git_history.duckdb_extension`: the DuckDB table functions `git_hunks`, `git_commits`, `git_file_changes` over `git-history-core` |
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
| `gix` | 0.90.0, no default features, features `blob-diff`, `revision`, `sha1`, `max-performance` (resolves `gix-diff` 0.70.0, `gix-imara-diff` 0.3.2, `gix-traverse` 0.64.0, `gix-odb` 0.87.0, `gix-pack` 0.77.0) |
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
normal --prefix none -f "{p} {l}"`, 120 crates besides the two of this
workspace) has these licenses:

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
(about 4.6 MB; links only `libc` and `libgcc_s` dynamically).

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
spike's tables (`scripts/code-db/schema.sql`, `history.py`), with the
deviations listed under "How the rows are computed".

### `git_hunks(repo, since := …, include_merges := …)`

`(sha VARCHAR, path VARCHAR, old_start INTEGER, old_lines INTEGER, new_start INTEGER, new_lines INTEGER)`

One row per `@@ -old_start,old_lines +new_start,new_lines @@` header of
`git log -p -U0 -M --no-ext-diff --no-textconv --diff-merges=off`
(`--diff-merges=first-parent` with `include_merges`), with git's 1-based
numbers (a count of 0 means a pure insertion or deletion, and its start is
the line before). `path` is the new path, or the old one for a deletion.
Unlike the spike, which passes language pathspecs to git, this covers all
files; filter in SQL (`WHERE path LIKE '%.rs'`). Binary files and
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
external diff drivers (the spike disables both), the repository's `diff.*`
configuration, copy detection, pathspec history simplification,
`diff.renameLimit` other than 1000.

## Parity

Measured on this repository at 0df26a53 (1,589 commits, 343 merges)
against git 2.55.0 with `scripts/gix_parity.py` (rows as multisets, commit
order as sequences), and against the spike database built with
`uv run --locked --script scripts/code-db/build.py build . --out target/code-db/horizon.duckdb --languages rust`
with `scripts/parity.py`:

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
| `git_hunks` filtered to `*.rs` vs spike `hunks` | 34,694 | 34,702 | 116 / 124 |
| `git_commits` vs spike `commits` | 1,589 | 1,589 | 0 / 0 |
| `git_file_changes` vs spike `file_changes` | 9,008 | 9,008 | 0 / 0 |

The context-line rows come from `gix-probe --context N`, which uses the
same diff as `git_hunks` and adds N lines of context through gix's
`UnifiedDiff`. The extension's rows are identical, in order, to
`gix-probe`'s TSVs for all five row sets above. The spike's `hunks` differ
by the `-U0` placements, plus one row that is the spike's own: the spike
runs `git log -p … -- '*.rs'`, and a pathspec turns on git's history
simplification, which drops commits on a side branch whose merge is
TREESAME to the other parent for those paths (commit e91103e8, reached
through merge 4ba1ceff, deletes one line of
`crates/horizon-sessiond/tests/latency_probe.rs`; `git log -- '*.rs'`
omits it, `git log --full-history -- '*.rs'` and plain `git log -p` show
it). Filtering an all-files result in SQL cannot reproduce that
simplification, and arguably should not.

Scripts (Python, run with `uv run --with duckdb==2.0.0 python …` where they
load the extension): `scripts/gix_parity.py` (the probe against the git
CLI; `python3 duckdb-ext/scripts/gix_parity.py REPO duckdb-ext/target/release/gix-probe OUT_DIR`),
`scripts/parity.py` (against a spike database), `scripts/cli_hunks.py` and
`scripts/cli_file_changes.py` (the git CLI output, parsed like the spike),
`scripts/load_smoke.py`, `scripts/perf.py`. `gix-probe REPO
hunks|commits|file_changes [--since REV] [--include-merges] [--context N]
[--stats]` prints any function's rows as TSV.

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
- `host-check/tests/load.rs`: loads the built extension into the `duckdb`
  crate with the machine's `libduckdb` 2.0.0 and checks `git_hunks` joined
  with `git_commits` on a fixture.
