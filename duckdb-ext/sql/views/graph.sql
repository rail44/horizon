-- The code graph: which unit references which, how units nest, which type a member belongs
-- to and which interfaces a type implements (type facts), the namespace and package edges folded from the unit edges,
-- the namespace cycles they form, and the intended layers. Every view is derived from
-- units (views/units.sql), scip_occurrences (views/scip.sql), source_lines
-- (views/files.sql) and the settings' tables (00_load.sql).
--
-- Locating an occurrence. An occurrence belongs to the innermost unit whose range (SCIP's
-- enclosing_range, line and column) contains its start: the containing unit with the
-- greatest unit_id. Every indexed file has its file unit as a unit, so an occurrence
-- outside every item (a `use` declaration, a `mod x;` declaration, a file-level macro
-- call) belongs to the file unit. A symbol's definition belongs to the unit it defines
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
-- belong to (above), except that a namespace's own symbol has none, nor a file unit's (a Go
-- file's package symbol, which every file of the package defines): a reference to a module
-- names a namespace (a path segment `crate::a::b::f`, a `use crate::a;`, a `mod a;`
-- declaration), not code the referrer depends on, and would otherwise draw an edge to
-- every namespace a path passes through. A symbol with definitions in several units (rust-analyzer names a
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
      AND coalesce(w.kind, '') NOT IN ('namespace', 'file')
)
SELECT symbol, unit_id, count(*) OVER (PARTITION BY symbol) > 1 AS ambiguous FROM defs;

-- unit_references(src_unit_id, dst_unit_id, symbol, occurrences, ambiguous, src_is_test,
--     dst_is_test): for each non-local symbol, the units holding a reference occurrence
-- of it (source) and the units its definitions belong to (destination), one row per
-- (source, destination, symbol); occurrences counts the source's reference occurrences
-- of the symbol. Symbols defined in no source file (external packages, the standard
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

-- type_facts(fact, file_id, symbol, name_line, name_column, to_file_id, to_symbol,
--     to_name_line, to_name_column, to_name): what each language says about types
-- (lang/<language>.sql, <language>_type_facts), each end a definition occurrence: 'member'
-- (a member and its type), 'implements' (a type and an interface it implements),
-- 'implements_member' (a member and the interface's member it implements), 'converts' (a
-- type and the name of a type it converts from or into). The views below put units at
-- both ends; an end no unit has (unresolved, external) is NULL.
CREATE OR REPLACE VIEW type_facts AS
SELECT * FROM rust_type_facts
UNION ALL
SELECT * FROM go_type_facts;

-- type_fact_units(fact, unit_id, to_unit_id, to_name): type_facts with units at both ends.
CREATE OR REPLACE VIEW type_fact_units AS
WITH keys AS MATERIALIZED (
    SELECT unit_id, file_id, symbol, name_line, name_column FROM units
)
SELECT f.fact, u.unit_id, t.unit_id AS to_unit_id, f.to_name
FROM type_facts f
JOIN keys u ON u.file_id = f.file_id AND u.symbol = f.symbol
     AND u.name_line = f.name_line AND u.name_column = f.name_column
LEFT JOIN keys t ON t.file_id = f.to_file_id AND t.symbol = f.to_symbol
     AND t.name_line = f.to_name_line AND t.name_column = f.to_name_column;

-- unit_owner_types(unit_id, type_unit_id): for each unit that is a member of a type or
-- lies inside one (a unit in a member's body), the type of its nearest ancestor-or-self
-- member; none when that member's type is unresolved.
CREATE OR REPLACE VIEW unit_owner_types AS
WITH members AS MATERIALIZED (
    SELECT unit_id AS member_unit_id, to_unit_id AS type_unit_id FROM type_fact_units WHERE fact = 'member'
),
nearest AS (
    SELECT a.unit_id, arg_min(m.member_unit_id, a.depth) AS member_unit_id
    FROM unit_ancestors a JOIN members m ON m.member_unit_id = a.ancestor_id
    GROUP BY a.unit_id
)
SELECT n.unit_id, m.type_unit_id
FROM nearest n JOIN members m USING (member_unit_id)
WHERE m.type_unit_id IS NOT NULL;

-- type_interfaces(type_unit_id, interface_unit_id): the interfaces each type implements.
CREATE OR REPLACE VIEW type_interfaces AS
SELECT DISTINCT unit_id AS type_unit_id, to_unit_id AS interface_unit_id
FROM type_fact_units WHERE fact = 'implements' AND to_unit_id IS NOT NULL;

-- member_implementations(unit_id, interface_member_unit_id): each member implementing an
-- interface's member, with that member (NULL when it has no unit: an external trait's).
CREATE OR REPLACE VIEW member_implementations AS
SELECT DISTINCT unit_id, to_unit_id AS interface_member_unit_id
FROM type_fact_units WHERE fact = 'implements_member';

-- type_conversions(type_unit_id, to_name): the types each type converts from or into, by
-- name (queries/duplication.sql, conversion_pair).
CREATE OR REPLACE VIEW type_conversions AS
SELECT DISTINCT unit_id AS type_unit_id, to_name
FROM type_fact_units WHERE fact = 'converts';

-- file_namespaces(file_id, namespace, namespace_path): the namespace each source file
-- belongs to, as its language places it (lang/<language>.sql, <language>_file_namespaces):
-- a Rust file is its own namespace (its file module), a Go file belongs to its directory's
-- package. namespace_path places the namespace among the others as a `/`-separated path.
CREATE OR REPLACE VIEW file_namespaces AS
SELECT * FROM rust_file_namespaces
UNION ALL
SELECT * FROM go_file_namespaces;

-- namespaces(namespace, file_id, namespace_unit_id, package, namespace_path): every
-- namespace: each namespace of file_namespaces that holds a file with a file unit (an
-- indexed file), and each namespace unit inside a file (an inline module `mod x { … }`,
-- named `<file's namespace>::<names>`, src/shapes.rs::tests, and placed below its file's
-- namespace_path, src/shapes/tests). file_id and namespace_unit_id are the file and its
-- file unit when the namespace is one file (every Rust file module, a one-file Go
-- package), NULL when it spans several (a Go package). A namespace is an ancestor of
-- another when its namespace_path is a prefix of the other's at a `/`.
CREATE OR REPLACE VIEW namespaces AS
WITH file_units AS MATERIALIZED (
    SELECT u.unit_id, u.file_id, u.package, n.namespace, n.namespace_path
    FROM units u JOIN file_namespaces n USING (file_id)
    WHERE u.is_file_unit
),
file_level AS (
    SELECT namespace,
           CASE WHEN count(*) = 1 THEN any_value(file_id) END AS file_id,
           CASE WHEN count(*) = 1 THEN any_value(unit_id) END AS namespace_unit_id,
           any_value(package) AS package, any_value(namespace_path) AS namespace_path
    FROM file_units
    GROUP BY namespace
)
SELECT * FROM file_level
UNION ALL
SELECT f.namespace || '::' || replace(u.qualified_name, '.', '::'), u.file_id, u.unit_id, u.package,
       f.namespace_path || '/' || replace(u.qualified_name, '.', '/')
FROM units u JOIN file_units f USING (file_id)
WHERE u.kind = 'namespace' AND NOT u.is_file_unit;

-- unit_namespace(unit_id, namespace, namespace_unit_id, file_id, package, namespace_path): the namespace a
-- unit belongs to: the nearest namespace with a unit among the unit itself and its
-- enclosing units (a namespace unit belongs to the namespace it is), else its file's
-- namespace (a Go package's units); the namespace's columns follow.
CREATE OR REPLACE VIEW unit_namespace AS
WITH namespace_rows AS MATERIALIZED (
    SELECT * FROM namespaces
),
nearest AS (
    SELECT a.unit_id, arg_min(a.ancestor_id, a.depth) AS namespace_unit_id
    FROM unit_ancestors a JOIN namespace_rows m ON m.namespace_unit_id = a.ancestor_id
    GROUP BY a.unit_id
),
by_file AS (
    SELECT u.unit_id, n.namespace
    FROM units u JOIN file_namespaces n USING (file_id)
    WHERE u.unit_id NOT IN (SELECT unit_id FROM nearest)
)
SELECT n.unit_id, m.namespace, m.namespace_unit_id, m.file_id, m.package, m.namespace_path
FROM nearest n JOIN namespace_rows m USING (namespace_unit_id)
UNION ALL
SELECT b.unit_id, m.namespace, m.namespace_unit_id, m.file_id, m.package, m.namespace_path
FROM by_file b JOIN namespace_rows m USING (namespace);

-- namespace_edges(src_namespace, dst_namespace, src_package, dst_package, unit_edges, occurrences,
--     test_unit_edges, ambiguous_unit_edges, nested_namespaces, same_type_unit_edges) and
-- package_edges(src_package, dst_package, unit_edges, occurrences, test_unit_edges,
--     ambiguous_unit_edges): unit_edges between different namespaces (packages), self edges
-- excluded. unit_edges counts the unit edges, occurrences their occurrences;
-- test_unit_edges counts the edges with a test unit at either end, ambiguous_unit_edges
-- those that are ambiguous. nested_namespaces marks an edge between a namespace and one of its
-- ancestors or descendants (namespaces.namespace_path; in one package), same_type_unit_edges
-- counts the unit edges whose two ends both lie in members of one type (unit_owner_types):
-- one type's impl blocks split over several files. module_cycle_edges leaves out the
-- edges that are nested_namespaces or whose every unit edge is same-type.
CREATE OR REPLACE VIEW namespace_edges AS
WITH m AS MATERIALIZED (
    SELECT unit_id, namespace, package, namespace_path FROM unit_namespace
),
owner_type AS MATERIALIZED (
    SELECT unit_id, type_unit_id FROM unit_owner_types
)
SELECT ms.namespace AS src_namespace, md.namespace AS dst_namespace, ms.package AS src_package, md.package AS dst_package,
       count(*) AS unit_edges, sum(e.occurrences) AS occurrences,
       count(*) FILTER (WHERE e.src_is_test OR e.dst_is_test) AS test_unit_edges,
       count(*) FILTER (WHERE e.ambiguous) AS ambiguous_unit_edges,
       ms.package = md.package AND (starts_with(md.namespace_path || '/', ms.namespace_path || '/')
                                OR starts_with(ms.namespace_path || '/', md.namespace_path || '/')) AS nested_namespaces,
       count(*) FILTER (WHERE ts.type_unit_id = td.type_unit_id) AS same_type_unit_edges
FROM unit_edges e
JOIN m ms ON ms.unit_id = e.src_unit_id
JOIN m md ON md.unit_id = e.dst_unit_id
LEFT JOIN owner_type ts ON ts.unit_id = e.src_unit_id
LEFT JOIN owner_type td ON td.unit_id = e.dst_unit_id
WHERE NOT e.is_self AND ms.namespace <> md.namespace
GROUP BY ms.namespace, md.namespace, ms.package, md.package, ms.namespace_path, md.namespace_path;

CREATE OR REPLACE VIEW package_edges AS
WITH c AS MATERIALIZED (
    SELECT unit_id, package FROM units
)
SELECT cs.package AS src_package, cd.package AS dst_package,
       count(*) AS unit_edges, sum(e.occurrences) AS occurrences,
       count(*) FILTER (WHERE e.src_is_test OR e.dst_is_test) AS test_unit_edges,
       count(*) FILTER (WHERE e.ambiguous) AS ambiguous_unit_edges
FROM unit_edges e
JOIN c cs ON cs.unit_id = e.src_unit_id
JOIN c cd ON cd.unit_id = e.dst_unit_id
WHERE NOT e.is_self AND cs.package <> cd.package
GROUP BY ALL;

-- namespace_cycle_edges(cycle, src_namespace, dst_namespace, unit_edges, occurrences,
--     test_unit_edges, ambiguous_unit_edges): the namespace edges inside each strongly
-- connected group of namespaces (two namespaces are in one group when each reaches the other
-- along the edges). cycle names the group by its least member.
-- namespace_cycles(cycle, package, namespaces, members, edges, unit_edges, occurrences): one
-- row per group, with its members and the counts of its inner edges. Reachability is
-- the transitive closure of the edges, by a recursive common table expression that
-- adds only new pairs (UNION).
-- The edges are namespace_edges without the structural ones: those between a namespace and its
-- ancestor or descendant (nested_namespaces) and those whose every unit edge joins members of
-- one type (same_type_unit_edges = unit_edges). Parameter (DuckDB variable; default
-- when unset): cycle_edges 'collapsed' ('raw': every namespace edge).
CREATE OR REPLACE VIEW namespace_cycle_edges AS
WITH RECURSIVE edges AS MATERIALIZED (
    SELECT src_namespace, dst_namespace, unit_edges, occurrences, test_unit_edges, ambiguous_unit_edges
    FROM namespace_edges
    WHERE CASE coalesce(getvariable('cycle_edges'), 'collapsed')
        WHEN 'raw' THEN true
        WHEN 'collapsed' THEN NOT nested_namespaces AND same_type_unit_edges < unit_edges
        ELSE error('namespace_cycle_edges: cycle_edges is collapsed or raw') END
),
reach(src, dst) AS (
    SELECT src_namespace, dst_namespace FROM edges
    UNION
    SELECT r.src, e.dst_namespace FROM reach r JOIN edges e ON e.src_namespace = r.dst
),
reach_rows AS MATERIALIZED (
    SELECT * FROM reach
),
groups AS MATERIALIZED (
    SELECT a.src AS namespace, min(a.dst) AS cycle
    FROM reach_rows a JOIN reach_rows b ON b.src = a.dst AND b.dst = a.src
    GROUP BY a.src
)
SELECT gs.cycle, e.*
FROM edges e
JOIN groups gs ON gs.namespace = e.src_namespace
JOIN groups gd ON gd.namespace = e.dst_namespace AND gd.cycle = gs.cycle;

CREATE OR REPLACE VIEW namespace_cycles AS
WITH packages AS MATERIALIZED (
    SELECT namespace, package FROM namespaces
)
SELECT e.cycle, any_value(c.package) AS package, count(DISTINCT e.src_namespace) AS namespaces,
       list(DISTINCT e.src_namespace ORDER BY e.src_namespace) AS members, count(*) AS edges,
       sum(e.unit_edges) AS unit_edges, sum(e.occurrences) AS occurrences
FROM namespace_cycle_edges e
JOIN packages c ON c.namespace = e.src_namespace
GROUP BY e.cycle;

-- The intended layers, from the analysed repository's settings file (config_layers and
-- config_layer_deps, 00_load.sql; empty without one). namespace_layer_matches(namespace, layer):
-- each namespace (namespaces.namespace) with each layer one of whose namespace patterns it matches
-- (GLOB operator, `*` crosses `/`). namespace_layers(namespace, layer): one layer per namespace; a
-- namespace that matches two layers is an error (code-db reports it before any query). A
-- namespace in no layer has no row.
CREATE OR REPLACE VIEW namespace_layer_matches AS
SELECT DISTINCT m.namespace, l.name AS layer
FROM namespaces m JOIN config_layers l ON m.namespace GLOB l.namespace_pattern;

CREATE OR REPLACE VIEW namespace_layers AS
SELECT namespace,
       CASE WHEN count(*) > 1
            THEN error('namespace_layers: ' || namespace || ' matches the layers '
                       || string_agg(layer, ', ' ORDER BY layer))
            ELSE any_value(layer) END AS layer
FROM namespace_layer_matches
GROUP BY namespace;

-- graph_layer_violation(src_layer, dst_layer): whether an edge from a namespace in src_layer
-- to a namespace in dst_layer goes against the layers: both namespaces are in a layer, the
-- layers differ, and src_layer's may_depend_on (config_layer_deps) does not name
-- dst_layer. A namespace in no layer (NULL) is unconstrained either way; may_depend_on is
-- not transitive.
CREATE OR REPLACE MACRO graph_layer_violation(src_layer, dst_layer) AS
    src_layer IS NOT NULL AND dst_layer IS NOT NULL AND src_layer <> dst_layer
    AND NOT EXISTS (SELECT 1 FROM config_layer_deps d WHERE d.name = src_layer AND d.may_depend_on = dst_layer);

-- namespace_layer_violations(src_namespace, dst_namespace, src_layer, dst_layer): the namespace edges
-- (namespace_edges, every one: nested and same-type edges included) that go against the
-- layers.
CREATE OR REPLACE VIEW namespace_layer_violations AS
SELECT e.src_namespace, e.dst_namespace, ls.layer AS src_layer, ld.layer AS dst_layer
FROM namespace_edges e
JOIN namespace_layers ls ON ls.namespace = e.src_namespace
JOIN namespace_layers ld ON ld.namespace = e.dst_namespace
WHERE graph_layer_violation(ls.layer, ld.layer);

-- package_cycles(cycle, packages, members, edges, unit_edges, occurrences): the same over
-- package_edges. Cargo forbids dependency cycles between packages, so a row here means
-- either an aggregation error or a cycle Cargo allows (a dev-dependency used by test
-- code).
CREATE OR REPLACE VIEW package_cycles AS
WITH RECURSIVE edges AS MATERIALIZED (
    SELECT src_package, dst_package, unit_edges, occurrences FROM package_edges
),
reach(src, dst) AS (
    SELECT src_package, dst_package FROM edges
    UNION
    SELECT r.src, e.dst_package FROM reach r JOIN edges e ON e.src_package = r.dst
),
reach_rows AS MATERIALIZED (
    SELECT * FROM reach
),
groups AS MATERIALIZED (
    SELECT a.src AS package, min(a.dst) AS cycle
    FROM reach_rows a JOIN reach_rows b ON b.src = a.dst AND b.dst = a.src
    GROUP BY a.src
)
SELECT gs.cycle, count(DISTINCT e.src_package) AS packages, list(DISTINCT e.src_package ORDER BY e.src_package) AS members,
       count(*) AS edges, sum(e.unit_edges) AS unit_edges, sum(e.occurrences) AS occurrences
FROM edges e
JOIN groups gs ON gs.package = e.src_package
JOIN groups gd ON gd.package = e.dst_package AND gd.cycle = gs.cycle
GROUP BY gs.cycle;
