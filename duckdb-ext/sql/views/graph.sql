-- The code graph: which unit references which, how units nest, which type an impl is
-- for and which trait it implements, the module and crate edges folded from the unit
-- edges and the module cycles they form, and co-change on those edges. Every view is
-- derived from units, tags, impl_parts (views/units.sql), scip_occurrences
-- (views/scip.sql) and unit_history (views/lineage.sql).
--
-- Optional, read after views/scip.sql (which it needs) and views/snapshot.sql: nothing
-- else depends on it. cache.sql and refresh.sql keep these views as views; they read
-- whichever layers are in place.
--
-- One derivation, two trees. The views from unit_occurrences to module_cycles are each a
-- table macro graph_<view>_over(...) applied to the current tree: the macro's arguments
-- name its input relations (files_t, units_t, tags_t, impl_parts_t, occurrences_t, with
-- the columns of files, units, tags, impl_parts and scip_occurrences), read with
-- query_table, and a macro reads the steps before it by applying their macros to the same
-- names. The before/after comparison (end of this file) applies the same macros to an
-- earlier commit's tree (views/snapshot.sql) and the index of that tree (scip_before).
-- The macros are named graph_*, which refresh.sql's derive_key leaves out: no cached layer
-- is derived through them, and this file is optional.
--
-- Locating an occurrence. An occurrence is inside a unit when its lines lie within the
-- unit's lines [start_line, end_line] (units have lines only; a definition spanning the
-- whole file, as rust-analyzer writes a file module's, lies in no unit). It is
-- attributed to the innermost such unit: the containing unit with the greatest
-- unit_id. Units nest as the syntax tree does and unit_id numbers them in source order
-- with an enclosing unit before what it contains, so among nested units that is the
-- innermost. Two units that do not nest but share a line (`fn a() {} fn b() {}` on one
-- line) both contain an occurrence on that line, and it goes to the later one; this
-- repository had no such pair when this was written. An occurrence on the line where a
-- unit starts or ends but outside it (code before `fn` on the same line) goes to that
-- unit too.
--
-- Accepted limitations of the edges:
--   - `local N` symbols are excluded from unit_references: they close within one
--     document (closure parameters, bindings, type parameters).
--   - A call through a generic bound or a trait object resolves, in rust-analyzer's
--     index, to the trait's method definition, not to an impl's: the edge goes to the
--     trait method's unit (a call on a concrete type reaches the impl's method).
--   - Occurrences inside macro invocations are the ones rust-analyzer maps back to the
--     source through the expansion; via_macro flags edges whose every source
--     occurrence lies inside a generated_or_macro node (by lines, as tags do).

-- unit_occurrences(file_id, symbol, start_line, start_column, end_line, end_column,
--     is_definition, is_local, unit_id, in_macro): every occurrence in a source file
-- with the innermost unit containing it (NULL outside every unit: `use` declarations
-- and other items at file level), and whether a generated_or_macro tag's lines contain
-- it. Local symbols are kept (is_local) so that their share can be counted.
CREATE OR REPLACE MACRO graph_unit_occurrences_over(units_t, tags_t, occurrences_t) AS TABLE
WITH unit_lines AS MATERIALIZED (
    SELECT unit_id, file_id, start_line, end_line FROM query_table(units_t)
),
occurrences AS MATERIALIZED (
    SELECT row_number() OVER () AS occurrence_ord, file_id, symbol, start_line, start_column,
           end_line, end_column, is_definition, starts_with(symbol, 'local ') AS is_local
    FROM query_table(occurrences_t) WHERE file_id IS NOT NULL
),
macros AS MATERIALIZED (
    SELECT file_id, start_line, end_line FROM query_table(tags_t) WHERE tag = 'generated_or_macro'
),
located AS MATERIALIZED (
    SELECT o.occurrence_ord, max(u.unit_id) AS unit_id
    FROM occurrences o
    JOIN unit_lines u ON u.file_id = o.file_id AND u.start_line <= o.start_line AND o.end_line <= u.end_line
    GROUP BY o.occurrence_ord
),
in_macro AS MATERIALIZED (
    SELECT DISTINCT o.occurrence_ord
    FROM occurrences o
    JOIN macros m ON m.file_id = o.file_id AND m.start_line <= o.start_line AND o.end_line <= m.end_line
)
SELECT o.file_id, o.symbol, o.start_line, o.start_column, o.end_line, o.end_column,
       o.is_definition, o.is_local, l.unit_id, m.occurrence_ord IS NOT NULL AS in_macro
FROM occurrences o
LEFT JOIN located l USING (occurrence_ord)
LEFT JOIN in_macro m USING (occurrence_ord);

CREATE OR REPLACE VIEW unit_occurrences AS FROM graph_unit_occurrences_over('units', 'tags', 'scip_occurrences');

-- unit_tests(unit_id, is_test): whether a test-tagged node of the unit's file contains
-- the unit's lines (a #[test] function, a #[cfg(test)] module and what it holds, every
-- unit of a file under tests/).
CREATE OR REPLACE MACRO graph_unit_tests_over(units_t, tags_t) AS TABLE
WITH test_lines AS MATERIALIZED (
    SELECT DISTINCT file_id, start_line, end_line FROM query_table(tags_t) WHERE tag = 'test'
)
SELECT u.unit_id,
       EXISTS (SELECT 1 FROM test_lines t
               WHERE t.file_id = u.file_id AND t.start_line <= u.start_line AND u.end_line <= t.end_line) AS is_test
FROM query_table(units_t) u;

CREATE OR REPLACE VIEW unit_tests AS FROM graph_unit_tests_over('units', 'tags');

-- unit_references(src_unit_id, dst_unit_id, symbol, occurrences, macro_occurrences,
--     via_macro, ambiguous, src_is_test, dst_is_test): for each non-local symbol, the
-- units holding a reference occurrence of it (source) and the units holding a
-- definition occurrence of it (destination), one row per (source, destination,
-- symbol). occurrences counts the source unit's reference occurrences of the symbol,
-- macro_occurrences those inside a macro invocation; via_macro is true when all of them
-- are. A symbol defined in several units yields an edge to each, flagged ambiguous:
-- rust-analyzer names a symbol by package, not by target, so the same function defined
-- in two integration tests or two binaries of one package is one symbol (and a cfg'd
-- pair of definitions is too). References outside every unit, and symbols defined in
-- no unit (external crates, file modules, consts and statics at file level), give no
-- edge.
CREATE OR REPLACE MACRO graph_unit_references_over(units_t, tags_t, occurrences_t) AS TABLE
WITH located AS MATERIALIZED (
    SELECT symbol, unit_id, is_definition, in_macro FROM graph_unit_occurrences_over(units_t, tags_t, occurrences_t)
    WHERE NOT is_local AND unit_id IS NOT NULL
),
defs AS MATERIALIZED (
    SELECT symbol, unit_id, count(*) OVER (PARTITION BY symbol) > 1 AS ambiguous
    FROM (SELECT DISTINCT symbol, unit_id FROM located WHERE is_definition)
),
refs AS (
    SELECT unit_id, symbol, count(*) AS occurrences, count(*) FILTER (WHERE in_macro) AS macro_occurrences
    FROM located WHERE NOT is_definition
    GROUP BY ALL
),
tests AS MATERIALIZED (
    SELECT * FROM graph_unit_tests_over(units_t, tags_t)
)
SELECT r.unit_id AS src_unit_id, d.unit_id AS dst_unit_id, r.symbol, r.occurrences, r.macro_occurrences,
       r.macro_occurrences = r.occurrences AS via_macro, d.ambiguous,
       ts.is_test AS src_is_test, td.is_test AS dst_is_test
FROM refs r
JOIN defs d USING (symbol)
JOIN tests ts ON ts.unit_id = r.unit_id
JOIN tests td ON td.unit_id = d.unit_id;

CREATE OR REPLACE VIEW unit_references AS FROM graph_unit_references_over('units', 'tags', 'scip_occurrences');

-- unit_edges(src_unit_id, dst_unit_id, symbols, occurrences, macro_occurrences, is_self,
--     via_macro, ambiguous, src_is_test, dst_is_test): unit_references folded over
-- symbols. symbols is the number of distinct symbols; is_self marks a unit referencing
-- what it defines itself (a recursive call, an enum's variants named in its own
-- definition); via_macro (ambiguous) is true when every symbol of the edge is.
CREATE OR REPLACE MACRO graph_unit_edges_over(units_t, tags_t, occurrences_t) AS TABLE
SELECT src_unit_id, dst_unit_id, count(*) AS symbols, sum(occurrences) AS occurrences,
       sum(macro_occurrences) AS macro_occurrences, src_unit_id = dst_unit_id AS is_self,
       bool_and(via_macro) AS via_macro, bool_and(ambiguous) AS ambiguous,
       any_value(src_is_test) AS src_is_test, any_value(dst_is_test) AS dst_is_test
FROM graph_unit_references_over(units_t, tags_t, occurrences_t)
GROUP BY src_unit_id, dst_unit_id;

CREATE OR REPLACE VIEW unit_edges AS FROM graph_unit_edges_over('units', 'tags', 'scip_occurrences');

-- unit_contains(parent_unit_id, child_unit_id): the nesting of units (units.parent_unit_id).
CREATE OR REPLACE VIEW unit_contains AS
SELECT parent_unit_id, unit_id AS child_unit_id FROM units WHERE parent_unit_id IS NOT NULL;

-- unit_ancestors(unit_id, ancestor_id, depth): every unit with itself (depth 0) and
-- each enclosing unit (depth 1 for the parent, ...), along unit_contains.
CREATE OR REPLACE MACRO graph_unit_ancestors_over(units_t) AS TABLE
WITH RECURSIVE unit_rows AS MATERIALIZED (
    SELECT unit_id, parent_unit_id FROM query_table(units_t)
),
chain(unit_id, ancestor_id, depth) AS (
    SELECT unit_id, unit_id, 0 FROM unit_rows
    UNION ALL
    SELECT c.unit_id, u.parent_unit_id, c.depth + 1
    FROM chain c JOIN unit_rows u ON u.unit_id = c.ancestor_id
    WHERE u.parent_unit_id IS NOT NULL
)
SELECT * FROM chain;

CREATE OR REPLACE VIEW unit_ancestors AS FROM graph_unit_ancestors_over('units');

-- impl_targets(impl_unit_id, part, symbol, target_unit_id): for each impl unit, its
-- `type` part and (for a trait impl) its `trait` part (impl_parts), the symbol of the
-- last reference occurrence in the part's head (NULL when the head has none), and the
-- `type` unit that defines that symbol (NULL when none does: an external trait or type,
-- a type parameter, a primitive). One row per part and target unit.
CREATE OR REPLACE MACRO graph_impl_targets_over(units_t, tags_t, impl_parts_t, occurrences_t) AS TABLE
WITH parts AS MATERIALIZED (
    SELECT u.unit_id AS impl_unit_id, p.*
    FROM query_table(impl_parts_t) p JOIN query_table(units_t) u ON u.file_id = p.file_id AND u.node_id = p.node_id AND u.kind = 'impl'
),
occurrences AS MATERIALIZED (
    SELECT file_id, symbol, start_line, start_column, is_definition, is_local, unit_id FROM graph_unit_occurrences_over(units_t, tags_t, occurrences_t)
),
heads AS MATERIALIZED (
    SELECT p.impl_unit_id, p.part,
           arg_max(o.symbol, {'line': o.start_line, 'column': o.start_column}) AS symbol
    FROM parts p
    JOIN occurrences o ON o.file_id = p.file_id AND NOT o.is_definition
         AND (o.start_line > p.start_line OR (o.start_line = p.start_line AND o.start_column >= p.start_column))
         AND (o.start_line < p.head_end_line OR (o.start_line = p.head_end_line AND o.start_column < p.head_end_column))
    GROUP BY ALL
),
type_defs AS MATERIALIZED (
    SELECT DISTINCT o.symbol, o.unit_id
    FROM occurrences o JOIN query_table(units_t) u ON u.unit_id = o.unit_id AND u.kind = 'type'
    WHERE o.is_definition AND NOT o.is_local
)
SELECT p.impl_unit_id, p.part, h.symbol, d.unit_id AS target_unit_id
FROM parts p
LEFT JOIN heads h USING (impl_unit_id, part)
LEFT JOIN type_defs d ON d.symbol = h.symbol;

CREATE OR REPLACE VIEW impl_targets AS FROM graph_impl_targets_over('units', 'tags', 'impl_parts', 'scip_occurrences');

-- impl_for(impl_unit_id, type_unit_id) and implements(impl_unit_id, trait_unit_id): the
-- impl_targets that resolved to a unit.
CREATE OR REPLACE MACRO graph_impl_for_over(units_t, tags_t, impl_parts_t, occurrences_t) AS TABLE
SELECT impl_unit_id, target_unit_id AS type_unit_id FROM graph_impl_targets_over(units_t, tags_t, impl_parts_t, occurrences_t)
WHERE part = 'type' AND target_unit_id IS NOT NULL;

CREATE OR REPLACE VIEW impl_for AS FROM graph_impl_for_over('units', 'tags', 'impl_parts', 'scip_occurrences');

CREATE OR REPLACE VIEW implements AS
SELECT impl_unit_id, target_unit_id AS trait_unit_id FROM impl_targets
WHERE part = 'trait' AND target_unit_id IS NOT NULL;

-- file_crate(file_id, crate): the crate a file belongs to, from this repository's
-- layout: crates/<name>/... is <name>, preview-plugin/... is preview-plugin,
-- duckdb-ext/<name>/... is <name>, anything else the root package horizon. The name is
-- the directory's, which is the package name in this repository.
CREATE OR REPLACE MACRO graph_file_crate_over(files_t) AS TABLE
SELECT file_id,
       CASE WHEN path LIKE 'crates/%/%' OR path LIKE 'duckdb-ext/%/%' THEN split_part(path, '/', 2)
            WHEN path LIKE 'preview-plugin/%' THEN 'preview-plugin'
            ELSE 'horizon' END AS crate
FROM query_table(files_t);

CREATE OR REPLACE VIEW file_crate AS FROM graph_file_crate_over('files');

-- modules(module, file_id, module_unit_id, crate, module_path): every file as an
-- implicit module (module = its path, module_unit_id NULL) and every module unit (module
-- = `<path>::<qualified name>` with `::` between names, e.g. src/shapes.rs::tests).
-- module_path places each module in its crate's module tree by Rust's file-module
-- convention, as a `/`-separated path: a crate root (`…/lib.rs`, `…/main.rs`) and a
-- `…/mod.rs` are their directory, any other file its path without `.rs`, and a module
-- unit its file's module_path followed by its names (src/shapes.rs::tests is
-- src/shapes/tests; a `mod x;` declaration in src/lib.rs is src/x, the module_path of
-- src/x.rs). A module is an ancestor of another when its module_path is a prefix of the
-- other's at a `/`. `#[path]` attributes are not read, and a binary under src/bin/ is
-- placed under the library's root.
CREATE OR REPLACE MACRO graph_modules_over(files_t, units_t) AS TABLE
WITH file_modules AS MATERIALIZED (
    SELECT f.path, f.file_id, c.crate,
           CASE WHEN regexp_matches(f.path, '(^|/)(lib|main|mod)\.rs$')
                THEN regexp_replace(f.path, '/?(lib|main|mod)\.rs$', '')
                ELSE regexp_replace(f.path, '\.rs$', '') END AS module_path
    FROM query_table(files_t) f JOIN graph_file_crate_over(files_t) c USING (file_id)
)
SELECT path AS module, file_id, NULL::BIGINT AS module_unit_id, crate, module_path
FROM file_modules
UNION ALL
SELECT m.path || '::' || replace(u.qualified_name, '.', '::'), u.file_id, u.unit_id, m.crate,
       m.module_path || '/' || replace(u.qualified_name, '.', '/')
FROM query_table(units_t) u JOIN file_modules m USING (file_id)
WHERE u.kind = 'module';

CREATE OR REPLACE VIEW modules AS FROM graph_modules_over('files', 'units');

-- unit_module(unit_id, module, module_unit_id, file_id, crate, module_path): the module a
-- unit belongs to: the nearest module unit among the unit itself and its enclosing units
-- (a module unit belongs to the module it defines), else its file's implicit module.
CREATE OR REPLACE MACRO graph_unit_module_over(files_t, units_t) AS TABLE
WITH unit_rows AS MATERIALIZED (
    SELECT unit_id, file_id FROM query_table(units_t)
),
nearest AS (
    SELECT a.unit_id, arg_min(a.ancestor_id, a.depth) AS module_unit_id
    FROM graph_unit_ancestors_over(units_t) a JOIN query_table(units_t) m ON m.unit_id = a.ancestor_id
    WHERE m.kind = 'module'
    GROUP BY a.unit_id
)
SELECT u.unit_id, m.module, n.module_unit_id, u.file_id, m.crate, m.module_path
FROM unit_rows u
LEFT JOIN nearest n USING (unit_id)
JOIN graph_modules_over(files_t, units_t) m ON m.file_id = u.file_id AND m.module_unit_id IS NOT DISTINCT FROM n.module_unit_id;

CREATE OR REPLACE VIEW unit_module AS FROM graph_unit_module_over('files', 'units');

-- module_edges(src_module, dst_module, src_crate, dst_crate, unit_edges, occurrences,
--     test_unit_edges, macro_unit_edges, ambiguous_unit_edges, nested_modules,
--     same_type_unit_edges) and crate_edges(src_crate, dst_crate, unit_edges, occurrences,
--     test_unit_edges, macro_unit_edges, ambiguous_unit_edges): unit_edges between
-- different modules (crates), self edges excluded. unit_edges counts the unit edges,
-- occurrences their occurrences; test_unit_edges counts the edges with a test unit at
-- either end, macro_unit_edges (ambiguous_unit_edges) those that are via_macro
-- (ambiguous). nested_modules marks an edge between a module and one of its ancestors
-- or descendants (modules.module_path; in one crate), same_type_unit_edges counts the
-- unit edges whose two ends both lie in impls (the nearest enclosing impl of each end,
-- the end itself included) of one type (impl_for): one type's impl blocks split over
-- several files. module_cycle_edges leaves out the edges that are nested_modules or whose
-- every unit edge is same-type.
CREATE OR REPLACE MACRO graph_module_edges_over(files_t, units_t, tags_t, impl_parts_t, occurrences_t) AS TABLE
WITH m AS MATERIALIZED (
    SELECT unit_id, module, crate, module_path FROM graph_unit_module_over(files_t, units_t)
),
-- The type of each unit's nearest enclosing impl that has one.
impl_type AS MATERIALIZED (
    SELECT a.unit_id, arg_min(f.type_unit_id, a.depth) AS type_unit_id
    FROM graph_unit_ancestors_over(units_t) a
    JOIN graph_impl_for_over(units_t, tags_t, impl_parts_t, occurrences_t) f ON f.impl_unit_id = a.ancestor_id
    GROUP BY a.unit_id
)
SELECT ms.module AS src_module, md.module AS dst_module, ms.crate AS src_crate, md.crate AS dst_crate,
       count(*) AS unit_edges, sum(e.occurrences) AS occurrences,
       count(*) FILTER (WHERE e.src_is_test OR e.dst_is_test) AS test_unit_edges,
       count(*) FILTER (WHERE e.via_macro) AS macro_unit_edges,
       count(*) FILTER (WHERE e.ambiguous) AS ambiguous_unit_edges,
       ms.crate = md.crate AND (starts_with(md.module_path || '/', ms.module_path || '/')
                                OR starts_with(ms.module_path || '/', md.module_path || '/')) AS nested_modules,
       count(*) FILTER (WHERE ts.type_unit_id = td.type_unit_id) AS same_type_unit_edges
FROM graph_unit_edges_over(units_t, tags_t, occurrences_t) e
JOIN m ms ON ms.unit_id = e.src_unit_id
JOIN m md ON md.unit_id = e.dst_unit_id
LEFT JOIN impl_type ts ON ts.unit_id = e.src_unit_id
LEFT JOIN impl_type td ON td.unit_id = e.dst_unit_id
WHERE NOT e.is_self AND ms.module <> md.module
GROUP BY ms.module, md.module, ms.crate, md.crate, ms.module_path, md.module_path;

CREATE OR REPLACE VIEW module_edges AS FROM graph_module_edges_over('files', 'units', 'tags', 'impl_parts', 'scip_occurrences');

CREATE OR REPLACE VIEW crate_edges AS
WITH c AS MATERIALIZED (
    SELECT u.unit_id, fc.crate FROM units u JOIN file_crate fc USING (file_id)
)
SELECT cs.crate AS src_crate, cd.crate AS dst_crate,
       count(*) AS unit_edges, sum(e.occurrences) AS occurrences,
       count(*) FILTER (WHERE e.src_is_test OR e.dst_is_test) AS test_unit_edges,
       count(*) FILTER (WHERE e.via_macro) AS macro_unit_edges,
       count(*) FILTER (WHERE e.ambiguous) AS ambiguous_unit_edges
FROM unit_edges e
JOIN c cs ON cs.unit_id = e.src_unit_id
JOIN c cd ON cd.unit_id = e.dst_unit_id
WHERE NOT e.is_self AND cs.crate <> cd.crate
GROUP BY ALL;

-- module_cycle_edges(cycle, src_module, dst_module, unit_edges, occurrences,
--     test_unit_edges, macro_unit_edges, ambiguous_unit_edges): the module edges inside
-- each strongly connected group of modules (two modules are in one group when each
-- reaches the other along the edges). cycle names the group by its least member.
-- module_cycles(cycle, crate, modules, members, edges, unit_edges, occurrences): one
-- row per group, with its members and the counts of its inner edges. Reachability is
-- the transitive closure of the edges, by a recursive common table expression that
-- adds only new pairs (UNION).
-- The edges are module_edges without the structural ones: those between a module and its
-- ancestor or descendant (nested_modules) and those whose every unit edge joins impls of
-- one type (same_type_unit_edges = unit_edges). Parameter (DuckDB variable; default
-- when unset): cycle_edges 'collapsed' ('raw': every module edge, as before this rule).
CREATE OR REPLACE MACRO graph_module_cycle_edges_over(files_t, units_t, tags_t, impl_parts_t, occurrences_t) AS TABLE
WITH RECURSIVE edges AS MATERIALIZED (
    SELECT src_module, dst_module, unit_edges, occurrences, test_unit_edges, macro_unit_edges,
           ambiguous_unit_edges
    FROM graph_module_edges_over(files_t, units_t, tags_t, impl_parts_t, occurrences_t)
    WHERE CASE coalesce(getvariable('cycle_edges'), 'collapsed')
        WHEN 'raw' THEN true
        WHEN 'collapsed' THEN NOT nested_modules AND same_type_unit_edges < unit_edges
        ELSE error('module_cycle_edges: cycle_edges is collapsed or raw') END
),
reach(src, dst) AS (
    SELECT src_module, dst_module FROM edges
    UNION
    SELECT r.src, e.dst_module FROM reach r JOIN edges e ON e.src_module = r.dst
),
reach_rows AS MATERIALIZED (
    SELECT * FROM reach
),
groups AS MATERIALIZED (
    SELECT a.src AS module, min(a.dst) AS cycle
    FROM reach_rows a JOIN reach_rows b ON b.src = a.dst AND b.dst = a.src
    GROUP BY a.src
)
SELECT gs.cycle, e.*
FROM edges e
JOIN groups gs ON gs.module = e.src_module
JOIN groups gd ON gd.module = e.dst_module AND gd.cycle = gs.cycle;

CREATE OR REPLACE VIEW module_cycle_edges AS FROM graph_module_cycle_edges_over('files', 'units', 'tags', 'impl_parts', 'scip_occurrences');

CREATE OR REPLACE MACRO graph_module_cycles_over(files_t, units_t, tags_t, impl_parts_t, occurrences_t) AS TABLE
WITH crates AS MATERIALIZED (
    SELECT module, crate FROM graph_modules_over(files_t, units_t)
)
SELECT e.cycle, any_value(c.crate) AS crate, count(DISTINCT e.src_module) AS modules,
       list(DISTINCT e.src_module ORDER BY e.src_module) AS members, count(*) AS edges,
       sum(e.unit_edges) AS unit_edges, sum(e.occurrences) AS occurrences
FROM graph_module_cycle_edges_over(files_t, units_t, tags_t, impl_parts_t, occurrences_t) e
JOIN crates c ON c.module = e.src_module
GROUP BY e.cycle;

CREATE OR REPLACE VIEW module_cycles AS FROM graph_module_cycles_over('files', 'units', 'tags', 'impl_parts', 'scip_occurrences');

-- crate_cycles(cycle, crates, members, edges, unit_edges, occurrences): the same over
-- crate_edges. Cargo forbids dependency cycles between crates, so a row here means
-- either an aggregation error or a cycle Cargo allows (a dev-dependency used by test
-- code).
CREATE OR REPLACE VIEW crate_cycles AS
WITH RECURSIVE edges AS MATERIALIZED (
    SELECT src_crate, dst_crate, unit_edges, occurrences FROM crate_edges
),
reach(src, dst) AS (
    SELECT src_crate, dst_crate FROM edges
    UNION
    SELECT r.src, e.dst_crate FROM reach r JOIN edges e ON e.src_crate = r.dst
),
reach_rows AS MATERIALIZED (
    SELECT * FROM reach
),
groups AS MATERIALIZED (
    SELECT a.src AS crate, min(a.dst) AS cycle
    FROM reach_rows a JOIN reach_rows b ON b.src = a.dst AND b.dst = a.src
    GROUP BY a.src
)
SELECT gs.cycle, count(DISTINCT e.src_crate) AS crates, list(DISTINCT e.src_crate ORDER BY e.src_crate) AS members,
       count(*) AS edges, sum(e.unit_edges) AS unit_edges, sum(e.occurrences) AS occurrences
FROM edges e
JOIN groups gs ON gs.crate = e.src_crate
JOIN groups gd ON gd.crate = e.dst_crate AND gd.cycle = gs.cycle
GROUP BY gs.cycle;

-- Co-change on units. unit_commits(sha, unit_id): the commits of each current unit's
-- lineage (unit_history, views/lineage.sql: its own changes and those of the units it
-- continues; two current units with the same identity, such as two `impl Foo` blocks of
-- one file, are both credited), without the commits at which that lineage links an
-- identity (unit_history.via_link: a file_rename, content_match, body_move or inherited
-- link), so that two units moved, renamed or split in one commit do not share that
-- commit. As in
-- queries/file_cochange.sql, commits touching more than max_commit_files files (all
-- files, every language) are dropped, and merges count only with include_merges.
-- Parameters (DuckDB variables; default when unset): max_commit_files 50;
-- cochange_link_commits false (true: keep the link commits, as before this rule);
-- include_merges (00_load.sql, false).
CREATE OR REPLACE VIEW unit_commits AS
WITH eligible AS (
    SELECT fc.sha
    FROM file_changes fc
    JOIN commits c USING (sha)
    WHERE NOT c.is_merge OR getvariable('include_merges')
    GROUP BY fc.sha
    HAVING count(DISTINCT fc.path) <= coalesce(getvariable('max_commit_files'), 50)
)
SELECT DISTINCT h.sha, h.unit_id
FROM unit_history h
JOIN eligible USING (sha)
WHERE NOT h.via_link OR coalesce(CAST(getvariable('cochange_link_commits') AS BOOLEAN), false);

-- unit_cochange(unit_a, unit_b, shared_commits, commits_a, commits_b, coupling_pct,
--     same_file, nested): pairs of current units (unit_a < unit_b) changed together in at
-- least min_shared commits of unit_commits. commits_a and commits_b count each unit's
-- commits there; coupling_pct is shared_commits over the larger of the two (the basis
-- `max` of file_cochange). nested marks a unit and one of its enclosing units (with
-- attribution 'all', every change to a method is a change to its impl). Test units
-- (unit_tests) are left out unless include_tests: a pair with a test unit on either side
-- is a test changing with what it tests, or two tests written from one template.
-- Parameters: min_shared 3, include_tests false, and those of unit_commits.
CREATE OR REPLACE VIEW unit_cochange AS
WITH touched AS MATERIALIZED (
    SELECT c.* FROM unit_commits c
    WHERE coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false)
       OR c.unit_id NOT IN (SELECT unit_id FROM unit_tests WHERE is_test)
),
per_unit AS (
    SELECT unit_id, count(*) AS commits FROM touched GROUP BY unit_id
),
pairs AS (
    SELECT a.unit_id AS unit_a, b.unit_id AS unit_b, count(*) AS shared_commits
    FROM touched a JOIN touched b ON a.sha = b.sha AND a.unit_id < b.unit_id
    GROUP BY ALL
    HAVING count(*) >= coalesce(getvariable('min_shared'), 3)
),
unit_rows AS MATERIALIZED (
    SELECT unit_id, file_id FROM units
)
SELECT p.unit_a, p.unit_b, p.shared_commits, ca.commits AS commits_a, cb.commits AS commits_b,
       round(100.0 * p.shared_commits / greatest(ca.commits, cb.commits), 1) AS coupling_pct,
       ua.file_id = ub.file_id AS same_file,
       EXISTS (SELECT 1 FROM unit_ancestors x
               WHERE x.depth > 0 AND ((x.unit_id = p.unit_b AND x.ancestor_id = p.unit_a)
                                      OR (x.unit_id = p.unit_a AND x.ancestor_id = p.unit_b))) AS nested
FROM pairs p
JOIN per_unit ca ON ca.unit_id = p.unit_a
JOIN per_unit cb ON cb.unit_id = p.unit_b
JOIN unit_rows ua ON ua.unit_id = p.unit_a
JOIN unit_rows ub ON ub.unit_id = p.unit_b;

-- edge_cochange(src_unit_id, dst_unit_id, symbols, occurrences, via_macro, ambiguous,
--     src_is_test, dst_is_test, shared_commits, commits_src, commits_dst): every unit edge
-- that is not a self edge, with how often its two ends changed together in
-- unit_commits (0 when never; not subject to min_shared) and each end's commits.
CREATE OR REPLACE VIEW edge_cochange AS
WITH touched AS MATERIALIZED (
    SELECT * FROM unit_commits
),
per_unit AS (
    SELECT unit_id, count(*) AS commits FROM touched GROUP BY unit_id
),
edges AS MATERIALIZED (
    SELECT * FROM unit_edges WHERE NOT is_self
),
shared AS (
    SELECT e.src_unit_id, e.dst_unit_id, count(*) AS shared_commits
    FROM edges e
    JOIN touched a ON a.unit_id = e.src_unit_id
    JOIN touched b ON b.unit_id = e.dst_unit_id AND b.sha = a.sha
    GROUP BY ALL
)
SELECT e.src_unit_id, e.dst_unit_id, e.symbols, e.occurrences, e.via_macro, e.ambiguous,
       e.src_is_test, e.dst_is_test, coalesce(s.shared_commits, 0) AS shared_commits,
       coalesce(cs.commits, 0) AS commits_src, coalesce(cd.commits, 0) AS commits_dst
FROM edges e
LEFT JOIN shared s USING (src_unit_id, dst_unit_id)
LEFT JOIN per_unit cs ON cs.unit_id = e.src_unit_id
LEFT JOIN per_unit cd ON cd.unit_id = e.dst_unit_id;

-- cochange_without_reference(unit_a, unit_b, shared_commits, commits_a, commits_b,
--     coupling_pct): the pairs of unit_cochange in different files with no unit edge in
-- either direction and a coupling of at least min_coupling_pct: units that change
-- together without referencing each other (the "modularity violation" shape of Mo et
-- al., TSE 2021, there between files). Only the two units themselves are compared: an
-- edge from a method of an impl does not count as an edge from the impl. Test units are
-- left out as in unit_cochange.
-- Parameters: min_coupling_pct 30, and those of unit_cochange (include_tests among them).
CREATE OR REPLACE VIEW cochange_without_reference AS
-- (The edges are compared as unordered pairs, an equality anti-join: an OR of the two
-- directions made the query about five times slower over the cache.)
WITH linked AS MATERIALIZED (
    SELECT DISTINCT least(src_unit_id, dst_unit_id) AS unit_a, greatest(src_unit_id, dst_unit_id) AS unit_b
    FROM unit_edges
)
SELECT c.unit_a, c.unit_b, c.shared_commits, c.commits_a, c.commits_b, c.coupling_pct
FROM unit_cochange c
WHERE NOT c.same_file
  AND c.coupling_pct >= coalesce(getvariable('min_coupling_pct'), 30)
  AND NOT EXISTS (SELECT 1 FROM linked l WHERE l.unit_a = c.unit_a AND l.unit_b = c.unit_b);

-- The before/after comparison (README.md, "Before/after comparison").
--
-- unit_fan_in(unit_id, fan_in_files, reference_occurrences): for each unit with a
-- symbol defined within its lines (a definition occurrence inside [start_line,
-- end_line], so an enclosing unit includes what it contains), the distinct files other
-- than its own that hold a reference occurrence of one of those symbols, and the number of
-- those occurrences: the fan-in of queries/unit_fan_in_churn.sql, over the relations
-- graph_unit_fan_in_over names (units_t, occurrences_t).
CREATE OR REPLACE MACRO graph_unit_fan_in_over(units_t, occurrences_t) AS TABLE
WITH unit_rows AS MATERIALIZED (
    SELECT unit_id, file_id, start_line, end_line FROM query_table(units_t)
),
occurrences AS MATERIALIZED (
    SELECT file_id, symbol, start_line, end_line, is_definition, is_reference
    FROM query_table(occurrences_t) WHERE file_id IS NOT NULL
),
defs AS MATERIALIZED (
    SELECT DISTINCT u.unit_id, u.file_id, o.symbol
    FROM unit_rows u
    JOIN occurrences o
      ON o.file_id = u.file_id AND o.is_definition
     AND o.start_line >= u.start_line AND o.end_line <= u.end_line
)
SELECT d.unit_id, count(DISTINCT o.file_id) AS fan_in_files, count(*) AS reference_occurrences
FROM defs d
JOIN occurrences o ON o.symbol = d.symbol AND o.is_reference AND o.file_id <> d.file_id
GROUP BY d.unit_id;

CREATE OR REPLACE VIEW unit_fan_in AS FROM graph_unit_fan_in_over('units', 'scip_occurrences');

-- snapshot_scip_occurrences: the occurrences of the SCIP index `scip_before` (an index of
-- compare_before's tree, made outside DuckDB as the current one is) in the shape of
-- scip_occurrences, with file_id from joining `document` to snapshot_files.path. Empty
-- when scip_before is unset (the index of the current tree is then opened when the view
-- is bound, and never read).
CREATE OR REPLACE VIEW snapshot_scip_occurrences AS
SELECT f.file_id, o.*
FROM scip_document_occurrences_of(coalesce(getvariable('scip_before'), getvariable('scip_index'))) o
LEFT JOIN snapshot_files f ON f.path = o.document
WHERE getvariable('scip_before') IS NOT NULL;

-- The graph of the before side: the views above over snapshot_files, snapshot_units,
-- snapshot_tags (empty: blobs have no tags), snapshot_impl_parts and
-- snapshot_scip_occurrences. Without tags, the before side's unit edges are never
-- via_macro nor test edges; unit_edges counts, module edges and cycles do not read them.
CREATE OR REPLACE VIEW snapshot_unit_module AS FROM graph_unit_module_over('snapshot_files', 'snapshot_units');

CREATE OR REPLACE VIEW snapshot_unit_fan_in AS
FROM graph_unit_fan_in_over('snapshot_units', 'snapshot_scip_occurrences');

CREATE OR REPLACE VIEW snapshot_module_edges AS
FROM graph_module_edges_over('snapshot_files', 'snapshot_units', 'snapshot_tags', 'snapshot_impl_parts',
                             'snapshot_scip_occurrences');

CREATE OR REPLACE VIEW snapshot_module_cycles AS
FROM graph_module_cycles_over('snapshot_files', 'snapshot_units', 'snapshot_tags', 'snapshot_impl_parts',
                              'snapshot_scip_occurrences');

-- module_renames(module_before, module): each module of the before side
-- (snapshot_unit_module's modules) named as at HEAD: its file's path carried through the
-- file renames of compare_before..HEAD (file_changes.old_path -> path, gix's rename
-- detection, in commit order: a path is carried by the first rename of it after the
-- commit the walk reached it at), the rest of the name (`::` and inline module names)
-- kept. An inline module renamed in place is not followed.
CREATE OR REPLACE VIEW module_renames AS
WITH RECURSIVE before AS MATERIALIZED (
    SELECT ord FROM compare_commit
),
renames AS MATERIALIZED (
    SELECT o.ord, f.old_path, f.path,
           coalesce(lag(o.ord) OVER (PARTITION BY f.old_path ORDER BY o.ord), -1) AS prev_ord
    FROM (SELECT DISTINCT sha, old_path, path FROM file_changes
          WHERE old_path IS NOT NULL AND old_path <> path) f
    JOIN commit_order o USING (sha)
    JOIN compare_range r USING (sha)
),
paths AS MATERIALIZED (
    SELECT DISTINCT path FROM snapshot_files
),
walk(start, path, ord) AS (
    SELECT p.path, p.path, b.ord FROM paths p, before b
    UNION ALL
    SELECT w.start, r.path, r.ord
    FROM walk w JOIN renames r ON r.old_path = w.path AND r.ord > w.ord AND w.ord >= r.prev_ord
),
final AS MATERIALIZED (
    SELECT start, arg_max(path, ord) AS path FROM walk GROUP BY start
),
before_modules AS (
    SELECT DISTINCT m.module, f.path
    FROM snapshot_unit_module m JOIN snapshot_files f USING (file_id)
)
SELECT m.module AS module_before, x.path || substr(m.module, length(m.path) + 1) AS module
FROM before_modules m JOIN final x ON x.start = m.path;
