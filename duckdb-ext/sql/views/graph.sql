-- The code graph: which unit references which, how units nest, which type an impl is for
-- and which trait it implements, the module and crate edges folded from the unit edges,
-- the module cycles they form, and the intended layers. Every view is derived from
-- units (views/units.sql), scip_occurrences (views/scip.sql), source_lines
-- (views/files.sql) and the settings' tables (00_load.sql).
--
-- Locating an occurrence. An occurrence belongs to the innermost unit whose range (SCIP's
-- enclosing_range, line and column) contains its start: the containing unit with the
-- greatest unit_id. Every indexed file has its file module as a unit, so an occurrence
-- outside every item (a `use` declaration, a `mod x;` declaration, a file-level macro
-- call) belongs to the file module. A symbol's definition belongs to the unit it defines
-- when it is a unit (an item), else to the unit containing it (a field or variant to its
-- type, a parameter or local to its function).
--
-- Accepted limitations of the edges:
--   - `local N` symbols are excluded from unit_references: they close within one
--     document (closure parameters, bindings, type parameters).
--   - A call through a generic bound or a trait object resolves, in rust-analyzer's
--     index, to the trait's method definition, not to an impl's.
--   - Occurrences inside macro invocations are the ones rust-analyzer maps back to the
--     source through the expansion; references inside `macro_rules!` bodies have none.

-- unit_occurrences(file_id, symbol, start_line, start_column, end_line, end_column,
--     is_definition, is_local, unit_id): every occurrence in a source file with the
-- innermost unit containing it (NULL only in a file without units). Local symbols are
-- kept (is_local).
CREATE OR REPLACE VIEW unit_occurrences AS
WITH unit_ranges AS MATERIALIZED (
    SELECT unit_id, file_id, start_line, start_column, end_line, end_column FROM units
),
occurrences AS MATERIALIZED (
    SELECT row_number() OVER () AS occurrence_ord, file_id, symbol, start_line, start_column,
           end_line, end_column, is_definition, starts_with(symbol, 'local ') AS is_local
    FROM scip_occurrences WHERE file_id IS NOT NULL
),
located AS MATERIALIZED (
    SELECT o.occurrence_ord, max(u.unit_id) AS unit_id
    FROM occurrences o
    JOIN unit_ranges u ON u.file_id = o.file_id AND u.start_line <= o.start_line AND o.start_line <= u.end_line
     AND (u.start_line < o.start_line OR u.start_column <= o.start_column)
     AND (o.start_line < u.end_line OR o.start_column < u.end_column)
    GROUP BY o.occurrence_ord
)
SELECT o.file_id, o.symbol, o.start_line, o.start_column, o.end_line, o.end_column,
       o.is_definition, o.is_local, l.unit_id
FROM occurrences o
LEFT JOIN located l USING (occurrence_ord);

-- symbol_units(symbol, unit_id, ambiguous): the unit each non-local symbol's definitions
-- belong to (above), except that a module's own symbol has none: a reference to a module
-- names a namespace (a path segment `crate::a::b::f`, a `use crate::a;`, a `mod a;`
-- declaration), not code the referrer depends on, and would otherwise draw an edge to
-- every module a path passes through. A symbol with definitions in several units (rust-analyzer names a
-- symbol by package, not by target: one helper defined in two integration tests of a
-- package, or a cfg'd pair of definitions) has a row for each, flagged ambiguous.
CREATE OR REPLACE VIEW symbol_units AS
WITH own AS MATERIALIZED (
    SELECT file_id, symbol, name_line, name_column, unit_id, kind FROM units
),
defs AS (
    SELECT DISTINCT o.symbol, coalesce(w.unit_id, o.unit_id) AS unit_id
    FROM unit_occurrences o
    LEFT JOIN own w ON w.file_id = o.file_id AND w.symbol = o.symbol
         AND w.name_line = o.start_line AND w.name_column = o.start_column
    WHERE o.is_definition AND NOT o.is_local AND coalesce(w.unit_id, o.unit_id) IS NOT NULL
      AND w.kind IS DISTINCT FROM 'module'
)
SELECT symbol, unit_id, count(*) OVER (PARTITION BY symbol) > 1 AS ambiguous FROM defs;

-- unit_references(src_unit_id, dst_unit_id, symbol, occurrences, ambiguous, src_is_test,
--     dst_is_test): for each non-local symbol, the units holding a reference occurrence
-- of it (source) and the units its definitions belong to (destination), one row per
-- (source, destination, symbol); occurrences counts the source's reference occurrences
-- of the symbol. Symbols defined in no source file (external crates, the standard
-- library) give no edge.
CREATE OR REPLACE VIEW unit_references AS
WITH refs AS (
    SELECT unit_id, symbol, count(*) AS occurrences
    FROM unit_occurrences
    WHERE NOT is_definition AND NOT is_local AND unit_id IS NOT NULL
    GROUP BY ALL
),
tests AS MATERIALIZED (
    SELECT unit_id, is_test FROM units
)
SELECT r.unit_id AS src_unit_id, d.unit_id AS dst_unit_id, r.symbol, r.occurrences, d.ambiguous,
       ts.is_test AS src_is_test, td.is_test AS dst_is_test
FROM refs r
JOIN symbol_units d USING (symbol)
JOIN tests ts ON ts.unit_id = r.unit_id
JOIN tests td ON td.unit_id = d.unit_id;

-- unit_edges(src_unit_id, dst_unit_id, symbols, occurrences, is_self, ambiguous,
--     src_is_test, dst_is_test): unit_references folded over symbols. symbols is the
-- number of distinct symbols; is_self marks a unit referencing what it defines itself (a
-- recursive call, a type naming its own variants); ambiguous holds when every symbol of
-- the edge is.
CREATE OR REPLACE VIEW unit_edges AS
SELECT src_unit_id, dst_unit_id, count(*) AS symbols, sum(occurrences) AS occurrences,
       src_unit_id = dst_unit_id AS is_self, bool_and(ambiguous) AS ambiguous,
       any_value(src_is_test) AS src_is_test, any_value(dst_is_test) AS dst_is_test
FROM unit_references
GROUP BY src_unit_id, dst_unit_id;

-- unit_contains(parent_unit_id, child_unit_id): the nesting of units (units.parent_unit_id).
CREATE OR REPLACE VIEW unit_contains AS
SELECT parent_unit_id, unit_id AS child_unit_id FROM units WHERE parent_unit_id IS NOT NULL;

-- unit_ancestors(unit_id, ancestor_id, depth): every unit with itself (depth 0) and
-- each enclosing unit (depth 1 for the parent, ...), along unit_contains.
CREATE OR REPLACE VIEW unit_ancestors AS
WITH RECURSIVE unit_rows AS MATERIALIZED (
    SELECT unit_id, parent_unit_id FROM units
),
chain(unit_id, ancestor_id, depth) AS (
    SELECT unit_id, unit_id, 0 FROM unit_rows
    UNION ALL
    SELECT c.unit_id, u.parent_unit_id, c.depth + 1
    FROM chain c JOIN unit_rows u ON u.unit_id = c.ancestor_id
    WHERE u.parent_unit_id IS NOT NULL
)
SELECT * FROM chain;

-- impls(impl_id, file_id, crate, impl_key, impl_type, impl_trait, header_line, members):
-- the impl blocks as their members' descriptors give them (views/units.sql): one row per
-- impl_key, file and header line (two `impl Foo` blocks of one file are two rows; members
-- with no header line found form one row per key and file). members lists their unit ids.
CREATE OR REPLACE VIEW impls AS
SELECT (row_number() OVER (ORDER BY file_id, min(unit_id)) - 1)::INTEGER AS impl_id,
       file_id, any_value(crate) AS crate, impl_key, any_value(impl_type) AS impl_type,
       any_value(impl_trait) AS impl_trait, impl_header_line AS header_line,
       list(unit_id ORDER BY unit_id) AS members
FROM units
WHERE impl_key IS NOT NULL
GROUP BY file_id, impl_key, impl_header_line;

-- impl_targets(impl_id, part, name, target_unit_id, resolved_by): each impl's `type` part
-- and, for a trait impl, its `trait` part, resolved by name to a type unit (a trait for
-- the trait part): among the type units whose name is the part's bare name
-- (impl_base_name), the one whose symbol is referenced on the impl's header line
-- (resolved_by 'header'), else the only one in the impl's file ('file'), else the only one
-- in its crate ('crate'), else the only one anywhere ('name'). NULL target: none, or
-- several at the last step (an external trait such as From, a type parameter, a name
-- several crates define).
CREATE OR REPLACE VIEW impl_targets AS
WITH parts AS MATERIALIZED (
    SELECT impl_id, file_id, crate, header_line, 'type' AS part, impl_type AS written FROM impls
    UNION ALL
    SELECT impl_id, file_id, crate, header_line, 'trait', impl_trait FROM impls WHERE impl_trait IS NOT NULL
),
named AS MATERIALIZED (
    SELECT *, impl_base_name(written) AS name FROM parts
),
types AS MATERIALIZED (
    SELECT unit_id, file_id, crate, name, symbol, symbol_kind FROM units WHERE kind = 'type'
),
header_refs AS MATERIALIZED (
    SELECT DISTINCT o.file_id, o.start_line, o.symbol
    FROM scip_occurrences o
    WHERE o.is_reference AND (o.file_id, o.start_line) IN (SELECT (file_id, header_line) FROM impls)
),
candidates AS MATERIALIZED (
    SELECT n.impl_id, n.part, t.unit_id, t.file_id = n.file_id AS same_file, t.crate = n.crate AS same_crate,
           EXISTS (SELECT 1 FROM header_refs h
                   WHERE h.file_id = n.file_id AND h.start_line = n.header_line AND h.symbol = t.symbol) AS on_header
    FROM named n
    JOIN types t ON t.name = n.name AND (n.part = 'type' OR t.symbol_kind = 'Trait')
),
picked AS (
    SELECT impl_id, part,
           CASE WHEN count(*) FILTER (WHERE on_header) = 1 THEN 'header'
                WHEN count(*) FILTER (WHERE on_header) = 0 AND count(*) FILTER (WHERE same_file) = 1 THEN 'file'
                WHEN count(*) FILTER (WHERE on_header) = 0 AND count(*) FILTER (WHERE same_file) = 0
                     AND count(*) FILTER (WHERE same_crate) = 1 THEN 'crate'
                WHEN count(*) FILTER (WHERE on_header) = 0 AND count(*) FILTER (WHERE same_file) = 0
                     AND count(*) FILTER (WHERE same_crate) = 0 AND count(*) = 1 THEN 'name'
           END AS resolved_by,
           any_value(unit_id) FILTER (WHERE on_header) AS on_header_unit,
           any_value(unit_id) FILTER (WHERE same_file) AS same_file_unit,
           any_value(unit_id) FILTER (WHERE same_crate) AS same_crate_unit,
           any_value(unit_id) AS any_unit
    FROM candidates
    GROUP BY impl_id, part
)
SELECT n.impl_id, n.part, n.written AS name,
       CASE p.resolved_by WHEN 'header' THEN p.on_header_unit WHEN 'file' THEN p.same_file_unit
                          WHEN 'crate' THEN p.same_crate_unit WHEN 'name' THEN p.any_unit END AS target_unit_id,
       p.resolved_by
FROM named n
LEFT JOIN picked p USING (impl_id, part);

-- impl_for(impl_id, type_unit_id) and implements(impl_id, trait_unit_id): the
-- impl_targets that resolved to a unit.
CREATE OR REPLACE VIEW impl_for AS
SELECT impl_id, target_unit_id AS type_unit_id FROM impl_targets WHERE part = 'type' AND target_unit_id IS NOT NULL;

CREATE OR REPLACE VIEW implements AS
SELECT impl_id, target_unit_id AS trait_unit_id FROM impl_targets WHERE part = 'trait' AND target_unit_id IS NOT NULL;

-- unit_impl_type(unit_id, impl_id, type_unit_id): for each unit inside an impl (a member,
-- or a unit inside a member's body), the impl of its nearest ancestor-or-self member and
-- the type that impl is for (NULL when unresolved).
CREATE OR REPLACE VIEW unit_impl_type AS
WITH members AS MATERIALIZED (
    SELECT i.impl_id, unnest(i.members) AS unit_id FROM impls i
),
nearest AS (
    SELECT a.unit_id, arg_min(m.impl_id, a.depth) AS impl_id
    FROM unit_ancestors a JOIN members m ON m.unit_id = a.ancestor_id
    GROUP BY a.unit_id
)
SELECT n.unit_id, n.impl_id, f.type_unit_id
FROM nearest n LEFT JOIN impl_for f USING (impl_id);

-- modules(module, file_id, module_unit_id, crate, module_path): every module unit: a file
-- module is named by its file's path, an inline module `<path>::<names>`
-- (src/shapes.rs::tests). module_path places each module in its crate's module tree by
-- Rust's file-module convention, as a `/`-separated path: a crate root (`…/lib.rs`,
-- `…/main.rs`) and a `…/mod.rs` are their directory, any other file its path without
-- `.rs`, an inline module its file's module_path followed by its names (src/shapes.rs::tests
-- is src/shapes/tests). A module is an ancestor of another when its module_path is a prefix
-- of the other's at a `/`. The path convention rather than the descriptors places modules
-- because SCIP names every target of a package's crate root `crate/`: an integration
-- test's root would otherwise be an ancestor of every module of the library. `#[path]`
-- attributes are not read, and a binary under src/bin/ is placed under the library's root.
CREATE OR REPLACE VIEW modules AS
WITH module_units AS MATERIALIZED (
    SELECT u.unit_id, u.file_id, u.crate, u.qualified_name, u.is_file_module, f.path,
           CASE WHEN regexp_matches(f.path, '(^|/)(lib|main|mod)\.rs$')
                THEN regexp_replace(f.path, '/?(lib|main|mod)\.rs$', '')
                ELSE regexp_replace(f.path, '\.rs$', '') END AS file_module_path
    FROM units u JOIN files f USING (file_id)
    WHERE u.kind = 'module'
)
SELECT CASE WHEN is_file_module THEN path ELSE path || '::' || replace(qualified_name, '.', '::') END AS module,
       file_id, unit_id AS module_unit_id, crate,
       CASE WHEN is_file_module THEN file_module_path
            ELSE file_module_path || '/' || replace(qualified_name, '.', '/') END AS module_path
FROM module_units;

-- unit_module(unit_id, module, module_unit_id, file_id, crate, module_path): the module a
-- unit belongs to: the nearest module unit among the unit itself and its enclosing units
-- (a module unit belongs to the module it is).
CREATE OR REPLACE VIEW unit_module AS
WITH module_rows AS MATERIALIZED (
    SELECT * FROM modules
),
nearest AS (
    SELECT a.unit_id, arg_min(a.ancestor_id, a.depth) AS module_unit_id
    FROM unit_ancestors a JOIN module_rows m ON m.module_unit_id = a.ancestor_id
    GROUP BY a.unit_id
)
SELECT n.unit_id, m.module, n.module_unit_id, m.file_id, m.crate, m.module_path
FROM nearest n JOIN module_rows m USING (module_unit_id);

-- module_edges(src_module, dst_module, src_crate, dst_crate, unit_edges, occurrences,
--     test_unit_edges, ambiguous_unit_edges, nested_modules, same_type_unit_edges) and
-- crate_edges(src_crate, dst_crate, unit_edges, occurrences, test_unit_edges,
--     ambiguous_unit_edges): unit_edges between different modules (crates), self edges
-- excluded. unit_edges counts the unit edges, occurrences their occurrences;
-- test_unit_edges counts the edges with a test unit at either end, ambiguous_unit_edges
-- those that are ambiguous. nested_modules marks an edge between a module and one of its
-- ancestors or descendants (modules.module_path; in one crate), same_type_unit_edges
-- counts the unit edges whose two ends both lie in impls of one type (unit_impl_type):
-- one type's impl blocks split over several files. module_cycle_edges leaves out the
-- edges that are nested_modules or whose every unit edge is same-type.
CREATE OR REPLACE VIEW module_edges AS
WITH m AS MATERIALIZED (
    SELECT unit_id, module, crate, module_path FROM unit_module
),
impl_type AS MATERIALIZED (
    SELECT unit_id, type_unit_id FROM unit_impl_type WHERE type_unit_id IS NOT NULL
)
SELECT ms.module AS src_module, md.module AS dst_module, ms.crate AS src_crate, md.crate AS dst_crate,
       count(*) AS unit_edges, sum(e.occurrences) AS occurrences,
       count(*) FILTER (WHERE e.src_is_test OR e.dst_is_test) AS test_unit_edges,
       count(*) FILTER (WHERE e.ambiguous) AS ambiguous_unit_edges,
       ms.crate = md.crate AND (starts_with(md.module_path || '/', ms.module_path || '/')
                                OR starts_with(ms.module_path || '/', md.module_path || '/')) AS nested_modules,
       count(*) FILTER (WHERE ts.type_unit_id = td.type_unit_id) AS same_type_unit_edges
FROM unit_edges e
JOIN m ms ON ms.unit_id = e.src_unit_id
JOIN m md ON md.unit_id = e.dst_unit_id
LEFT JOIN impl_type ts ON ts.unit_id = e.src_unit_id
LEFT JOIN impl_type td ON td.unit_id = e.dst_unit_id
WHERE NOT e.is_self AND ms.module <> md.module
GROUP BY ms.module, md.module, ms.crate, md.crate, ms.module_path, md.module_path;

CREATE OR REPLACE VIEW crate_edges AS
WITH c AS MATERIALIZED (
    SELECT unit_id, crate FROM units
)
SELECT cs.crate AS src_crate, cd.crate AS dst_crate,
       count(*) AS unit_edges, sum(e.occurrences) AS occurrences,
       count(*) FILTER (WHERE e.src_is_test OR e.dst_is_test) AS test_unit_edges,
       count(*) FILTER (WHERE e.ambiguous) AS ambiguous_unit_edges
FROM unit_edges e
JOIN c cs ON cs.unit_id = e.src_unit_id
JOIN c cd ON cd.unit_id = e.dst_unit_id
WHERE NOT e.is_self AND cs.crate <> cd.crate
GROUP BY ALL;

-- module_cycle_edges(cycle, src_module, dst_module, unit_edges, occurrences,
--     test_unit_edges, ambiguous_unit_edges): the module edges inside each strongly
-- connected group of modules (two modules are in one group when each reaches the other
-- along the edges). cycle names the group by its least member.
-- module_cycles(cycle, crate, modules, members, edges, unit_edges, occurrences): one
-- row per group, with its members and the counts of its inner edges. Reachability is
-- the transitive closure of the edges, by a recursive common table expression that
-- adds only new pairs (UNION).
-- The edges are module_edges without the structural ones: those between a module and its
-- ancestor or descendant (nested_modules) and those whose every unit edge joins impls of
-- one type (same_type_unit_edges = unit_edges). Parameter (DuckDB variable; default
-- when unset): cycle_edges 'collapsed' ('raw': every module edge).
CREATE OR REPLACE VIEW module_cycle_edges AS
WITH RECURSIVE edges AS MATERIALIZED (
    SELECT src_module, dst_module, unit_edges, occurrences, test_unit_edges, ambiguous_unit_edges
    FROM module_edges
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

CREATE OR REPLACE VIEW module_cycles AS
WITH crates AS MATERIALIZED (
    SELECT module, crate FROM modules
)
SELECT e.cycle, any_value(c.crate) AS crate, count(DISTINCT e.src_module) AS modules,
       list(DISTINCT e.src_module ORDER BY e.src_module) AS members, count(*) AS edges,
       sum(e.unit_edges) AS unit_edges, sum(e.occurrences) AS occurrences
FROM module_cycle_edges e
JOIN crates c ON c.module = e.src_module
GROUP BY e.cycle;

-- The intended layers, from the analysed repository's settings file (config_layers and
-- config_layer_deps, 00_load.sql; empty without one). module_layer_matches(module, layer):
-- each module (modules.module) with each layer one of whose module patterns it matches
-- (GLOB operator, `*` crosses `/`). module_layers(module, layer): one layer per module; a
-- module that matches two layers is an error (code-db reports it before any query). A
-- module in no layer has no row.
CREATE OR REPLACE VIEW module_layer_matches AS
SELECT DISTINCT m.module, l.name AS layer
FROM modules m JOIN config_layers l ON m.module GLOB l.module_pattern;

CREATE OR REPLACE VIEW module_layers AS
SELECT module,
       CASE WHEN count(*) > 1
            THEN error('module_layers: ' || module || ' matches the layers '
                       || string_agg(layer, ', ' ORDER BY layer))
            ELSE any_value(layer) END AS layer
FROM module_layer_matches
GROUP BY module;

-- graph_layer_violation(src_layer, dst_layer): whether an edge from a module in src_layer
-- to a module in dst_layer goes against the layers: both modules are in a layer, the
-- layers differ, and src_layer's may_depend_on (config_layer_deps) does not name
-- dst_layer. A module in no layer (NULL) is unconstrained either way; may_depend_on is
-- not transitive.
CREATE OR REPLACE MACRO graph_layer_violation(src_layer, dst_layer) AS
    src_layer IS NOT NULL AND dst_layer IS NOT NULL AND src_layer <> dst_layer
    AND NOT EXISTS (SELECT 1 FROM config_layer_deps d WHERE d.name = src_layer AND d.may_depend_on = dst_layer);

-- module_layer_violations(src_module, dst_module, src_layer, dst_layer): the module edges
-- (module_edges, every one: nested and same-type edges included) that go against the
-- layers.
CREATE OR REPLACE VIEW module_layer_violations AS
SELECT e.src_module, e.dst_module, ls.layer AS src_layer, ld.layer AS dst_layer
FROM module_edges e
JOIN module_layers ls ON ls.module = e.src_module
JOIN module_layers ld ON ld.module = e.dst_module
WHERE graph_layer_violation(ls.layer, ld.layer);

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
