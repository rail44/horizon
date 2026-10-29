# Code database views

The current tree of a repository as SQL views (`docs/code-database-design.md`): its units
come from the SCIP index of each of its Cargo workspaces, its quality axes are views and
one query per axis over them, and what depends on the project's own judgment comes from
its settings file. The facts are read by table functions: `git_tracked_files` (the file
set, this workspace's `git_history` extension, `../README.md`),
`read_scip`/`read_scip_symbols`/`read_scip_metadata` (this workspace's `scip_index`, over
rust-analyzer's indexes), `read_text` (the source text) and `read_json` (jscpd's
reports); everything else is a view. Nothing is stored except by `refresh.sql`, a cache
of the derived layers keyed by the state of their inputs.

History is not here (owner decision of 2026-10-29: history is added back only when a
concrete question needs it as an expression in a query). The rebuild around SCIP, the
measurements behind it and what was removed are in
`docs/research/code-db-scip-rebuild-2026-10-29.md`; the agreement of SCIP's definitions
with the earlier tree-sitter units that it starts from is in
`docs/research/code-db-tree-sitter-roles-2026-10-29.md`, the axes in
`docs/research/code-quality-axes-2026-10-29.md`.

## Running

`code-db` is the way to use this layer from the shell. It is a Rust command in
`../code-db/`, built by `duckdb-ext/build.sh` into `duckdb-ext/code-db/target/release/code-db`
(written `code-db` below) and linked against the machine's libduckdb 2.0.0 as Horizon's
daemon is (`../README.md`). It reads the SQL files and the extensions of the Horizon
checkout it was built in, and works on any repository `REPO`:

```sh
code-db index REPO                       # inputs: one SCIP index per workspace, jscpd reports
code-db query REPO size LIMIT=20         # queries/size.sql, as a table
code-db query REPO coupling min_crates=4 FORMAT=box
code-db sql REPO my-question.sql         # any SQL file, after the same loading
code-db shell REPO                       # an interactive duckdb2 session
code-db limits REPO [--json]             # the limits the loading would set
```

- **`index REPO`** reads `00_load.sql` and `views/files.sql` to list the source files
  (`files`: the settings' `scope.exclude` applied) and the tracked `Cargo.toml` files.
  It runs `jscpd.sh` over exactly those source files into
  `REPO/target/code-db/jscpd/{exact,normalized}`, then `rust-analyzer scip` once per Cargo
  workspace: every directory whose tracked `Cargo.toml` declares `[workspace]` (or a
  `[workspace.…]` table) and holds a source file, the repository root included. A
  workspace nested in another is indexed on its own (here `duckdb-ext/`,
  `duckdb-ext/code-db/`, `duckdb-ext/host-check/` and `preview-plugin/` beside the root).
  The indexes go to `REPO/target/code-db/scip/`, `root.scip` for the root and
  `ws-<path with / as +>.scip` for the others (`ws-duckdb-ext+code-db.scip`); the index of
  a workspace that is gone is removed. Each tool's output is in
  `REPO/target/code-db/logs/`, and the time of each step is printed. Run it again after
  edits: nothing checks the indexes against the tree. On this repository the root takes
  about 3 minutes cold (`rust-analyzer` builds the build scripts and proc macros into
  that workspace's `target/debug`), the four others 7–64 s (the record above has the
  numbers).
- **`query`, `sql`, `shell`** read `REPO/.code-db.toml` when it exists (Settings file),
  derive the limits (Limits) and print them to stderr, set `repo`, the limits, the
  settings' values and the command line's variables, then read the view files in the
  order below and `refresh.sql` (Cache), with the loading's own output suppressed. When
  the settings declare layers, a module that matches two layers stops the command with
  the modules and their layers. `query QUERY` then runs `queries/QUERY.sql`, `sql
  FILE.sql` reads the file, `shell` leaves the CLI open with everything in place. A query
  whose header says "Needs the SCIP indexes" stops with a message when there is none, and
  one that says "Needs the jscpd reports" when a report is missing; every other query, and
  any `sql` file without those words, runs without them (the views are then empty).
  After the loading, `code-db` removes the cache files of other input states, keeping the
  most recent.
- **Where it runs.** In the default `FORMAT`, `markdown`, everything runs in the
  command's own process through the `duckdb` crate, and the command prints each result
  as the DuckDB CLI's markdown mode does (byte for byte, `docs/research/code-db-launcher-2026-10-29.md`);
  a SQL file may use the CLI's `.print` and `.read`, and no other dot command. Any other
  `FORMAT`, and `shell`, hand the same loading to the DuckDB 2.0.0 CLI (`DUCKDB_CLI`,
  default `~/.local/bin/duckdb2`) as a script, because the crate has no renderer for
  the CLI's other modes and no interactive session; `shell` writes the script to
  `REPO/target/code-db/shell-init-PID.sql` for `-init` and removes it on exit.
- **Variables**: `NAME=VALUE` sets a DuckDB variable before anything is read (a
  number, `true`, `false` or `NULL` as it is, anything else as a string, so
  `level=crate` and `kind=function` need no quotes); `NAME:=EXPR` passes SQL text
  (`"exclude_patterns:=['src/*']"`), and overrides what the settings file sets.
  `memory_limit`, `threads` and `temp_limit` take `NAME=VALUE` only: they go into the
  limits rather than straight to DuckDB. `FORMAT` is the CLI's output mode (default
  `markdown`; `box`, `csv`, `json`, …) and `LIMIT` the number of rows `query` prints
  (it wraps the query in `SELECT * FROM (…) LIMIT n`; the query files stay as they are).
- **Errors** are one line saying what to do: no index or no jscpd report for a query
  that needs it (run `index`), an extension not built (`duckdb-ext/build.sh`), the CLI
  not found (`DUCKDB_CLI`), jscpd not installed (`JSCPD` overrides the refactor audit's
  pinned binary), a settings file that does not parse (with the line).

`check.py` (Check, below) is the tool for assertions and measurements.

## Limits

`code-db` derives DuckDB's memory, thread and spill limits from the machine's state
each time it loads the views, so a query cannot use up the machine's memory: past the
limit DuckDB spills to disk or fails with an out-of-memory error instead. It passes the
results as the variables `memory_limit`, `threads`, `temp_limit` and `temp_directory`,
and `00_load.sql` applies them at every read; DuckDB computes nothing about its own
limits. Measurements are in `docs/research/code-db-limits-2026-10-27.md`.

| Setting | Derived value (`code-db`) | Override |
| --- | --- | --- |
| `memory_limit` | 60% of what the OS can give now (`MemAvailable` from `/proc/meminfo`); at most DuckDB's default (80% of physical memory, as DuckDB reports it); below 2 GB the command stops with a message. Without a readable `/proc/meminfo`, 60% of DuckDB's default | `memory_limit=4GB` |
| `threads` | the CPU count (DuckDB's default), at most one per GB of `memory_limit` | `threads=N` |
| `temp_directory` | `REPO/target/code-db/tmp` | `temp_directory=PATH` |
| `max_temp_directory_size` | 50% of the free space of the file system holding the spill directory (`statvfs`); DuckDB's default (90% of the available disk space) when that cannot be read | `temp_limit=1GB` |

`00_load.sql` takes the four variables as they are and, for each one left unset, keeps
DuckDB's default: a client that reads the SQL files without `code-db` gets DuckDB's
defaults (its spill directory is then `.tmp` in the working directory, the Horizon
checkout root), unless it sets the variables; `code-db limits REPO --json` prints them
(`check.py` takes them from there).

The 40% of available memory left outside `memory_limit` is for what DuckDB's limit does
not account: allocations inside extensions (the git and SCIP readers), the OS page
cache, other processes. Those allocations are not bounded by anything here, and neither
is the page cache. A repository on a `tmpfs` spills into memory.

`code-db` prints the chosen values as one line on stderr, for example

```
limits: memory 25.3 GB (60% of 42.2 GB available, /proc/meminfo), temp 29.1 GB (50% of 58.2 GB free), threads 24 (CPUs)
```

and passes the same line as the variable `limits`, which the last statement of
`00_load.sql` returns (another client sees it as the result of the `.read`, or, without
the variable, a line made of the settings in force). The line says which source each
value came from: an override names its variable, `/proc/meminfo unreadable` marks the
fallback, `free space unknown` marks DuckDB's default spill cap. `check.py` prints it
once per connection.

DuckDB refuses to change `temp_directory` once it has spilled into it, even to the same
path: in a CLI session that has spilled, a second `.read` of `00_load.sql` fails with
"Cannot switch temporary directory after the current one has been used" (start a new
session). `check.py` drops that one statement when re-reading the file on a connection
that already has the directory.

**External tools.** `code-db index` runs `jscpd.sh` and each `rust-analyzer scip` under a
memory cap of 80% of `MemAvailable` (`tool_memory_max=SIZE` overrides it: a number with
an optional `K`, `M`, `G` or `T` suffix, base 1024). With `systemd-run --user` available
the cap is a cgroup (`--scope -p MemoryMax=… -p MemorySwapMax=0`): it bounds the resident
memory of all the tool's processes together, and the kernel kills the tool at the cap.
Otherwise it is `prlimit --as=…`, a limit on each process's address space, not its
resident memory: a process reserves more address space than it touches (rust-analyzer's
peak address space was 9.6 GB at 7.4 GB resident here), so the cap stops a tool earlier
than its resident size would, and each process of a tree has its own. A tool that
reaches it fails an allocation and aborts. Without either, the tools run unbounded and
`code-db` says so. `code-db` prints the cap it chose, and a tool that fails is reported
with its exit status or signal and the cap; that workspace's previous index is kept.

The rest of this section is what `code-db` does, for other clients. Every client needs
DuckDB **2.0.0** (the extensions load only into that version), unsigned extensions
allowed, and the git_history and scip_index binaries built (`duckdb-ext/build.sh` builds
both). Run from the root of this Horizon checkout: `00_load.sql` loads both by paths
relative to it, and `views/clones.sql` and `refresh.sql` read
`duckdb-ext/sql/empty-jscpd-report.json` the same way. The analysed repository is a
variable and can be anywhere. The files are executed in this order:

```
duckdb-ext/sql/00_load.sql
duckdb-ext/sql/views/files.sql
duckdb-ext/sql/views/scip.sql
duckdb-ext/sql/views/units.sql
duckdb-ext/sql/views/graph.sql
duckdb-ext/sql/views/clones.sql
duckdb-ext/sql/views/quality.sql
duckdb-ext/sql/refresh.sql      (optional: the cache)
```

Every file can be read whether or not the indexes and reports exist: `00_load.sql`
lists the ones that do (`scip_indexes`, `clone_reports`), and the views over an empty
list are empty. Only `00_load.sql` (the file snapshot from `git_tracked_files` and the two
lists) and `refresh.sql` read a source when they are executed.

DuckDB CLI 2.0.0 (here `~/.local/bin/duckdb2`; the system `duckdb` is 1.5.0 and refuses
the extensions):

```sh
duckdb2 -unsigned -cmd "SET VARIABLE repo = '$PWD'"
```
```
.read duckdb-ext/sql/00_load.sql
.read duckdb-ext/sql/views/files.sql
.read duckdb-ext/sql/views/scip.sql
.read duckdb-ext/sql/views/units.sql
.read duckdb-ext/sql/views/graph.sql
.read duckdb-ext/sql/views/clones.sql
.read duckdb-ext/sql/views/quality.sql
.read duckdb-ext/sql/refresh.sql
SET VARIABLE level = 'crate';
.read duckdb-ext/sql/queries/size.sql
```

When stdout is not a terminal and `AI_AGENT` is set, the 2.0.0 CLI switches to an "agent
mode" with Markdown tables; `-no-agent` turns it off. The CLI goes on after a failing
statement unless started with `-bail` (or after `.bail on`); the checks in `refresh.sql`
stop the file only then.

## Settings file

Judgment that depends on the analysed repository (what is out of scope, what counts as
a test, which module dependencies are intended) lives in that repository's
`.code-db.toml`, at its root, not in the views (owner decision 2026-10-29). `code-db`
reads it with the `toml` crate and hands DuckDB the values; without the file everything
has its default. Every entry needs a `reason`, so that a later reader knows why it is
there; an unknown key, a missing `reason`, a layer named twice or a `may_depend_on`
naming no declared layer is an error that names the key and the line.

```toml
[scope]
exclude = [ { path = "experiments/**", reason = "spikes, not product code" } ]

[tests]
paths = [ { path = "**/*.test.rs", reason = "..." } ]       # extra test files
extra_attributes = [ { name = "my_test", reason = "..." } ] # extra test attribute names

[[layers]]
name = "board"
modules = ["crates/horizon-agentd/src/board_flow/**"]
reason = "..."
[[layers]]
name = "session"
modules = ["crates/horizon-agentd/src/session/**"]
may_depend_on = ["board"]
reason = "..."
```

| Key | Becomes | Read by |
| --- | --- | --- |
| `scope.exclude[].path` | the variable `exclude_patterns` | `00_load.sql` (`is_source_file`): the files, so also the units, the graph and what `index` hands jscpd |
| `tests.paths[].path` | the variable `extra_test_paths` (GLOB operator patterns on the relative path, `*` crosses `/`), added to Rust's own path rules | `derived_units` (`is_test`) |
| `tests.extra_attributes[].name` | the variable `extra_test_attributes`, added to the attributes whose name ends in `test` and `cfg(test)` | `derived_units` (`is_test`) |
| `[[layers]]` `name`, `modules` | the table `config_layers(name, module_pattern)`, one row per pattern | `module_layer_matches`, `module_layers` (`views/graph.sql`) |
| `[[layers]]` `may_depend_on` | the table `config_layer_deps(name, may_depend_on)` | `graph_layer_violation`, `module_layer_violations`, the queries `layer_violations` and `module_cycles` |

A module (`modules.module`: a file's path, or `<path>::<name>` for an inline `mod` block)
is in a layer when it matches one of the layer's patterns with the GLOB operator, so
`src/model.rs*` covers `src/model.rs` and its inline modules. A module in no layer is
unconstrained; a module that matches two layers is an error (`code-db` reports it before
any query, and `module_layers` fails on it). An edge goes against the layers when both
ends are in layers, the layers differ and the source layer's `may_depend_on` does not
name the target's (`may_depend_on` is not transitive). The variables and tables are set
before `00_load.sql` is read, which creates the two tables empty when they do not exist;
a `NAME=VALUE` on the command line overrides a variable the file sets. The cache's key
includes the two test variables.

Not settings, and derived instead (owner decisions): which types are contracts shared
across crates (`contract_candidate`, a type referenced from at least `min_crates` other
crates, a query parameter) and which duplicates are intended copies
(`conversion_pair`, two types joined by a conversion impl).

## SCIP

`views/scip.sql` reads every index of `scip_indexes` (default: the `*.scip` files of
`<repo>/target/code-db/scip/`, sorted, listed when `00_load.sql` is read). The scip_index
functions take the list and read the indexes in order, each row carrying its index's
path; an index's documents are relative to its project root, which
`scip_index_roots` turns into a repository-relative prefix from the index's metadata
(`project_root`), so that every document is a repository path. The workspaces of this
repository share no document (measured: each index lists only its own workspace's
files).

- **`scip_index_occurrences`, `scip_index_symbols`, `scip_index_metadata`** are
  `read_scip`, `read_scip_symbols` and `read_scip_metadata` as they come
  (`../README.md`): 0-based lines and characters, raw `local N` symbols, documents
  relative to the index's project root, plus `index_path`.
- **`scip_index_roots(index_path, project_root, prefix)`**: each index's root as a
  directory of the repository (`''` for the root; NULL outside `repo`).
- **`scip_occurrences(file_id, document, symbol, start_line, start_column, end_line,
  end_column, is_definition, is_reference, enclosing_start_line, enclosing_start_column,
  enclosing_end_line, enclosing_end_column)`**: lines 1-based and inclusive, columns
  1-based with the end the column after the last character, in the unit of the
  document's position encoding (UTF-8 bytes for rust-analyzer). `file_id` is `document`
  joined to `files.path`, NULL for a document that is not a source file (an excluded
  file). `local N` symbols are qualified as `local N <document>`. `is_reference` is `NOT
  is_definition`: rust-analyzer 1.99.1 sets no role bit but Definition (the Test and
  Generated bits are never set, so tests are found from the text, Units).
- **`scip_symbols(document, symbol, kind, display_name, documentation, signature)`**:
  `read_scip_symbols` with the same document and local naming; `signature` is the text of
  `signature_documentation` (the declaration's head: visibility, `fn`, parameters, return
  type, `where` clause; no attributes); `document` is NULL for an external symbol. A
  non-local symbol can be listed under several documents (a package's `crate/` symbol
  under each target's root file), one row per listing.
- `scip_document_occurrences` and `scip_document_symbols` are the same rows without
  `file_id`.

## Units

`views/units.sql` defines `units` from the definition occurrences (the recommended form
the owner accepted on 2026-10-29: 「scipは推奨の形で導入しましょう」):

- **What is a unit.** One per definition occurrence of a non-local symbol in a source
  file whose SCIP kind maps to a unit kind: Function → `function`; Method, StaticMethod,
  TraitMethod → `method`; Struct, Enum, Union, TypeAlias, Trait, AssociatedType → `type`;
  Module → `module`; Macro → `macro`; Constant, StaticVariable → `constant`. Fields, enum
  variants, parameters and locals are not units; their occurrences belong to the unit
  containing them. Impl blocks are not units (rust-analyzer gives them no symbol); their
  members carry them in their descriptors, `…impl#[Type]m().` and
  `…impl#[Type][Trait]m().`, as `impl_key`, `impl_type` and `impl_trait` (`member_of`:
  `impl`, `trait_impl`, or `trait` for a trait's own items). A `mod x;` declaration is a
  reference, not a unit; file modules (`is_file_module`, rust-analyzer's whole-file
  definition) and inline modules are. Code rust-analyzer does not see (an inactive cfg,
  a file outside every workspace, such as a script's fixture) has no units; its file is
  still in `files`.
- **Ranges** are `enclosing_range` (leading doc comments and attributes included; a plain
  `//` comment above an item is not, and belongs to the enclosing unit), with the name's
  position as `name_line`/`name_column`. `parent_unit_id` is the innermost other unit
  whose range contains the unit's; units with the same range (several items one macro
  call defines) are siblings. `unit_id` orders units by file, then range start, the longer
  range first, so the innermost unit containing a position is the containing one with the
  greatest `unit_id`.
- **Names.** `crate` is the SCIP package; `name` the display name (the package for a
  crate root); `qualified_name` the descriptors below the file's own module, `.`-joined:
  `Type.m`, `Trait for Type.m`, `tests.f`. Items inside a function body do not name the
  function (SCIP's descriptors do not), so two such items of one file can share a
  qualified name; `symbol` is unique.
- **`loc`** counts the non-blank lines of the range (an enclosing unit counts what it
  contains). **`visibility`** is the signature's leading `pub`, `pub(crate)`, `pub(super)`
  or `pub(in …)`, else `private`, and NULL for a trait's items and a trait impl's
  members (they take the trait's). **`parameters`** counts a function's or method's
  parameters, `self` included, at the signature's outermost bracket depth. **`has_doc`**
  is whether the symbol has documentation; NULL for a trait impl's members, to which
  rust-analyzer gives the trait item's documentation.
- **`is_test`**: the file matches a test path (`tests/` directories, `tests.rs`,
  `*_tests.rs`, `test_*.rs`, or `extra_test_paths`); or a line of the unit's lead (its
  range from the start to the name, comment lines skipped) holds an attribute whose last
  path segment ends in `test` (`#[test]`, `#[tokio::test]`, `#[rstest]`), one named in
  `extra_test_attributes`, or `#[cfg(test)]`/`#[cfg(all(…test…))]`; or, for a file
  module, the file holds `#![cfg(test)]`; or the unit is a member of an impl whose header
  line is led by `#[cfg(test)]` (the header: the nearest line above the member starting
  an `impl` that names `impl_type`, outside the bodies of other functions,
  `impl_header_line`); or a unit containing it is a test.

## Graph

`views/graph.sql` derives edges from the units and the occurrences.

- **Locating an occurrence.** An occurrence belongs to the innermost unit whose range
  (lines and columns) contains its start. Every indexed file has its file module, so a
  `use` declaration or a file-level macro call belongs to the file module.
- **`unit_occurrences(file_id, symbol, start_line, start_column, end_line, end_column,
  is_definition, is_local, unit_id)`**.
- **`symbol_units(symbol, unit_id, ambiguous)`**: the unit a symbol's definition belongs
  to: the unit it defines when it is a unit, else the unit containing the definition (a
  field to its type). A module's own symbol has none: a reference to a module is a path
  segment, an import or a `mod x;`, not a dependency on its code. `ambiguous`: the symbol
  has several definitions in different units (rust-analyzer names symbols by package, not
  target).
- **`unit_references(src_unit_id, dst_unit_id, symbol, occurrences, ambiguous,
  src_is_test, dst_is_test)`** and **`unit_edges(src_unit_id, dst_unit_id, symbols,
  occurrences, is_self, ambiguous, src_is_test, dst_is_test)`**.
- **`unit_contains`, `unit_ancestors(unit_id, ancestor_id, depth)`**.
- **`impls(impl_id, file_id, crate, impl_key, impl_type, impl_trait, header_line,
  members)`**: one row per impl block as its members give it (key, file, header line).
  **`impl_targets(impl_id, part, name, target_unit_id, resolved_by)`**: the `type` and
  `trait` parts resolved to type units by their bare name: the candidate referenced on
  the header line (`header`), else the only one in the file (`file`), the crate
  (`crate`), the repository (`name`); NULL when none or several (external traits such as
  `From`). On this repository 565 of 571 impls' types resolve, 559 through the header.
  **`impl_for(impl_id, type_unit_id)`**, **`implements(impl_id, trait_unit_id)`**,
  **`unit_impl_type(unit_id, impl_id, type_unit_id)`** (the impl of a unit's nearest
  ancestor-or-self member).
- **`modules(module, file_id, module_unit_id, crate, module_path)`** and
  **`unit_module(unit_id, module, module_unit_id, file_id, crate, module_path)`**: every
  module unit; a file module is named by its path, an inline module `<path>::<names>`. A
  unit belongs to its nearest ancestor-or-self module. `module_path` places a module in
  its crate's tree by the file-module convention (a crate root and `mod.rs` are their
  directory, another file its path without `.rs`, an inline module its file's path plus
  its names), not by the descriptors: SCIP names every target's root `crate/`, which would
  make an integration test's root an ancestor of every library module.
- **`module_edges(src_module, dst_module, src_crate, dst_crate, unit_edges, occurrences,
  test_unit_edges, ambiguous_unit_edges, nested_modules, same_type_unit_edges)`**,
  **`crate_edges`**, **`module_cycle_edges`**, **`module_cycles(cycle, crate, modules,
  members, edges, unit_edges, occurrences)`**, **`crate_cycles`**: as before the rebuild,
  over these units. `nested_modules` marks an edge between a module and its ancestor or
  descendant; `same_type_unit_edges` counts the unit edges whose ends lie in impls of one
  type. Cycles leave out both kinds of structural edge unless `cycle_edges := 'raw'`.
- **Layers**: `module_layer_matches`, `module_layers`, `graph_layer_violation`,
  `module_layer_violations` (Settings file).

Accepted limitations: `local` symbols are excluded; a call through a generic bound or
`dyn` resolves to the trait's method, not an impl's (a call on a concrete type reaches
the impl's); occurrences inside macro invocations are the ones rust-analyzer maps back
through the expansion, and references inside `macro_rules!` bodies have none.

## Axes

`views/clones.sql` and `views/quality.sql` hold the per-unit facts of the axes; each
query asks one axis's question. Every query's header states the question, the rows, the
evidence (the strength the axes record gives, §5) and the judgment it takes from the
settings file.

| Axis (axes record) | Query | Per-unit facts | Evidence |
| --- | --- | --- | --- |
| size (§3.1, §3.2) | `size` (`level` unit, file, crate) | `loc` | medium |
| duplication (§3.5) | `duplication` (pairs, `conversion_pair`) | `unit_clones` | medium |
| coupling (§3.6) | `coupling` (`contract_candidate`, `min_crates`), `module_graph` | `unit_fan_in`, `unit_fan_out` | medium |
| independence (§3.8) | `module_cycles` (`violating_edges`), `layer_violations` | module of the unit, its cycle and layer | weak |
| interface (§3.4) | `interface` (parameters, visibility), `public_surface` (per crate) | `visibility`, `parameters` | weak |
| intent, mechanical part (§3.15, §3.18) | `intent` | `has_doc`, `unit_debt_markers` | weak to none |
| unreferenced definitions (§3.16) | `unreferenced` | `unit_reference_status` | weak |

- **`clone_pairs(pair_id, source, file_a, start_line_a, end_line_a, file_b, start_line_b,
  end_line_b, tokens, lines)`**: jscpd's duplicates from the reports of `clone_reports`
  (`index` runs jscpd over the files of `files`), read with a fixed schema; the list
  always includes `empty-jscpd-report.json`, so the view exists without reports.
  **`clone_sides(pair_id, side, file_id, start_line, end_line, unit_id)`**: each fragment
  with the innermost unit containing all its lines.
- **`unit_scope(unit_id, member_unit_id)`**: what a unit stands for when coupling is
  counted: itself, what it contains, and for a type its impls' members.
  **`unit_fan_in(unit_id, fan_in_files, fan_in_crates, fan_in_units,
  reference_occurrences)`**: the files and crates other than its own, and the units
  outside its scope, that reference a symbol belonging to its scope.
  **`unit_fan_out(unit_id, fan_out_files, fan_out_crates, fan_out_units)`**: the same for
  what its scope references.
- **`unit_reference_status(unit_id, references, status)`**: the reference occurrences of
  the unit's own symbol anywhere, and `referenced`, `unreferenced`, or why it is
  structurally unreferenced: `test`, `trait_impl`, `entry_point` (`main`), `crate_root`,
  `trait_item`.
- **`unit_debt_markers(unit_id, debt_lines)`**: lines with `TODO` or `FIXME`, each counted
  for its innermost unit.
- **`unit_clones(unit_id, clone_pairs, exact_pairs, normalized_pairs)`**.
- **`unit_quality`**: one row per unit with the per-unit columns of every axis side by
  side (identity, `loc`, clone counts, fan-in and fan-out, the module's cycle and layer,
  visibility and parameters, `has_doc` and `debt_lines`, references and their status). It
  combines nothing: there is no score.

Other queries: `unit_dependencies` (a unit's references and referrers up to `depth`).

Deferred: complexity and nesting depth (a later increment, from the saved SCIP index's
queries); an exhaustive-`match` check (needs syntax).

## Cache

`refresh.sql` materializes the derived layers and points their views at the copies;
every other view and every query keeps its text. Materialization is only a cache: the
views compute the same rows live (`check.py refresh` compares them).

- **Key.** `repo`, the file snapshot (paths and content ids, taken again), each index's
  path and content hash, the two test variables, and the text of every view and macro.
  The copies of one key are the file `<cache_dir>/<md5 of the key>.duckdb` (default
  `<repo>/target/code-db/cache/`, which must exist; `code-db` creates it), attached as
  `cache`. A file that exists already holds its layers (each is created whole), so a
  read with an unchanged state only attaches it; any change of the key builds a new file
  from scratch. `code-db` removes the files of other keys after a read.
- **Layers**, in order, each built from the copies before it: `scip_occurrences`,
  `scip_symbols`, `units`, `unit_occurrences`, `symbol_units`, `unit_references`,
  `unit_edges`, `unit_ancestors`, `impls`, `impl_targets`, `unit_impl_type`, `modules`,
  `unit_module`, `module_edges`, `unit_scope`, `unit_fan_in`, `unit_fan_out`,
  `unit_reference_status`, `unit_debt_markers`. Views that read a query variable or the
  settings' tables (cycles, layers) and the clone views stay live, over the copies.
- **Measured** on this repository (the record has the runs): building the copies takes
  1.0–2.3 s; with them, `unit_quality` takes 0.13–0.15 s instead of 16–19 s live, and
  each query at most 0.25 s (live: 0.4–19 s, peak RSS up to 7.7 GB); a `code-db query`
  process spends 1–3.5 s loading
  (the file snapshot twice, the key's index hash).
- Read it once per session after the view files; reading it again needs the view files
  first (it refuses while the views read a cache).

## Check

```sh
uv run --locked --script duckdb-ext/sql/check.py fixture          # assertions on a generated fixture
uv run --locked --script duckdb-ext/sql/check.py fixture --print  # the same rows, printed
uv run --locked --script duckdb-ext/sql/check.py repo REPO        # counts and every query's first rows
uv run --locked --script duckdb-ext/sql/check.py refresh REPO     # the cache compared with live
uv run --locked --script duckdb-ext/sql/check.py measure REPO --runs 3  # each query live, building and with the cache
```

`fixture` builds a git repository in a temporary directory: a Cargo workspace of two
crates (`shapes`, a library with a trait, a constant, a struct, a trait impl with an
associated type, two inherent impls one of them `#[cfg(test)]`, a `#[cfg(test)]` module,
a TODO, an unused function and two sibling modules calling each other through a `From`
impl; `app`, a binary using it, an integration test and a file the settings name a test),
a second workspace nested in it (`tools/`), a file outside every workspace and a file the
settings exclude. It indexes both workspaces with rust-analyzer (`RUST_ANALYZER`; skipped
with a message when missing), writes two jscpd-shaped reports (a pair between two types a
conversion joins, and one that is not), fills the settings' tables as `code-db` does
(`exclude_patterns`, `extra_test_paths`, `extra_test_attributes`, two layers), and asserts
every row of `files` and `units`, `impl_targets`, `unit_edges`, `modules`, `crate_edges`,
`module_cycles`, `unit_quality`, and every query with its parameters (`size` at three
levels, `coupling` with `min_crates`, `unreferenced` with the structural rows,
`duplication` with tests), the layers emptied (no violation) and doubled (an error), and
`unit_dependencies` without its unit (an error). It then reads `refresh.sql` and asserts
the copies and every query equal to live, a second read keeping the file, the refusal to
read it twice, and an edit building another file; and that the views exist and are empty
without indexes and reports.

Every connection takes its limits from `code-db limits REPO --json` (the binary
`duckdb-ext/code-db/target/release/code-db` that `duckdb-ext/build.sh` builds, or
`CODE_DB`), with `--set`'s `memory_limit`, `threads` and `temp_limit` passed to it.
`check.py` reads the SQL files in-process and does not read a `.code-db.toml`; the
fixture sets the variables and tables itself.

`code-db`'s own tests are in `../code-db/` (`cargo test`): the settings file, the limits
from a fake `/proc/meminfo` and DuckDB's size formatting, the markdown renderer and the
statement splitter, workspace discovery and index names, and an end-to-end run on a
fixture repository: `index` with a stand-in jscpd (both workspaces indexed, the files in
scope handed to jscpd), the `layer_violations` row and the cycle's `violating_edges`, the
settings' test rules and exclusion, a duplication pair, and a query without the reports
(runs) against one that needs them (stops). `duckdb-ext/check.sh` runs them.

## Layout

| File | Defines |
| --- | --- |
| `00_load.sql` | `LOAD` of the two extensions; the limits it is given (Limits); parameter defaults; the settings' tables `config_layers`, `config_layer_deps` (created empty when missing); `is_source_file`, `tracked_source_files(path, mode, blob_id)`; the snapshot `source_files`/`source_paths`; the lists `scip_indexes` and `clone_reports`; the limits line as its last result |
| `views/files.sql` | `files(file_id, path, language, mode, blob_id)`, `sources(file_id, content)`, `source_lines(file_id, line, text, is_blank, is_comment, nonblank_through)` |
| `views/scip.sql` | `scip_index_occurrences`, `scip_index_symbols`, `scip_index_metadata`, `scip_index_roots`; `scip_document_occurrences`, `scip_document_symbols`; `scip_occurrences`, `scip_symbols` (SCIP) |
| `views/units.sql` | the macros `impl_base_name`, `impl_generic_arg`; `derived_units`, `units` (Units) |
| `views/graph.sql` | `unit_occurrences`, `symbol_units`, `unit_references`, `unit_edges`, `unit_contains`, `unit_ancestors`, `impls`, `impl_targets`, `impl_for`, `implements`, `unit_impl_type`, `modules`, `unit_module`, `module_edges`, `crate_edges`, `module_cycle_edges`, `module_cycles`, `module_layer_matches`, `module_layers`, the macro `graph_layer_violation`, `module_layer_violations`, `crate_cycles` (Graph) |
| `views/clones.sql` | `clone_pairs`, `clone_sides` |
| `views/quality.sql` | `unit_scope`, `unit_fan_in`, `unit_fan_out`, `unit_reference_status`, `unit_debt_markers`, `unit_clones`, `unit_quality` (Axes) |
| `queries/*.sql` | one statement each: `size`, `duplication`, `coupling`, `interface`, `public_surface`, `intent`, `unreferenced`, `module_graph`, `module_cycles`, `layer_violations`, `unit_dependencies`; the header states the question, the axis, the evidence, the settings it reads and the parameters |
| `refresh.sql` | the cache (Cache) |
| `jscpd.sh` | the two jscpd reports `views/clones.sql` reads |
| `empty-jscpd-report.json` | a report with no duplicates, always in `clone_pairs`' list |
| `../code-db/` | the launcher, `code-db` (Running) |
| `check.py` | the check above |

## Conventions

- **Lines are 1-based and inclusive** everywhere (`start_line`, `end_line`), as in
  jscpd; columns are 1-based, the end the column after the last character.
- **Paths are repository-relative** with `/`. `files.file_id` is the position in the
  sorted file list; `units.unit_id` the position in the unit order (Units).
- **Neutral vocabulary**: unit kinds `function`, `method`, `type`, `module`, `macro`,
  `constant`; the SCIP kind is kept as `symbol_kind`. Language knowledge (the kind
  mapping, the test rules, the impl descriptors, the file-module convention) lives in
  `views/units.sql` and `views/graph.sql`, written for Rust; another language's indexer
  would add its own mapping there.
- **A view reads each source once.** A view or common table expression that is not
  `MATERIALIZED` is expanded at every reference; the views materialize what they read
  more than once, and the cache materializes the layers.

## Parameters

DuckDB variables (`SET VARIABLE name = value`). A view or query reads them when it runs,
so a change applies to the next query, except the file list and the two input lists,
which `00_load.sql` takes as snapshots.

| Variable | Default | Read by |
| --- | --- | --- |
| `repo` | required; an absolute path | everything |
| `memory_limit`, `threads`, `temp_limit`, `temp_directory` | unset (DuckDB's defaults); `code-db` sets them to the derived limits (Limits) | `00_load.sql` |
| `limits` | unset; `code-db` sets it to its limits line | `00_load.sql` (its last result) |
| `include_untracked` | `false` (also list untracked files that are not ignored) | `00_load.sql`, `refresh.sql` |
| `exclude_patterns` | `[]` (GLOB operator patterns on the relative path; `*` crosses `/`) | `00_load.sql`, `refresh.sql` |
| `languages` | `MAP {'rs': 'rust'}` (extension to language) | `00_load.sql`, `files` |
| `scip_indexes` | the `*.scip` files of `repo/target/code-db/scip/` | `views/scip.sql`, `refresh.sql` |
| `clone_reports` | the reports `repo/target/code-db/jscpd/{exact,normalized}/jscpd-report.json` that exist | `clone_pairs` |
| `extra_test_paths`, `extra_test_attributes` | `[]`; `code-db` sets them from the settings file | `derived_units`, `refresh.sql` (key) |
| `cache_dir` | `repo/target/code-db/cache` | `refresh.sql` |
| `cycle_edges` | `'collapsed'` (`'raw'`: every module edge) | `module_cycle_edges` and what reads it |
| `level`, `kind`, `include_tests` | `'unit'`, NULL, `false` | `size` (`kind` and `include_tests` also other queries) |
| `source` | NULL (both reports) | `duplication` |
| `min_crates` | 3 | `coupling` (`contract_candidate`) |
| `min_parameters` | 0 | `interface` |
| `include_structural` | `false` | `unreferenced` |
| `crate` | NULL (all crates) | `module_graph`, `module_cycles` |
| `path`, `unit_kind`, `qualified_name`, `depth`, `direction` | required, required, required, 1, `'both'` (`'in'`, `'out'`) | `unit_dependencies` |

How parameters reach the table functions: `getvariable('…')` is accepted as an argument
of `read_text`, `read_json`, `read_blob`, `glob`, `ATTACH` and the git and scip
functions inside a view, and is evaluated when the view is scanned. `LOAD` takes only a
literal, so the extension paths are fixed in `00_load.sql`. `read_json` and `read_blob`
need at least one file, which is why `empty-jscpd-report.json` is always in their lists.

## Comparison

Two snapshots of a repository compared as differences of their obstacle lists (owner
decisions of 2026-10-29: the purpose of the old audit's `compare`, whether the targeted
obstacles went down after a refactoring and whether new ones appeared; identity by
descriptor; a rename may show as a disappeared and an appeared row; no unit-by-unit
correspondence and no lineage). The record is
`docs/research/code-db-obstacle-diff-2026-10-29.md`.

```sh
code-db compare AFTER BEFORE                      # compare/queries/summary.sql
code-db compare AFTER BEFORE units kind=function  # compare/queries/units.sql
code-db compare AFTER BEFORE ./my-question.sql    # a QUERY with a / is a SQL file
```

Each snapshot is a checkout with its own `target/code-db/` (`code-db index` run in it).

- **How it loads.** `code-db` reads BEFORE as `query` reads a repository (the view files,
  `refresh.sql`, BEFORE's cache, built when missing), reads `compare/sides.sql`, and
  copies its `side_*` views with `compare/export.sql` into
  `AFTER/target/code-db/tmp/compare-before-PID.duckdb`. It then reads AFTER the same way,
  reads `compare/sides.sql` again, attaches the copy read-only as `before`, reads
  `compare/views.sql`, runs the query and removes the copy. Both sides are derived by the
  SQL files of the Horizon checkout `code-db` was built in, in one run, so a cache built
  by other SQL is never read (its key differs; a new one is built). BEFORE's cache cannot
  be attached as it is: `unit_quality`, the clone views, the cycles and the layers are not
  in it (they read jscpd's reports, a variable or the settings), and the views read the
  repository through global variables, so two repositories cannot be live in one session.
- **Refusals.** The SCIP indexes and both jscpd reports must exist on both sides
  (`compare (BEFORE) needs the SCIP indexes in …: run 'code-db index …'`): an input
  missing on one side would show that side's whole list as appeared or disappeared.
  AFTER and BEFORE must be different directories.
- **Settings.** Both sides read AFTER's `.code-db.toml`: a difference is then one of the
  code, not of the judgment. When BEFORE's file differs, a line on stderr says so.
  `NAME=VALUE` arguments apply to both sides.
- **Identity.** A unit is `(crate, kind, descriptor)`: the SCIP symbol without scheme,
  manager, package and version (`workspace/run().`), its place in the crate's module tree.
  A file moved without changing its module (`transcript.rs` to `transcript/mod.rs`, an
  inline `mod tests` to `tests.rs`) keeps its units; an item moved to another module, or
  renamed, is a disappeared and an appeared row. A descriptor several units share on a
  side (a package's targets all name their root `crate/`; items in function bodies leave
  the function out) pairs its units by path first, in line order within a file, then the
  rest in path and line order (`ambiguous`, `paired_by`). Modules (edges, cycles,
  violations) are `modules.module`, a path. A clone pair is its report and its two units.
  A cycle is matched by its members: a BEFORE and an AFTER cycle that share a module.
- **Test code** is reported separately everywhere: units by `is_test`, clone pairs when a
  side is a test unit, edges by their unit edges with and without a test end, cycles when
  every member is a test module.

Views (`compare/views.sql`; `compare/sides.sql` holds the `side_*` views of one side):

- **`compare_units(unit_key, crate, kind, qualified_name, status, is_test, text_changed,
  ambiguous, paired_by, before_unit_id, after_unit_id, path_before, path_after,
  module_before, module_after, start_line_before, start_line_after, loc_before,
  loc_after, before, after)`**: one row per unit on either side, `kept`, `appeared` or
  `disappeared`; `text_changed` compares md5 hashes of the unit's lines (start to end
  line, each with its trailing whitespace removed); `before` and `after` are the whole
  `side_units` rows (every `unit_quality` column) as structs.
- **`compare_unit_axes(…, column_name, axis, rule, before_value, after_value, delta,
  change, effect, obstacle_before, obstacle_after)`**: one row per unit and `unit_quality`
  column, the columns read from the view at load time, so columns added to `unit_quality`
  appear without a change here. **`compare_column_rules(column_name, axis, rule)`** says
  how a column reads: `lower` (a size: loc, clone pairs, fan-in and fan-out, parameters,
  debt lines), `null_is_better` (cycle), `true_is_better` (has_doc),
  `unreferenced_is_worse` (reference_status), `none` (layer, visibility, references); an
  unlisted column is `lower` when it is a number, `none` otherwise.
- **`compare_clones`**, **`compare_edges`** (module and crate edges with unit edges in
  total, production and test, and occurrences on both sides), **`compare_cycles`**
  (sizes, members added and removed, `split`, `merged`),
  **`compare_layer_violations`**.

Queries (`compare/queries/`): `summary` (per obstacle list and scope: totals on both sides,
kept, improved, regressed, changed, appeared, disappeared), `units`, `unit_axes`, `clones`,
`edges`, `cycles`, `layer_violations`; each header states its rows and parameters. Nothing
is thresholded; rows are ordered by the size of the change.

Measured (the record has the runs): on the September 2026 sweep (e223bc20 → 46c37290) a
`code-db compare` takes 3.4–5.7 s with both caches built, of which BEFORE's copy is
1.7–2.8 s and the query 0.01–1.3 s. Without the cache (the views live) the fixture's
queries had not finished after 4.5 minutes (where the time goes was not traced); `code-db`
and the check always read `refresh.sql`.

`check.py fixture` also runs `check_compare.py` (`uv run --no-project --with duckdb==2.0.0
python duckdb-ext/sql/check_compare.py [--print]` runs it alone): two snapshots of one
workspace with a known split, a rename, a duplicate removed, a cycle broken, a layer
violation introduced (closing a new cycle), a changed constant and two targets sharing a
root descriptor, each indexed with rust-analyzer and read through `refresh.sql`, with every
row of the views and queries asserted. `code-db`'s end-to-end test runs `compare` on a
renamed function and on a BEFORE without indexes.

## Deliberately not here

- **History**: blobs, unit changes, lineage, churn, co-change and the before/after
  comparison were removed with the decision of 2026-10-29; the comparison is to come back
  as a diff of obstacle lists over two SCIP indexes.
- **Complexity and nesting depth**, the exhaustive-`match` check (above).
- Go and TypeScript (their SCIP indexers and the unit mapping); SCIP relationships
  (rust-analyzer 1.99.1 writes none), so no edge from an impl's method to the trait
  method it implements.
