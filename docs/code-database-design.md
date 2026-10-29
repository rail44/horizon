# Code database — design

A repository's structure and history as DuckDB tables, queried in SQL, to
locate refactoring candidates with the signals the literature supports
(function-level change history, clone pairs that changed inconsistently,
clone sites classified by their syntactic context, high fan-in combined
with high churn — see
[the survey](research/code-quality-analysis-trends-2026-10-24.md)). It is
language-independent: Rust is the first language, Go and TypeScript are
intended. The owner's starting idea (2026-10-24) was to store the syntax
tree and the dependency graph as they are and query them with DuckDB.

## Principle (owner decision, 2026-10-24)

**Facts come from table functions; everything derived is a view;
materializing is a cache, not a build step.** The owner confirmed this
after two earlier shapes (a Python builder that loaded tables, and a SQL
build script that did the same) were judged not to use what DuckDB is
for. DuckDB's strength is querying sources where they live, lazily, with
projection and filter pushdown and parallel scans; a code database built
that way has no build, only `LOAD` and views. Fixed baselines (the audit's
before/after comparisons) are taken with `CREATE TABLE AS` or Parquet
exports when wanted; several repositories are compared by `ATTACH`.

Consequences:

- A table function returns raw facts and nothing else: commits, file
  changes, hunks, syntax nodes, clone pairs, SCIP occurrences. It does not
  decide what a "unit" or a "test" is.
- Unit definitions, tags, unit changes, churn, co-change, clone
  consistency and fan-in are SQL views or table macros over those facts.
  Language-specific knowledge lives only in the per-language unit and tag
  views.
- A table function is expected to stream, run in parallel and honor
  pushdown where the source allows it; computing everything at init on
  one thread is a known shortcoming of the first `git_history`, not the
  target.

## Components and decisions so far

| Layer | Decision | Record |
| --- | --- | --- |
| Extension route | Rust on the DuckDB C API v2 (official `extension-template-rs`, `duckdb-neo`), binaries pinned to DuckDB 2.0.0; accepted 2026-10-24 | [extension options](research/duckdb-extension-options-2026-10-24.md) |
| Placement | `duckdb-ext/`, a Cargo sub-workspace of this repository excluded from the root workspace (like `preview-plugin/`) | `duckdb-ext/README.md` |
| Git facts | Own extension `git_history` (`git_commits`, `git_file_changes`, `git_hunks`) on gitoxide; chosen over libgit2 plus ported git logic (GPL) after measuring parity with `git log`: commits and file changes exact, `-U0` hunk placement differs in about 0.4 % of rows with no effect found on unit attribution | [gix record](research/gix-parity-2026-10-24.md) |
| Syntax facts | The community extension `sitting_duck` (`read_ast`): its node rows matched an independent tree-sitter parse of all 515 Rust files exactly, and SQL views over them reproduced the reference units and tags exactly | [evaluation](research/sitting-duck-evaluation-2026-10-24.md) |
| Clone facts | jscpd's JSON report read with the core `read_json` (no extension) | — |
| Reference facts | SCIP indexes (`rust-analyzer scip`, `scip-go`, `scip-typescript`) read by our second extension `scip_index` (chosen 2026-10-25 over the `scip` CLI's JSON) | [SCIP record](research/code-db-scip-2026-10-25.md) |

## First increment: views only (2026-10-24)

`duckdb-ext/sql/` holds the views and the first two questions as plain
SQL files read in order by any DuckDB 2.0.0 client (`00_load.sql` with
parameters as variables, `views/`, `lang/rust.sql`, `queries/`), plus
`check.py` (fixture, counts, timings, CLI comparison) and `jscpd.sh`. No
Rust changed. Measured on this repository
([record](research/code-db-views-2026-10-24.md)): every reference to a
view over `read_ast` re-scans it (11–17 scans per query, no pushdown), so
live queries over units and tags take 6–18 s; with `nodes`, `units` and
`tags` materialized (about 8 s to build) the same queries take under
0.1 s, and with `commits`/`file_changes` materialized (1–2 s) the
co-change queries drop from 2–3 s to 0.02 s. `nodes`, `hunks` and
`clone_pairs` are cheap live for a single reference. Which layers to
cache, where the cache lives and when it is refreshed is the open "cache"
knob of the principle above. `glob()` reads no `.gitignore` and descends
hidden directories, so the file set needs explicit include and exclude
patterns (a tracked-files table function is the obvious fact to add).

## Second increment: one scan per query, session cache, tracked files (2026-10-24/25)

The owner chose the order "single-scan views → tracked files → session
cache → incremental cache". Done so far
([record](research/code-db-views-single-scan-2026-10-24.md)):

- `units` and `tags` are each one statement whose node scan is a
  `MATERIALIZED` CTE, which DuckDB 2.0.0 honors inside nested views: one
  `read_ast` per query instead of 11–17. Live on this repository: units
  0.7 s, tags 0.9–1.0 s, the clone-site queries 1.0 s (from 13–14 s). The
  earlier slowness of `tags` over a node table was not `sitting_duck` but
  text slicing: hex-encoding each whole file once per identifier row; the
  fix slices BLOB bytes. Two conventions follow from the first drafts
  (one peaked at 44 GB): join text spans, never whole nodes, to file
  contents, and keep spans in their own `MATERIALIZED` CTE.
- `cache.sql` materializes nodes, units, tags, commits, file_changes and
  hunks into temporary tables and re-points the views at them (about 3 s
  to build, 3.2 GB peak; every query then runs in 0.02–0.12 s). Re-reading
  the view files returns to live; with a database file the re-pointed
  views persist and must be re-read in the next session.
- `git_tracked_files(repo, include_untracked)` is the fourth `git_history`
  function: the worktree's files with their current content ids, matching
  `git ls-files` and `git hash-object` on this repository in about 6 ms.

## Third increment: tracked files and a persistent incremental cache (2026-10-25)

([record](research/code-db-incremental-cache-2026-10-25.md)) The file
set now comes from `git_tracked_files` (regular files whose extension is
in the `languages` map; `include_untracked` optional), so the `glob()`
caveats are gone and `files` carries each file's `blob_id`. `refresh.sql`
keeps a per-repository cache file (`target/code-db/cache.duckdb`,
attached as `cache`): `nodes`, `units` and `tags` are stored per file and
only the files whose `(path, blob_id)` changed are re-parsed and
re-derived (one `read_ast` over the changed paths; the per-language views
take a `scope_paths` variable); history is appended with
`since := <cached head>` and rebuilt when that head is no longer in the
range; a `meta` row records keys (sitting_duck version, the derived
views' text, history parameters) whose change forces the matching
rebuild, and an interrupted refresh rebuilds everything. Reproduced on a
throwaway clone of this repository: cold 3.7 s (2.1 GB peak), nothing
changed 0.14–0.17 s, one edited file 0.20–0.24 s, ten files 0.24–0.26 s,
one commit of three files 0.25 s, an amended head 2.8–3.3 s (history
rebuilt); every refreshed view equals its live counterpart row for row;
queries from the cache run in 0.02–0.08 s; the cache file is about 50 MB
and does not compact deleted rows. The session cache (`cache.sql`) stays
for one-off use; the two refuse to be mixed.

## Fourth increment: historical blobs and unit changes (2026-10-25)

([record](research/code-db-unit-changes-2026-10-25.md)) The owner chose
the interim route "a table function that writes blob files" (option B)
over "content as a column parsed by `parse_ast_list`" (option A): A
returns no byte offsets, and the unit names depend on byte slicing, so the
history's units would not match the current ones. `git_changed_blobs`
(fifth `git_history` function) writes each changed regular file's old and
new blob once as `<dir>/<blob_id>.<ext>` and returns the (commit, path,
side, blob) rows; `read_ast` then reads history through the same path as
current files, and the unit rules — now a table macro over named node,
bytes and lines relations — derive `blob_units` with the same SQL (for
every current file in range, the blob's units equal `units`). The
`unit_changes` view applies the spike's hunk-range rules and reproduced
its rows exactly on the same hunks (40,731 rows on this repository);
`unit_churn` and `clone_change_consistency` return the spike record's
top rows. Blobs are immutable, so the blob layer of the cache is
append-only. Reproduced on this worktree: cold refresh 27.6 s (blob
parsing 19.5 s for 6,586 blobs and 31.4 M nodes, peak about 7 GB), then
0.4 s with nothing changed; the cache file grows to about 600 MB. Live
(uncached) history queries take 14–20 s at 8–9 GB and are not meant to
be run that way.

## Fifth increment: SCIP references and fan-in (2026-10-25)

([record](research/code-db-scip-2026-10-25.md)) The owner chose to read
SCIP indexes with a Rust table function rather than through the `scip`
CLI's JSON, after the downsides were laid out (schema tracking by hand,
one more version-pinned extension, decisions fixed in Rust, the CLI still
needed as a test reference, index generation external either way). The
second extension `scip_index` (`read_scip`, `read_scip_symbols`,
`read_scip_metadata`; a dependency-free wire-format decoder of about 160
lines citing `scip.proto` at scip-code/scip v0.10.0; documents streamed
one at a time) matched `scip print --json` on three indexes with zero
differences and reads this repository's 30 MB index in 0.07 s at 11 MiB
peak (the JSON route: 59 MB of JSON, 0.4 s to convert, 0.3–0.5 s and
420–470 MiB to load with `read_json`; both were measured, the table
function won on every number). `views/scip.sql` shapes the facts
(1-based lines and columns, `local N` symbols qualified by document,
`is_reference` = not definition since rust-analyzer 1.99.1 sets only the
Definition bit) and is optional in the file order; `unit_fan_in_churn`
reproduced the spike's 2,915 rows and top entries; the cache keys the
SCIP layer by the index file's content hash (0.02 s to hash) and rebuilds
it whole on change. The index itself is still produced outside DuckDB
(`rust-analyzer scip`, 160–280 s on this repository at 7 GB).

All three literature-backed signals (unit-level churn, inconsistent clone
changes, fan-in × churn) are now computable from the cache in under
0.2 s each.

## Sixth increment: the graph itself (2026-10-26)

([record](research/code-db-graph-2026-10-26.md)) The owner restated the
purpose — understand the code as a graph of structure and dependencies,
then judge — and the first candidate for the next step (a review ledger
with fingerprints and expiry, carried over from the old audit's sweep
workflow) was dropped as not following from it. The layer so far had only
aggregates; this increment derives the edges as views over the existing
facts (`views/graph.sql`): `unit_references`/`unit_edges` (the innermost
unit containing a reference occurrence → the innermost unit containing
the symbol's definition; `local` symbols excluded, 29 cross-unit edges
would have come from them; macro call sites and test ends flagged;
symbols with several definition units flagged `ambiguous`), structural
edges (`unit_contains`, `impl_for`, `implements` from the impl header's
occurrences), `unit_module` (files as implicit modules) and `file_crate`
with `module_edges`/`crate_edges`, `module_cycles` by recursive CTE
(crate cycles asserted empty), and history on edges (`unit_cochange`,
`edge_cochange`, `cochange_without_reference`). Queries:
`unit_dependencies` (callers/callees to a depth), `module_graph`,
`module_cycles`, `unit_cochange`, `cochange_without_reference`. All graph
views stay views over the cache (0.1–0.3 s per query; `unit_references`
0.15 s). On this repository: 34,195 unit edges, 3,517 module edges, 35
crate edges, 31 module cycles of which 3 are artifacts of rust-analyzer
giving same-named items in two targets of one package one symbol
(the largest real one: 40 modules under `horizon-agent/src/judge/`).
Two facts corrected the plan: a method call on a concrete type resolves
to the impl's method (7,517 edges) and only generic/`dyn` calls to the
trait's (32); and file-level `pub use` re-exports lie in no unit, so the
unit graph sees 105 of the 107 files referencing `SessionId`.

## Dogfooding on this repository (2026-10-26)

The owner asked to dogfood the layer and make its quality checkable. Two
checks were run, plus a one-command entry (`duckdb-ext/sql/run.sh` with
`index`, `query`, `sql` and `shell`; it loads the views in order, applies
the persistent cache and runs a question).

**Retrospective**
([record](research/code-db-dogfood-retrospective-2026-10-26.md)): the
signals were computed on the tree the September 2026 sweep started from
(e3c619a0; the sweep's commits are `e223bc20..46c37290`, 104 commits)
and compared with what the sweep actually changed (`unit_changes` over
that range) and with the units the owner explicitly preserved (the old
ledger's 27 entries plus 14 record rows; 32 mapped). "Changed during the
sweep" is broad (24.8 % of all unit identities, 29.2 % of production
functions and methods), so precision is read against that base rate: in
production scope `unit_churn` reaches P@10 0.9 and P@100 0.74, but plain
`loc` reaches 0.8 and 0.75 — history and size point at the same places —
while fan-in alone (0.28) is below the base rate, co-change without
reference is 0.49 and module-cycle membership 0.41; with a stricter
truth (≥3 sweep commits, base 0.026) the order is the same (churn 0.27,
loc 0.23, fan-in × churn 0.18, clone one-sided 0.17, co-change 0.12,
fan-in 0.04). Every preserved unit that a history signal ranked in its
top 100 was also changed in the sweep; the only preserved units ranked
high without having changed were ranked by `loc` or by cycle membership.
The sweep chose its targets with the old audit's size and clone signals,
so this is agreement between two different lenses, not an independent
outcome measure; no defect data exists.

**Prospective**
([record](research/code-db-dogfood-prospective-2026-10-26.md)): at HEAD
2ebd0ecb a reader took the top 10 of each signal and the 5 largest real
module cycles, read the code with the graph context, and judged each:
12 of 45 candidates have a nameable maintenance cost (9 distinct causes),
32 do not, 1 needs the owner's intent. Per signal (問題あり / なし):
`unit_churn` 4/6, fan-in × churn 3/7, clone one-sided 3/7, co-change
without reference 1/9, cycles 1/3 (+1 undecided). False alarms share
mechanisms the layer can name: nested units credited with inner changes;
a unit that keeps its name after its body moved keeps the old history
(the second way identity breaks, besides renames); contract types that
everything imports by design, where the compiler forces the propagation;
clone one-sidedness measured on the enclosing unit rather than the
fragment; test units in co-change pairs; parent/child module cycles and
one type's impl split over many files. The hand-over list and the
undecided question are in the record.

## Seventh increment: unit lineage (2026-10-26)

([record](research/code-db-lineage-2026-10-26.md)) Two owner decisions:
history follows the body, not the name (a body moved under a kept name
continues in the receiving unit; the stub starts a new lineage), and
lineage is many-to-many with churn counted as distinct commits over all
ancestors. `views/lineage.sql` links unit identities across one commit by
four rules — `same_name`, `file_rename` (through `file_changes.old_path`),
`content_match` (a removed and an added unit of the same commit whose
normalized line sets have Jaccard ≥ 0.5, gix's rename default) and
`body_move` (a modified unit whose pre-image is dissimilar to its
post-image while an added unit matches the pre-image) — marks ambiguous
links, derives `unit_lineage` by recursion and `unit_history` as the
distinct commits of all ancestors; the history-based queries read it,
and `unit_churn` keeps `commits_by_name` and `ancestors` so the effect is
visible. Materialized in `refresh.sql` per commit (cold +5.5 s, unchanged
+0.05 s). On this repository 3,245 of 9,264 units changed their count
(3,187 up, 58 down); `SessionId` and `definitions`→`build_definitions`
(2 → 49) resolved; `run_session` 21 → 50 with three ancestors; the
fan-in × churn order has no `commits = 1` rows; `mod tests;` with its
body moved into a *file* stays unresolved (bodies moved into several
units or a file are not a single receiver). Links at threshold 0.5:
same_name 14,044, file_rename 3,293, content_match 3,506 (18.5 %
ambiguous), body_move 127 (11.8 %).

## Re-evaluation after lineage (2026-10-26)

Both checks were repeated with lineage
([retrospective 2](research/code-db-dogfood-retrospective-2-2026-10-26.md),
[reading check 2](research/code-db-dogfood-reading-2-2026-10-26.md)).

Retrospective: aligning the ground truth through lineage changes little
(87 more sweep-changed identities map to the baseline, precision moves by
at most 0.01 at the ≥1-commit truth); the signals themselves changed. In
production scope at P@100, `unit_churn` 0.74 → 0.71, clone one-sidedness
0.65 → 0.68, fan-in × churn 0.58 → 0.43, co-change without reference
0.49 → 0.41 with its P@10 collapsing 0.9 → 0.4, fan-in alone and `loc`
unchanged (0.29, 0.75). The collapse has a named cause: units that moved
together now share their move commits, so relocation counts as
co-change (checked on two of the new top pairs: 2 of 5 and 3 of 7 shared
commits are moves). Two preserved units that never changed in the sweep
entered a history signal's top 100 through merged or moved helpers
(`scheme_from` 343 → 40 in churn), so round 1's observation that history
signals never flagged a preserved unit no longer holds.

Reading check: the sources are identical between rounds, so every change
in the top candidates comes from lineage (6/3/3/7 of 10 entered per
signal; cycles unchanged). Entrants judged: 3 with the same causes as
round 1 (`WorkspaceShell` channels, `AgentdState` locks), 2 small
test-only defects found by reading clone pairs (a match-arm paste in the
test renderer; a `GIT_CONFIG_*` reset inside a conditional loop), 13
benign. Totals 13 / 31 / 1 (round 1: 12 / 32 / 1). The mechanism table
counts, over the current 45 top candidates, how many each false-alarm
mechanism explains and whether SQL alone can fix it: nested units
credited with inner changes 11 (yes: innermost unit per hunk line);
kept-name stubs 3 (needs a lineage rule beyond the four approved); contract
types 3 (flag only); clone one-sidedness on the enclosing unit 10
(approximate on the fragment's lines); test units in co-change 6 (yes);
parent/child module cycles 4 (yes); and three mechanisms lineage
introduced or amplified: move commits counted as shared co-change 7 (yes:
exclude each unit's link commits), one-sided clone commits from before
the partner existed 6 (yes: count from the later side's first commit),
mechanical sweep commits 8 (only partly).

What this says about the mechanism: lineage made the candidates point at
the code's actual bodies (the stubs left, `build_definitions` and
`run_session` entered), which is what the reading check needs, while on
the retrospective's precision it is neutral for churn and clones and
harmful for the co-change signals until relocation commits are excluded.
The next improvements are measured, SQL-only, and ordered by how many
false alarms they remove.

## Eighth increment: the five counting rules (2026-10-27)

([record](research/code-db-counting-rules-2026-10-27.md)) The owner
approved all five fixes. As implemented, each behind a variable that
restores the old count (with all five restored the old SQL is reproduced
exactly): shared commits exclude a unit's lineage link commits
(`unit_history.via_link`; churn keeps them and shows `move_commits`);
one-sided clone changes count from the later side's first commit
(`since_sha` per pair); hunk lines credit the innermost unit, an
enclosing unit only its own lines (`attribution := 'innermost'`, lineage
keeps reading the `all` result so successions do not depend on it);
co-change pairs drop test units (`include_tests`); cycles are computed
after dropping nested-module edges and edges made only of same-type impl
blocks (`cycle_edges`). Effect on the 45 round-2 candidates: 19 left the
top lists (N1 5, N2 2, M1 8, M5 4, M6 2 cycles), precision among the
remaining 26 is 9 true (0.35 against 0.29 before); four true positives
left too — one whose cause (`reconcile`) entered on its own, one impl
whose problem is what it contains rather than its own lines, and two
small test-only findings whose counts were unrelated to the defect. The
retrospective's co-change signal recovered (P@10 0.4 → 0.6, P@100
0.41 → 0.49; ≥3 commits 0.05 → 0.14); churn, clones and fan-in did not
move in production scope. Cycles 31 → 24, edges 354 → 159;
`cochange_without_reference` 8,424 → 1,239 pairs.

The measurement exposed that innermost attribution still credits an
impl or module for inner units' doc comments and attributes (the syntax
tree places them outside the item) and for the blank lines between
items: of 853 impls still `modified`, 51 were due to code lines, 692 to
docs or attributes, 110 to blank or comment-only lines. The owner decided
(2026-10-27): leading doc comments and attributes belong to the unit they
precede (a Rust rule in `lang/rust.sql`), whitespace-only lines credit no
unit, and the innermost and test-exclusion rules stay the defaults.

## Ninth increment: attribution of leading docs and blank lines (2026-10-27)

([record](research/code-db-attribution-2026-10-27.md)) `rust_units_over`
gains `lead_start_line` (the first attribute or outer doc comment in the
run directly before the item; `start_line` and `loc` unchanged, so no
recorded number moved), and a cached `blob_blank_lines` layer lets
`unit_change_sides` drop hits whose lines are all blank on that side.
Under `all` attribution nothing changed (`unit_changes` 40,924 rows,
successions 20,970, row hashes equal). Under innermost, impls still
`modified` fell 853 → 68 (51 by code lines, 8 by their own lead, 8 by
comments) and modules 874 → 420; doc-only changes now credit the item
they document, so innermost is no longer a subset of `all`. Seven more of
the 45 round-2 candidates left the top lists (none of the 19 returned),
among them both true positives the previous round had kept: the
`AgentdState` impl (its type, the same cause, stays 2nd by fan-in ×
churn) and the `AgentdHandle`/`TerminaldHandle` clone pair, whose
fragments span several methods and therefore resolve to the impls (the
clone-side resolution, M4, is the remaining open mechanism). Retrospective
in production scope: churn unchanged (P@100 0.71 → 0.72), clones
unchanged, fan-in × churn 0.43 → 0.48, co-change 0.49 → 0.43 (a signal
effect; the truth also grew slightly because doc-only changes now count).
A first draft of the span join used over 30 GB live and was fixed by
materializing spans and hunk sides first (12.8 s at 8.7 GB live, as
before).

## Tenth increment: limits derived at load time (2026-10-27)

([record](research/code-db-limits-2026-10-27.md)) The owner asked that
memory exhaustion be impossible by mechanism and that the values be
derived, not fixed. `00_load.sql` now resets DuckDB's defaults and sets,
on every read: `memory_limit` = 60 % of what the OS can give now
(`MemAvailable` from `read_text('/proc/meminfo')` plus what this DuckDB
already holds; floor 2 GB, else it stops with a message; ceiling DuckDB's
default; without `/proc/meminfo`, 60 % of the default), `threads` =
min(CPUs, 1 per GB of the limit), `temp_directory` under
`target/code-db/tmp`, and `max_temp_directory_size` = 50 % of that
filesystem's free space (from `run.sh`/`check.py`; unset otherwise). All
four are overridable by variables and the chosen values are printed once
(`limits: memory 25.3 GB (60% of 42.2 GB available, /proc/meminfo), temp
29.1 GB (50% of 58.2 GB free), threads 24 (CPUs)` on this machine).
`run.sh index` starts `rust-analyzer scip` and jscpd under a cap of 80 %
of available memory (`systemd-run --user --scope -p MemoryMax` where it
exists; this machine has neither systemd nor a cgroup memory controller,
so `prlimit --as`, which bounds address space rather than resident size).
Measured: a cold refresh completes at 2 GB (68 s, 0.9 GB spilled) and at
4 GB (69 s, no spill) with caches identical to the unlimited one; the
live `unit_churn` that peaked at 17.9 GB completes at 4 GB in 60–77 s
with 9.9 GB of spill; a 1 GB spill cap fails with DuckDB's
`Out of Memory Error … max_temp_directory_size`; rust-analyzer under a
4.3 GB address-space cap aborts at 2.1 GB resident and `run.sh` reports
the kill, keeping the previous index. Resident size overshoots the limit
by about 0.5 GB at small limits (allocations outside DuckDB's accounting).
Two side findings: DuckDB cannot change `temp_directory` after it has
spilled in a session (`check.py` drops the statement on re-reads; a CLI
session must be restarted), and before this change the layer spilled to
DuckDB's default `.tmp` in the process's working directory, i.e. the
Horizon checkout root.

## Eleventh increment: clone-side resolution (2026-10-27)

([record](research/code-db-clone-sides-2026-10-27.md))
`clone_change_consistency` gains `clone_sides`: `overlap` resolves each
side of a clone pair to the set of innermost units whose own lines (the
spans of the ninth increment) overlap the fragment; `contain` is the
previous rule (the smallest unit containing the whole fragment, none when
the fragment spans several) and reproduces the previous output exactly
(1,502 pairs, every column, same order). The expected effect did not
occur. Of the ten clone candidates of the second reading check only the
`AgentdHandle`/`TerminaldHandle` pair changed at all (its fragments split
into four pairs, the best 3 / 0 at rank 34, not back in the top 10); the
other nine lie inside one unit, so their sides were already that unit.
Two causes: the attribution record's "9 / 0" for that pair had been
counted on the impls, not the methods, so its fall to 1 / 0 was never a
side-resolution effect and the M4 attribution was wrong for nine of ten
candidates; and a lineage gap — renaming an impl (`SessiondHandle` →
`AgentdHandle`) changes the identity of every method in it, but a method
whose lines no hunk touched has neither an `added` nor a `removed` row,
so no link is derived and its history starts at the rename (33 of 9,264
current units had an empty history). `overlap` also admits units by one
boundary line (670 of 5,433 side–unit correspondences overlap one line,
533 of them impls, modules or types), and in production scope the clone
signal's P@10 / P@50 / P@100 fell 0.8 / 0.72 / 0.66 → 0.3 / 0.38 / 0.38
(ranked identities 154 → 379); the other signals do not read
`clone_sides`. The default was `overlap` in this increment and is
restored to `contain` in the next, `overlap` staying selectable. The
method error: the record's per-candidate numbers were not re-derived on
the cache before the rule was proposed; the next increments' rules are
pre-checked that way first.

## Twelfth increment: inherited lineage, `contain` restored (2026-10-27)

([record](research/code-db-inherited-lineage-2026-10-27.md)) Pre-check
on the cache before the rule was proposed: of the 33 units with an empty
history, 29 had an enclosing unit with a rename or move link; inheriting
only the enclosing unit's immediate predecessor name reconnected 8, its
whole lineage 27 (57 commits). The rule as built is a fifth succession
kind, `inherited`, derived per commit in `derived_unit_successions`: every
inner unit of a `content_match` or `body_move` predecessor (qualified
name extending the predecessor's) links to the identity with the
successor's path and name and the same relative name, when that identity
is a unit of the new side (nothing is fabricated), neither identity has a
`content_match`, `body_move` or `file_rename` of its own at the commit
(those take precedence), and neither continues as itself under the same
path. Similarity is 1 for an inner unit no hunk touched; a link derived
from an ambiguous link, or with several inherited links at either end,
is ambiguous. On this repository: 143 links in 13 commits (122 with
similarity 1, 10 ambiguous, 4 many-to-many groups where the enclosing
types' matches crossed), empty-history units 33 → 3 — the remaining
three have no enclosing unit or no changes under their earlier names —
and 30 units reconnected with 65 commits, more than the pre-check
because the full relative name reaches through several nesting levels
and a reconnected method then follows its own earlier links. 44 further
units with some history gained commits (`AgentdState.new` 2 → 15), 74 in
all. The churn and fan-in × churn top 10 are unchanged; the clone query
under the restored `contain` default changed 4 of 1,502 rows;
retrospective P@k moved by at most 0.04 (clone P@50 0.72 → 0.76,
co-change P@100 0.43 → 0.40, fan-in × churn P@50 0.50 → 0.48; the ground
truth is unchanged). The lineage layer rebuilds once on an existing cache
(`lineage_key` hashes the derivation's text); refresh times are within
noise of the previous SQL. A first draft with correlated subqueries
passed the fixture but exceeded the spill cap on the cold refresh;
semi/anti joins over materialized sides fixed it.

## Thirteenth increment: `git_tree` (2026-10-27)

`git_history` gains its sixth function, `git_tree(repo, rev := 'HEAD')`
→ `(path, mode, blob_id)`: one row per leaf of the tree a revision
names, recursively, sorted by path bytes, modes `100644`/`100755`/
`120000`/`160000` (a symlink's blob holds its target, a gitlink's id is
the recorded commit, no directory rows), the revision resolved at bind
time with gix's `rev_parse_single` and peeled to a tree (a tree id or
`HEAD:src` is taken as is; an unknown revision is a bind-time error).
Parity with `git ls-tree -r` is exact on the fixture (every entry kind,
every revision form) and on this repository at HEAD, e223bc20 and
46c37290 (882 / 567 / 741 rows, same order; identical to
`git_tracked_files` on a clean tree). It is the fact the before/after
comparison builds on: the units of an earlier commit are its tree's
blobs joined to `blob_units`, no checkout needed. Measured precondition:
with merges excluded, the blob directory lacks one `.rs` blob of
e223bc20 and 21 of HEAD, all contents a conflict-resolving merge
produced (such as the merge of main into the decision-model branch), so
the blob layer must diff merges against their first parent to cover
every snapshot; with `include_merges := true` nothing is missing at any
of the three commits (6,600 → 6,630 distinct blobs).

## Fourteenth increment: before/after comparison (2026-10-28)

([record](research/code-db-compare-2026-10-28.md)) The owner approved
(2026-10-27) taking an earlier commit's snapshot from the cache through
`git_tree` with an index of that tree, the correspondence from the lineage with
ambiguous cases flagged, and comparison queries checked on the September sweep.
The blob layer now diffs merges against their first parent whatever
`include_merges` is (`blob_include_merges`, default true; the history views and
everything joined to hunks still follow `include_merges`, and the row hashes of
`unit_changes`, `unit_successions` and `unit_history` did not change; an existing
cache rebuilt only its blob file list and parsed 30 blobs, 1.2 s).
`views/snapshot.sql` resolves `compare_before` to a commit of `commits`, reads its
tree scoped like the current files, joins it to `blob_units`, lists files whose
blob is not in the layer (`snapshot_missing`) and gives each current unit its
lineage identities at that commit (`unit_at_before`). The graph's derivation from
`unit_occurrences` to `module_cycles` became table macros (`graph_*_over`) that the
current views and the before side (its units, impl parts from the cached blob
nodes, no tags, the index `scip_before`) both apply; the current graph views' rows
are unchanged, and `derive_key` leaves out the macros of the two optional files.
Queries: `compare_units` (kept, renamed, moved, added, removed, missing, with an
`ambiguous` flag, loc, commits, fan-in and module on each side), `compare_graph`
(module edges) and `compare_cycles`. On the sweep (46c37290 with the whole history,
before e223bc20): nothing missing; the before side equal to the current views of
an e223bc20 checkout (6,936 units, 2,343 module edges, 18 cycles, fan-in);
`unit_at_before` equal to `retro/lineage.sql`'s walk from every current identity
(6,703 pairs); kept 6,282, renamed 31, moved 390 (62 ambiguous in all), added
1,276, removed 242. Against the retrospective's ground truth, 1,256 of 1,609
by-name additions are added and 241 of 590 by-name removals removed, the rest
linked by lineage (moves into new files, renames); of 36 preserved identities 30
exist on both sides and 6 were created inside the range. Module edges added
1,283, removed 214, kept 2,129; cycles appeared 5, kept 18. This branch against
main: 415 units added, all under `duckdb-ext/`, everything else kept. Queries take
3–6 s from the cache including the load; the new macros re-derive an existing cache's units once (8.9 s), cold and
incremental refreshes are unchanged, and `check.py explain` counts are equal.

## Third re-evaluation (2026-10-28)

The owner asked for a third round and settled its shape proposal by
proposal (each with the problem it answers and the alternatives —
decision model, embeddings — considered): the reading check on the
unjudged candidates, the tracking of the known true positives, the
retrospective in its minimal form with a `loc` column, the comparison
evaluated on sampled rows and on one real branch, the mechanism table
attributed by experiment, and the owner's spot-check; no second reader
([reading check 3](research/code-db-dogfood-reading-3-2026-10-28.md),
[retrospective 3 and comparison evaluation](research/code-db-dogfood-retrospective-3-2026-10-28.md)).

**Reading check 3** at b3b94fc1 (sources unchanged since round 2): 14
of 45 candidates 問題あり, 30 問題なし, 1 undecided (round 2: 13 / 31 /
1); 9 distinct causes, 6 of round 2's 10 still surfacing plus 3 new. One
new cause is the first case where a one-sided clone count really was a
late propagation: the list modals' open/close/cancel duplicated between
`render.rs` and `modals.rs`, one fix applied to the palette (0a921404),
copied to the other two modals the next day (d39ea089), and one fix
applied four times (1c27f1ab) — reached by churn and by the clone signal,
though the lines lie outside the jscpd fragment. Co-change's top 10 (all
replaced by N1/M5) are all 問題なし. **Tracking** the 13 known 問題あり:
7 are in a top 10 now; 5 return under `attribution := 'all'` (the
`session_lifecycle.rs` and `commands.rs` impls, the `AgentdState` impl,
the `AgentdHandle`/`TerminaldHandle` pair, `tests.real_git`) and 1 under
`include_tests`; `overlap` alone does not restore the clone pair. By
signal, churn and fan-in × churn reach causes inside one large unit or
type, clones reach the copies, cycles reach only `start_approval_gate`,
and co-change reaches none in its top. **Mechanism table** by
experiment: 36 of 45 candidates move under some restored rule (19 by
their own number), 9 under none; M1 with the leading-doc attribution
explains 25, N1 8, N2 8, M5 8 (all by displacement), M6 5, M4 3; by
reading M3 10, N3 at least 4, M2 0. Five more mechanisms were named
(move commits counted in churn; a burst of preview iterations forming
co-change pairs; coupling through a third unit; a `mod x;` declaration
pairing with its file's items; an unresolved clone side never marked as
a test). M1 and the leading-doc attribution share one variable and
cannot be separated by toggling. Two discrepancies with the records are
unresolved: the clone pair ranks 39th rather than the 41st recorded
(counts equal), and restoring all five rules no longer reproduces round
2 exactly because the inherited lineage has no restore variable.

**Retrospective 3**: the final column at b3b94fc1 equals the twelfth
increment's in every cell (the thirteenth and fourteenth increments
touched nothing history reads; truth 778 / 92 / 30). In production
scope at 1+ commits, P@100: churn 0.71, clone 0.66, fan-in × churn 0.47,
co-change 0.40, fan-in 0.32 — and `loc` 0.75. Size still matches or
beats every history signal at P@100, as in round 1.

**Comparison evaluation.** Sampled rows of the sweep (seed 0.42):
`moved` non-ambiguous 10 / 10 correct, `renamed` 10 / 10, `moved`
ambiguous 4 / 10 — the six wrong rows are 0.5–0.6 `content_match` links
to a sibling moved in the same commit, each beside a correct 0.8–1.0
pair; `added` 4 / 5 and `removed` 3 / 5 (1 undecidable): a rename whose
body shrank from 40 to 10 lines and a move where every field gained
`pub(super)` fell below the threshold and show as added/removed. Of the
10 renamed rows, 7 keep the old name at HEAD as a wrapper or extraction
remainder, listed as `added`. On the decision-model range (2e6a5f32 →
2a49dc89, 13 commits, 53 files): kept 8,668 (125 with commits), added
174, removed 19, renamed 8, moved 1, edges added 66 / removed 3, cycles
unchanged (one grew 2 → 3 modules, still `kept`). What the output lacks
for a reviewer: `const`/`static` items are not units, so the range's
main behaviour change (the judge's instruction strings, the thresholds)
has no row; no text-changed flag — 55 kept rows have `commits` 2 with
identical text at both ends (three files deleted in cad2cfab and
restored in ff10f1ef) while 71 kept rows whose text changed look the
same; 23 kept rows changed with 0 commits (impls and modules under
innermost attribution, and one function whose only change came through
a merge's conflict resolution); no list of the commits behind a count;
13 tests moved across commits appear as removed + added; `run_judge` is
paired with `run_decision_judge` (similarity 0.12) rather than
`run_chat_judge` (0.71); parent/child module edges never form cycles;
edges whose weight changed stay `kept`; tests are 192 of the 328
non-trivial rows; non-Rust files (10 of 53) are out of scope.

## Quality axes, settings and the launcher (2026-10-28/29)

After the third round the owner restated what quality means here: not
taste but 「継続的にメンテナンスをする上での障害の少なさや機能の足しやすさ」,
with size, reusability and clarity of intent as the axes they named;
duplication and placement are structural properties; and 「人間の判断で
あったりプロジェクトの方向性で左右されるものは…設定やデータのような変更の
効くライフサイクルの短い部分で担うべき」. A survey of derivable axes
([record](research/code-quality-axes-2026-10-29.md)) lists 18, with
evidence strength and derivability, and classifies the parameters into
mechanism and judgment. Consultation then reduced the settings to what
cannot be decided from language structure or analysis: paths outside
maintenance (`scope.exclude`), test files and attributes a language's own
rules do not define (`tests.paths`, `tests.extra_attributes`), and the
intended layering (`[[layers]]` with `may_depend_on`); contract types and
intended duplications are to be derived (propagation share; conversion
impls with exhaustive matches) rather than listed, every entry carries a
`reason`, and the file is `.code-db.toml` in the analysed repository.
Reading that file with DuckDB was rejected as inverting the dependency
(「duckdbを用いることへの違和感が強い」), and the owner chose a Rust launcher
(「Bで」).

**Launcher** ([record](research/code-db-launcher-2026-10-29.md)):
`duckdb-ext/code-db/`, a workspace of its own linked to the machine's
libduckdb like `host-check`, replaces `run.sh` with the same subcommands
(`index`, `query`, `sql`, `shell`) plus `limits`. It reads
`.code-db.toml` (`toml`, strict: unknown keys and missing `reason` are
errors), derives the resource limits (moved out of `00_load.sql`, which
now leaves DuckDB's defaults when nothing is passed; `check.py` takes them
from `code-db limits --json`), and passes values as variables and the
layers as tables. New: `queries/layer_violations.sql` and a
`violating_edges` column on `module_cycles`. Output is byte-identical to
`run.sh` for every query (markdown, defaults). On the `board_flow`
question, with temporary settings in a clone: if the entry points belong
in `session/` there are no violations; if `board_flow/` is the only
place, the two edges `session/board.rs` and `session/connection.rs` →
`board_flow/operations.rs` are violations, and without them a 6-module
cycle inside `session/` remains (confirmed in SQL). No settings file is
committed: the layering is the owner's open decision.

**SCIP as the primary source for the current tree** (owner, 2026-10-29:
「SCIPが主でいいと想いますね。treesitter由来の木に残る役割をむしろ探した上で残すかどうかを判断したい」).
The roles of the tree-sitter tree were inventoried and measured
([record](research/code-db-tree-sitter-roles-2026-10-29.md)): 7,732 of
8,816 units match a SCIP definition one-to-one with the same kind;
`enclosing_range` starts at `lead_start_line` in all of them; 7,680
qualified names agree after normalisation; the misses are impls (551, not
symbols in SCIP), `mod x;` declarations (474), attribute-macro methods
(28), inactive-cfg code (19) and associated types (12); SCIP adds 611
`const`/`static` items. Tests are found without the tree by a path rule
and a text pattern over SCIP ranges (3,598 of 3,600). Only syntax gives
impl ranges, the extent of macro invocations (`via_macro`) and impl
header positions. History is the main constraint: one index costs
167–169 s and 2.5–2.7 GB of build output, so past blobs keep needing a
syntax parser. The decisions (unit set and kinds, ranges, naming, files
outside the index, test rule, `via_macro`, the history form, future
syntax-based axes, other languages' indexers) are open with the owner.

## Direction change: the current tree only (2026-10-29)

A pre-check ([record](research/code-db-line-tracking-precheck-2026-10-29.md))
asked whether the history of current units could come from tracking
their line ranges backward (`git log -L`) instead of the blob layer and
the lineage: line tracking passes file renames and enclosing-unit renames,
but loses 21.7 % of `unit_history`'s commits over all non-test functions
and methods, almost all at moves into other files (directory splits,
extractions; `run_session` 51 → 24, `paint_terminal` 23 → 1), at 11.6 ms a
unit. Asked why history had to be followed at all, the coordinator could
not name a situation the owner's purpose needs: churn did not beat size
in the retrospective (P@100 0.71 vs `loc` 0.75), co-change without
reference found nothing in reading check 3, clone divergence and change
frequency only reorder obstacles the current tree already shows, and a
question about one place is answered by running git by hand. The owner
set the criterion — whether history is wanted as an expression inside a
database query, joined with structure, rather than by hand — and decided:
「はい。それでよいです。」 History leaves the mechanism: the blob layer,
`unit_changes`, the lineage, churn, co-change and the history-based
queries go; the core is the current tree's axes from HEAD's SCIP index;
the comparison and the retrospective's truth become comparisons of two
snapshots. History is added back, at the granularity a concrete question
needs, when such a question appears. The increments 4, 7, 8, 11 and 12
and most of 9 remain as records of what was measured.

## Open points

- **Lineage limits**: links are same-commit only; a body split into
  several units or moved into a new file does not cut the stub's history;
  heavy rewrites below the threshold break the chain (a rename whose body
  shrank 40 → 10 lines, a move that added `pub(super)` to every field),
  and so does an enclosing unit's rename below the threshold (its inner
  units then stay unlinked too); short and test units link easily, and a
  sibling moved in the same commit draws 0.5–0.6 links beside the right
  one (6 of 10 sampled ambiguous moves); a wrapper left under the old name
  is an `added` row.
- **Comparison output** (third re-evaluation): a text-changed flag and
  the commit list behind `commits`; `const`/`static` items as units;
  edge-weight changes and parent/child edges in the graph comparison;
  tests separated.
- **Flag columns for M3 and N3** (contract types whose dependents change
  with them; sweep commits that touch many units lightly) are the
  consultation item the mechanism table leaves open: a column the reader
  discounts by, not a rule that removes rows.
- **Deferred consultations** from the third round: combining the signals
  into one calibrated score with the retrospective as labels; more truth
  sets by finding other refactoring periods with `compare_units`;
  embeddings as a sixth signal (duplicated concerns beyond token clones)
  or as a second correspondence to sample disagreements from.
- **A `git://` file system** remains the target for history access. The
  C API registers none today: both v1 and v2 only let an extension *use*
  the host file system (checked against the 2.0.0 headers), and
  registration is being added to v2 in duckdb/duckdb#25916 (open). When it
  lands, `git_changed_blobs` and the blob directory go away and
  `read_ast` reads `git://` paths directly.
- **Unit identity across commits** is `(path, kind, qualified name)`;
  renames and moves start a new history. No established system solves this
  with stored identifiers; method-level history tools treat it as a
  matching problem (90–99.5 % on Java).
- **Unit granularity**: innermost attribution credits an enclosing unit
  only with its own lines (ninth increment); the views keep the nesting
  (`parent` columns) and `attribution := 'all'` restores the old count,
  which is what brings 5 of the 13 known true positives back into the top
  10 (third re-evaluation) — the reading target and the counting unit
  still differ for impls and types.
- **How the views are loaded** (SQL files read by the client today;
  registration by an extension is not available on the C API v2 without a
  catalog surface), and whether `duckdb-ext/` is extracted for use in
  other repositories. The DuckDB 2.0.0 CLI is installed as
  `~/.local/bin/duckdb2` for interactive use.
- **Go and TypeScript** adapters exist only in the spike's record; the
  per-language SQL (`lang/<language>.sql`) and the SCIP indexers
  (`scip-go`, `scip-typescript`) are the pieces to add.

## History

A Python spike that built these tables (2026-10-24) answered the
feasibility question in 22 s on this repository and was removed the same
day because its form had not been agreed with the owner
([spike record](research/code-db-spike-2026-10-24.md); code in history at
475599d5). The measurements it produced remain valid as a reference for
the views.
