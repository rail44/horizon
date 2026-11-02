-- Question: which namespaces depend on each other in a cycle, and through which edges?
-- Rows: one per namespace edge inside a strongly connected group of namespaces
-- (namespace_cycle_edges, views/graph.sql: each member reaches every other along
-- namespace_edges); the group is named by its least member (`cycle`), with its package and
-- size. reverse_unit_edges is the edge in the other direction (0 when the cycle closes
-- through other members). symbols lists up to five display names of the edge's symbols.
-- Within a group, the edges with the fewest unit edges come first. violating_edges counts
-- the group's edges that go against the declared layers (graph_layer_violation,
-- views/graph.sql; README.md, Settings file), the same on every row of the group; 0
-- without layers. Structural edges (a namespace and its ancestor or descendant, and edges
-- whose every unit edge joins members of one type) are left out unless cycle_edges = 'raw'
-- (views/graph.sql, namespace_cycle_edges).
-- Axis: independence (docs/research/code-quality-axes-2026-10-29.md §3.8).
-- Judgment from the settings file: the intended layers ([[layers]], may_depend_on) behind
-- violating_edges; scope and tests (scope.exclude, tests.*). Needs the SCIP indexes.
-- Evidence: weak. SIG's component independence is not significant (0.16, p = 0.201);
-- Package Cycle is one of the six anti-patterns of Mo et al. (TSE 2021, 19
-- projects, Java/C/C++), not one of the two with the largest contribution; Cargo
-- forbids package cycles, and no Rust study was found that shows namespace cycles to be
-- harmful (see the survey).
-- Parameters (DuckDB variables; default when unset):
--   package NULL (all cycles; or a package name); cycle_edges 'collapsed' ('raw').
WITH params AS (
    SELECT CAST(getvariable('package') AS VARCHAR) AS package
),
cycles AS MATERIALIZED (
    SELECT cycle, package, namespaces FROM namespace_cycles
),
edges AS MATERIALIZED (
    SELECT * FROM namespace_cycle_edges
),
names AS MATERIALIZED (
    SELECT symbol, any_value(display_name) AS display_name FROM scip_symbols GROUP BY symbol
),
unit_mods AS MATERIALIZED (
    SELECT unit_id, namespace FROM unit_namespace
),
layers AS MATERIALIZED (
    SELECT namespace, layer FROM namespace_layers
),
violating AS (
    SELECT e.cycle, count(*) FILTER (WHERE graph_layer_violation(ls.layer, ld.layer)) AS violating_edges
    FROM edges e
    LEFT JOIN layers ls ON ls.namespace = e.src_namespace
    LEFT JOIN layers ld ON ld.namespace = e.dst_namespace
    GROUP BY e.cycle
),
edge_symbols AS (
    SELECT ms.namespace AS src_namespace, md.namespace AS dst_namespace,
           list(DISTINCT coalesce(nullif(n.display_name, ''), r.symbol)
                ORDER BY coalesce(nullif(n.display_name, ''), r.symbol)) AS symbols
    FROM unit_references r
    JOIN unit_mods ms ON ms.unit_id = r.src_unit_id
    JOIN unit_mods md ON md.unit_id = r.dst_unit_id
    JOIN edges e ON e.src_namespace = ms.namespace AND e.dst_namespace = md.namespace
    LEFT JOIN names n ON n.symbol = r.symbol
    GROUP BY ALL
)
SELECT c.cycle, c.package, c.namespaces, e.src_namespace, e.dst_namespace, e.unit_edges, e.occurrences,
       e.test_unit_edges, e.ambiguous_unit_edges,
       coalesce(r.unit_edges, 0) AS reverse_unit_edges, s.symbols[1:5] AS symbols, v.violating_edges
FROM edges e
JOIN cycles c USING (cycle)
JOIN violating v USING (cycle)
CROSS JOIN params p
LEFT JOIN edges r ON r.cycle = e.cycle AND r.src_namespace = e.dst_namespace AND r.dst_namespace = e.src_namespace
LEFT JOIN edge_symbols s ON s.src_namespace = e.src_namespace AND s.dst_namespace = e.dst_namespace
WHERE p.package IS NULL OR c.package = p.package
ORDER BY c.namespaces DESC, c.cycle, e.unit_edges, e.occurrences, e.src_namespace, e.dst_namespace;
