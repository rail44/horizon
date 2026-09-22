# Refactoring discovery

Run from the checkout you want to inspect. Requires Git, uv, Python 3.12
(uv can provision it), and the pinned ast-grep executable on PATH:

```sh
cargo install ast-grep --version 0.44.1 --locked
uv run --locked --script scripts/refactor-audit/run.py setup
uv run --locked --script scripts/refactor-audit/run.py scan
```

`setup` downloads checksum-pinned RCA and jscpd binaries into
`target/refactor-audit/tools/`. Automatic installation is verified on Linux
x86_64; other platforms require matching binaries on PATH (installation
commands are in `tool-versions.json`). Python dependencies and hashes are in
`run.py.lock`. A version mismatch stops the scan. There is no automatic upgrade.

## Scope and results

```sh
# Restrict discovery to repository-relative directories or files.
uv run --locked --script scripts/refactor-audit/run.py scan src/runtime crates/horizon-board/src
# Inspect tests separately, or keep both partitions labeled in one run.
uv run --locked --script scripts/refactor-audit/run.py scan --tests tests
uv run --locked --script scripts/refactor-audit/run.py scan --tests all
# Exercise real analyzers, classification, coordinates, and failure handling.
uv run --locked --script scripts/refactor-audit/run.py verify
```

The default scans Git-tracked and non-ignored untracked Rust files under the
roots in `config.json`, excluding test paths and items with test-only cfg or
test attributes. It reads the working tree, including uncommitted edits.
Excluded bytes become spaces, retaining byte offsets and line numbers.
Neither source files nor Git history are modified. Run outputs live under
ignored `target/refactor-audit/<commit>-<time>-<id>/`:

- `summary.md`: one compact row per region/partition, top clone groups, and
  convention matches. `--top N` changes the number of groups/matches/history
  pairs shown; it does not truncate JSON results.
- `report.json`: every named function's metrics, clone pairs/groups, rule
  matches, file coverage/exclusions, clone statistics, config, source/tool
  hashes, and commands.
- `raw/` and `input/`: original tool output/stderr and the analyzed snapshot.

Regions are shell domain directories, individual workspace crates, and the
preview plugin. RCA's function boundaries must agree with the independent
Tree-sitter parse. Unsupported syntax, incomplete RCA coverage, changed
inputs, and tool failures produce `status: failed` and a nonzero exit code;
partial output remains available. A completed scan is not a clean bill of health.

## How the signals are produced

| Signal | Implementation | Interpretation |
| --- | --- | --- |
| Function size and complexity | RCA parses Rust and emits physical code lines, cyclomatic and cognitive sums, including nested closures | Compare within a region; review contracts and state ownership before proposing a split |
| Repeated structure | jscpd token matching, once preserving identifiers/literals and once normalizing both; both runs compare the entire selected snapshot | Whitespace/comments are ignored in the default weak mode; normalized pairs need closer inspection |
| Convention checks | ast-grep structural rules listed in `config.json` | Only the explicitly encoded, scoped convention is checked |

Clone reports sharing a nonblank source line are grouped to reduce duplicate
review. Comments count for this grouping; groups do not imply one responsibility
or an extraction boundary. Jscpd omits files below the configured token minimum;
its analyzed-file counts are reported separately. Size/line ceilings are set
above the largest input, and a dedicated config prevents local jscpd overrides.
Macros are not expanded. Non-test cfg/features and
`cfg_attr` are not evaluated. Clones can cross function or masked-test boundaries.
External modules are classified by `test_paths`, including `e2e.rs`; their
parent module's cfg is not propagated across files. Register unusual test-only
file names there. `gpui::test` is recognized alongside Rust and Tokio tests.

## Rules and exploratory queries

`rules/runtime-visibility.yml` implements the existing crate-local visibility
convention as a review hint scoped to `src/runtime/`. It permits restricted
visibility and flags unrestricted `pub`. Add rules only for established
conventions; add positive and allowed-counterexample fixtures to `tests/`.
No metric or rule match fails the repository's build gate.

`queries/` contains structural searches without a violation judgment:

```sh
ast-grep scan --rule scripts/refactor-audit/queries/sync-reply.yml src/runtime
```

This searches the original sources, including tests. Compare each match's
timeout and error contract before deciding whether implementations should agree.
See [the review guide](../../docs/refactoring-review.md) for the semantic review.

## Optional corroboration

```sh
# Requires Java on PATH; downloads the pinned Code Maat JAR.
uv run --locked --script scripts/refactor-audit/run.py setup --history
uv run --locked --script scripts/refactor-audit/run.py scan src/runtime --history
# Requires the workspace's Rust toolchain and dependencies.
cargo install cargo-modules --version 0.26.0 --locked
uv run --locked --script scripts/refactor-audit/run.py scan src/runtime --dependencies horizon
```

History uses HEAD ancestry without merges or rename tracking. Code Maat filters
shared revisions, change-set size, and coupling according to `config.json`;
results retain current tracked Rust paths with at least one selected endpoint.
Test edits remain part of file history. Cargo-modules writes a DOT graph for
one library package, host target, default features, and no tests. Neither
co-change nor graph cycles establish a design defect.

When updating a tool, update its pin/checksum (and `uv lock --script
scripts/refactor-audit/run.py` for Python dependencies), then run `verify` and
whole-repository scans for production and tests. Keep regenerated reports out
of Git; preserve confirmed decisions in the relevant design document.
