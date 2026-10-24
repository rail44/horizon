# Code database (spike)

`build.py` stores a repository's syntax trees, definition units, tags, git
history, clone reports and (optionally) a SCIP index as tables in one DuckDB
file, so questions that join structure with history can be asked in SQL. It
is language-independent: everything language-specific lives in
`languages.py`; the schema, the loaders and every query in `queries/` only
use the neutral vocabulary described below.

This is a spike. **No query here is a validated quality metric**, and nothing
here feeds the build gate or any check. The header of each query states the
question it answers and, in one or two lines, what the literature does and
does not support.

## Running

Requires Git, uv and Python 3.12 (uv can provision it). Dependencies are
pinned in the script header and locked with hashes in `build.py.lock`.

```sh
# Build (always from scratch; the output file is deleted first).
uv run --locked --script scripts/code-db/build.py build . \
  --out target/code-db/horizon.duckdb --languages rust \
  --clones target/code-db/jscpd-exact/jscpd-report.json target/code-db/jscpd-normalized/jscpd-report.json \
  --scip target/code-db/index.scip

# Query by name (queries/<name>.sql) or by path; parameters are bound, not interpolated.
uv run --locked --script scripts/code-db/build.py query target/code-db/horizon.duckdb file_cochange \
  --param min_coupling_pct=50 --format table --limit 20

# Tests (fixture repositories built in a temporary directory; no network, no jscpd).
uv run --locked --script scripts/code-db/build.py verify
```

Other build options: `--languages rust,go,typescript` (default: all
adapters), `--clones LABEL=REPORT` to name a report explicitly, `--since REV`
to read only `REV..HEAD`, and `--include-merges` to also record merge commits'
diffs against their first parent (by default merges are recorded in
`commits` but carry no changes, as with plain `git log`).

Put generated databases, indexes and reports under the ignored
`target/code-db/`.

### Producing the inputs

Clone reports: the pinned jscpd 5.3.1 from the Rust audit
(`uv run --locked --script scripts/refactor-audit/run.py setup`) with the
audit's settings, once preserving and once normalizing identifiers and
literals. Pass `--absolute` (or scan `.` from the repository root): with
several scan paths jscpd 5.3.1 reports names relative to each scan path
(`crates/x/src/a.rs` appears as `x/src/a.rs`), which the loader rejects.

```sh
J=target/refactor-audit/tools/jscpd
echo '{}' > target/code-db/jscpd-config.json
for mode in exact normalized; do
  extra=(); [ $mode = normalized ] && extra=(--ignore-identifiers --ignore-literals)
  $J src crates preview-plugin/src --config target/code-db/jscpd-config.json --format rust \
    --min-tokens 70 --min-lines 5 --mode weak --reporters json --output target/code-db/jscpd-$mode \
    --max-size 10mb --max-lines 100000 --no-colors --workers 4 --absolute "${extra[@]}"
done
```

Unlike the audit, these reports scan the original sources, tests included,
so `clone_site_tags` can classify test clones instead of never seeing them.

SCIP index: `rust-analyzer scip . --output target/code-db/index.scip`
(unstable subcommand; it prints many "Duplicate symbol" warnings).

## Schema

Rows and columns are 0-based, as in tree-sitter and SCIP; end rows are
inclusive. Only `hunks` keeps git's 1-based line numbers. Queries print
1-based `start_line` columns for reading.

| Table | Content |
| --- | --- |
| `build_info(key, value)` | git HEAD and dirty flag, repo root, command line, UTC build time, Python/DuckDB/git and package versions, sha256 of each implementation file and input report/index, row counts, phase timings, peak RSS, `unit_changes` parse cache statistics |
| `files(file_id, path, language, size_bytes, blob_sha, parse_error)` | Git-tracked and non-ignored untracked files of the selected languages, read from the working tree; `blob_sha` is the git blob id of the working-tree content |
| `nodes(file_id, node_id, parent_id, kind, is_named, start_byte, end_byte, start_row, start_col, end_row, end_col, depth)` | The full concrete syntax tree, anonymous tokens included; `node_id` is the preorder index within the file |
| `units(unit_id, file_id, node_id, parent_unit_id, kind, name, qualified_name, start_row, end_row, loc)` | Definition units from the adapter's unit query; `qualified_name` joins enclosing unit names outermost first with `.`; `loc` counts non-blank lines |
| `tags(file_id, node_id, tag)` | Nodes captured by tag queries, plus file roots for file-level tags. Inheritance is not materialized: queries treat a range as tagged when a tagged node of the same file contains it |
| `commits(sha, author_time, author_name, subject, parent_count, is_merge)` | `git log` over HEAD (or `SINCE..HEAD`); `author_time` is UTC |
| `file_changes(sha, path, old_path, status, added, deleted)` | `--raw --numstat -M` for all files (not only selected languages); renames carry `old_path`; binary files have NULL counts |
| `hunks(sha, path, old_start, old_lines, new_start, new_lines)` | `-p -U0 -M` hunk headers for selected-language files only |
| `unit_changes(sha, path, unit_kind, unit_qualified_name, change_kind)` | Units touched per commit (below) |
| `clone_pairs(pair_id, group_id, source, file_a, start_row_a, end_row_a, file_b, start_row_b, end_row_b, tokens, lines)` | One row per jscpd duplicate; `group_id` joins pairs whose fragments overlap in the same file within one report |
| `scip_symbols(symbol, kind, display_name)` | Document and external symbols, first occurrence of a duplicated symbol kept |
| `scip_occurrences(file_id, symbol, start_row, start_col, end_row, end_col, is_definition, is_reference, is_import)` | Occurrences in documents that are in `files`; `is_reference` means "not a definition" (SCIP has no reference role) |

SCIP `local N` symbols are unique only within one document, so the loader
stores them as `local N <path>`.

## What `unit_changes` means

For every commit and every selected-language file with hunks, the post-image
blob (`new` side) and the pre-image blob (`old_path` or `path` at the parent)
are parsed with the adapter, and:

- a post-image unit whose row range intersects a hunk's new range is
  `modified` if a unit with the same (kind, qualified name) exists in the
  pre-image, else `added`;
- a pre-image unit intersecting a hunk's old range is `modified` if it still
  exists in the post-image, else `removed` (recorded under the old path).

Hunks come from `-U0`, so only changed lines count; a pure insertion touches
units only on the new side. Enclosing units (impl blocks, modules) are
touched whenever anything inside them is. Unit identity across commits is
`(path, kind, qualified_name)`: a file rename starts a new history under the
new path (pure renames have no hunks and produce no rows), and renaming or
moving a unit inside a file looks like a removal plus an addition. Two units
with the same identity in one file (e.g. cfg variants) are not distinguished.
Blobs are parsed once and cached by blob id; `build_info` records the lookups
and hits. Merge commits have no rows unless `--include-merges` is given.

## Adding a language adapter

Add an `Adapter` to `languages.py` and register it in `ADAPTERS`:

- `grammars`: file extension -> function returning the tree-sitter language
  capsule (pin the grammar package in the script header and re-lock with
  `uv lock --script scripts/code-db/build.py`).
- `unit_query`: each pattern captures the definition node as
  `@unit.<kind>` and the name as `@name`. Use the neutral kinds
  `function`, `method`, `type`, `module`, `impl`, `macro` where they fit.
  Several `@name` captures are joined in source order with
  `name_separator` (Rust impls: `Trait for Type`; Go methods:
  `Receiver.Method`). When several patterns capture the same node, the
  first pattern in the query wins, so list specific patterns first.
- `tag_queries`: tag -> query capturing the tagged node as `@tagged`; other
  captures may serve predicates (`#eq?`, `#match?`, `#any-of?`).
  The queries rely on the shared tags `test` and `generated_or_macro`;
  others (Rust's `trait_impl`) are allowed.
- `file_tags`: tag -> path globs; matching files are tagged at the root.
  `**/` also matches at the repository root.

Current adapters:

| Language | Units | Tags |
| --- | --- | --- |
| Rust (`.rs`) | functions; methods in `impl`/`trait` bodies (signatures too); `impl` blocks; struct/enum/union/type alias/trait as `type`; `mod`; `macro_rules!` | `test`: items after `#[test]`-style attributes (last path segment `test`, `rstest`, `test_case`, `bench`), items after `#[cfg(test)]` or `#[cfg(all(test, ...))]`, files with `#![cfg(test)]`, and paths `**/tests/**`, `**/tests.rs`, `**/*_tests.rs`, `**/test_*.rs`; `trait_impl`: `impl Trait for Type`; `generated_or_macro`: macro invocations and definitions |
| Go (`.go`) | functions; methods (named `Receiver.Method`); type specs and aliases | `test`: `*_test.go` files and functions named `Test`/`Benchmark`/`Fuzz`/`Example` followed by a non-lowercase character; `generated_or_macro`: files whose top-level comment is `// Code generated ... DO NOT EDIT.` |
| TypeScript (`.ts`, `.tsx` with the tsx grammar) | function declarations; `const f = () => ...` / function expressions; methods; class/interface/type alias/enum as `type`; namespaces | `test`: `*.test.ts(x)`/`*.spec.ts(x)` files and calls to `describe`/`it`/`test` (also `describe.each` and the like); `generated_or_macro`: files whose first comment contains `@generated` or `DO NOT EDIT` |

Differences from the Rust audit's test classification: the audit evaluates
cfg predicates in general (this adapter recognizes `cfg(test)` and
`cfg(all(test, ...))` only) and lists Horizon's `e2e.rs` as a test path
(this tool keeps repository-specific paths out).

## Clone report subset

The loader reads `duplicates[]`, and from each entry `firstFile` and
`secondFile` with `name`, `start`, `end` (1-based inclusive lines), plus
optional `tokens` and `lines`. Names may be absolute (made relative to the
repository root) or relative to the repository root; anything else is an
error. Labels default to the report's file stem, or its directory name for
`jscpd-report.json`. In jscpd 5.3.1 the normalized run labels its entries
`kind: "renamed"` or `"exact"`; the loader does not read `kind`.

## SCIP decoding

`scip.py` decodes the protobuf wire format directly for the handful of
fields stored (field numbers cited in its docstring from Sourcegraph's
`scip.proto`), skipping unknown fields. It also reads the protobuf JSON
mapping that `scip print --json` emits. This avoids both a generated
`_pb2` module (which needs `protoc` and ties to the protobuf runtime's
gencode version) and a second pinned binary. Both the legacy packed `range`
and the newer `single_line_range`/`multi_line_range` encodings are read.
The binary path is checked against a protobuf encoding of the JSON fixture
in the tests and was used on a real rust-analyzer index.

## Measured on this repository

Worktree at 2026-10-24 (515 Rust files, 1,581 commits of which 343 merges),
`--languages rust`, both jscpd reports and a rust-analyzer 1.99.1 SCIP index
(30 MB): 24 to 29 s wall time over three runs, peak RSS about 0.8 GB. Syntax 5-6 s, history
15-20 s, clones and SCIP 2 s. 1.5 M nodes, 8.9 k units, 40 k unit_changes from
34.7 k hunks; `unit_changes` parsed 6,554 distinct blobs for 12,443 lookups
(47 % cache hits). Generating the SCIP index took about 170 s; jscpd about
1 s per run. Two builds of the same tree produce identical table contents.
