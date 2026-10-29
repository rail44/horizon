-- Question: which modules depend on which, and how strongly?
-- Rows: module_edges (views/graph.sql): unit edges between different modules, a module
-- being a file or a `mod` block inside one (`<path>::<name>`), self edges excluded.
-- unit_edges counts the unit edges, occurrences their reference occurrences;
-- test_unit_edges and ambiguous_unit_edges count those with a test unit at either end
-- and those that exist only through a symbol defined in several units;
-- nested_modules marks an edge between a module and its ancestor or descendant.
-- reverse_unit_edges is the edge in the other direction (0 when there is none).
-- Axis: coupling at module level (docs/research/code-quality-axes-2026-10-29.md §3.6, §3.8).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). Needs the
-- SCIP indexes.
-- Evidence: medium for coupling in general (SIG's module coupling, see coupling.sql),
-- none for module coupling on Rust; architecture anti-patterns built on the dependency
-- graph (Mo et al., TSE 2021, 19 projects, Java/C/C++ files) are what this graph feeds.
-- Parameters (DuckDB variables; default when unset):
--   crate NULL (all edges; or a crate name: the edges with both ends in that crate,
--   SCIP's package names).
WITH params AS (
    SELECT CAST(getvariable('crate') AS VARCHAR) AS crate
),
edges AS MATERIALIZED (
    SELECT * FROM module_edges
)
SELECT e.src_module, e.dst_module, e.src_crate, e.dst_crate, e.unit_edges, e.occurrences,
       e.test_unit_edges, e.ambiguous_unit_edges, e.nested_modules,
       coalesce(r.unit_edges, 0) AS reverse_unit_edges
FROM edges e
CROSS JOIN params p
LEFT JOIN edges r ON r.src_module = e.dst_module AND r.dst_module = e.src_module
WHERE p.crate IS NULL OR (e.src_crate = p.crate AND e.dst_crate = p.crate)
ORDER BY e.unit_edges DESC, e.occurrences DESC, e.src_module, e.dst_module;
