-- Question: which modules depend on each other in a cycle, and through which edges?
-- Rows: one per module edge inside a strongly connected group of modules
-- (module_cycle_edges, views/graph.sql: each member reaches every other along
-- module_edges); the group is named by its least member (`cycle`), with its crate and
-- size. reverse_unit_edges is the edge in the other direction (0 when the cycle closes
-- through other members). symbols lists up to five display names of the edge's symbols.
-- Within a group, the edges with the fewest unit edges come first. Needs views/graph.sql.
-- Evidence: Package Cycle is one of the six anti-patterns of Mo et al. (TSE 2021, 19
-- projects, Java/C/C++), not one of the two with the largest contribution; Cargo
-- forbids crate cycles, and no Rust study was found that shows module cycles to be
-- harmful (see the survey).
-- Parameters (DuckDB variables; default when unset):
--   crate NULL (all cycles; or a crate name).
WITH params AS (
    SELECT CAST(getvariable('crate') AS VARCHAR) AS crate
),
cycles AS MATERIALIZED (
    SELECT cycle, crate, modules FROM module_cycles
),
edges AS MATERIALIZED (
    SELECT * FROM module_cycle_edges
),
names AS MATERIALIZED (
    SELECT symbol, any_value(display_name) AS display_name FROM scip_symbols GROUP BY symbol
),
unit_mods AS MATERIALIZED (
    SELECT unit_id, module FROM unit_module
),
edge_symbols AS (
    SELECT ms.module AS src_module, md.module AS dst_module,
           list(DISTINCT coalesce(nullif(n.display_name, ''), r.symbol)
                ORDER BY coalesce(nullif(n.display_name, ''), r.symbol)) AS symbols
    FROM unit_references r
    JOIN unit_mods ms ON ms.unit_id = r.src_unit_id
    JOIN unit_mods md ON md.unit_id = r.dst_unit_id
    JOIN edges e ON e.src_module = ms.module AND e.dst_module = md.module
    LEFT JOIN names n ON n.symbol = r.symbol
    GROUP BY ALL
)
SELECT c.cycle, c.crate, c.modules, e.src_module, e.dst_module, e.unit_edges, e.occurrences,
       e.test_unit_edges, e.macro_unit_edges, e.ambiguous_unit_edges,
       coalesce(r.unit_edges, 0) AS reverse_unit_edges, s.symbols[1:5] AS symbols
FROM edges e
JOIN cycles c USING (cycle)
CROSS JOIN params p
LEFT JOIN edges r ON r.cycle = e.cycle AND r.src_module = e.dst_module AND r.dst_module = e.src_module
LEFT JOIN edge_symbols s ON s.src_module = e.src_module AND s.dst_module = e.dst_module
WHERE p.crate IS NULL OR c.crate = p.crate
ORDER BY c.modules DESC, c.cycle, e.unit_edges, e.occurrences, e.src_module, e.dst_module;
