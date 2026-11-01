-- Question: which namespaces depend on which, and how strongly?
-- Rows: namespace_edges (views/graph.sql): unit edges between different namespaces, a namespace
-- being a file or a `mod` block inside one (`<path>::<name>`), self edges excluded.
-- unit_edges counts the unit edges, occurrences their reference occurrences;
-- test_unit_edges and ambiguous_unit_edges count those with a test unit at either end
-- and those that exist only through a symbol defined in several units;
-- nested_namespaces marks an edge between a namespace and its ancestor or descendant.
-- reverse_unit_edges is the edge in the other direction (0 when there is none).
-- Axis: coupling at namespace level (docs/research/code-quality-axes-2026-10-29.md §3.6, §3.8).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). Needs the
-- SCIP indexes.
-- Evidence: medium for coupling in general (SIG's module coupling, see coupling.sql),
-- none for namespace coupling on Rust; architecture anti-patterns built on the dependency
-- graph (Mo et al., TSE 2021, 19 projects, Java/C/C++ files) are what this graph feeds.
-- Parameters (DuckDB variables; default when unset):
--   package NULL (all edges; or a package name: the edges with both ends in that package,
--   SCIP's package names).
WITH params AS (
    SELECT CAST(getvariable('package') AS VARCHAR) AS package
),
edges AS MATERIALIZED (
    SELECT * FROM namespace_edges
)
SELECT e.src_namespace, e.dst_namespace, e.src_package, e.dst_package, e.unit_edges, e.occurrences,
       e.test_unit_edges, e.ambiguous_unit_edges, e.nested_namespaces,
       coalesce(r.unit_edges, 0) AS reverse_unit_edges
FROM edges e
CROSS JOIN params p
LEFT JOIN edges r ON r.src_namespace = e.dst_namespace AND r.dst_namespace = e.src_namespace
WHERE p.package IS NULL OR (e.src_package = p.package AND e.dst_package = p.package)
ORDER BY e.unit_edges DESC, e.occurrences DESC, e.src_namespace, e.dst_namespace;
