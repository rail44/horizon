-- Question: which module dependencies did compare_before..HEAD add and remove?
-- Rows: one per module edge (module_edges, views/graph.sql: unit edges between two
-- modules, self edges excluded) on either side: the after side is HEAD's module_edges,
-- the before side snapshot_module_edges, the same derivation over compare_before's tree
-- (views/snapshot.sql) and the SCIP index of that tree (scip_before). Before modules are
-- named as at HEAD through the file renames of the range (module_renames: an inline
-- module renamed in place is not followed); two before edges that land on one pair are
-- summed.
--   status  added (only after), removed (only before), kept (both)
-- unit_edges_before/unit_edges_after: the unit edges of the module edge on each side
-- (NULL where absent). The before side has no tags (blobs have none), so its test and
-- macro counts are not compared. snapshot_missing: the number of before files whose blob
-- is not in the blob layer (their units, and so their edges, are absent; on every row).
-- Ordering: status in the order above, then |unit_edges_after - unit_edges_before|
-- descending, then the modules.
-- Needs views/graph.sql, scip_before, and the persistent cache.
-- Parameters (DuckDB variables; default when unset):
--   compare_before (required), scip_before (required: the path of a SCIP index of
--   compare_before's tree; README.md, "Before/after comparison"); crate NULL (all
--   crates; or a crate name, matching either end).
WITH before AS MATERIALIZED (
    SELECT CASE WHEN getvariable('scip_before') IS NULL
                THEN error('compare_graph needs scip_before, the path of a SCIP index of compare_before''s tree')
                ELSE ord END AS ord
    FROM compare_commit
),
renamed AS MATERIALIZED (
    SELECT module_before, module FROM module_renames
),
before_edges AS MATERIALIZED (
    SELECT coalesce(rs.module, e.src_module) AS src_module, coalesce(rd.module, e.dst_module) AS dst_module,
           any_value(e.src_crate) AS src_crate, any_value(e.dst_crate) AS dst_crate, sum(e.unit_edges) AS unit_edges
    FROM snapshot_module_edges e
    LEFT JOIN renamed rs ON rs.module_before = e.src_module
    LEFT JOIN renamed rd ON rd.module_before = e.dst_module
    GROUP BY ALL
),
after_edges AS MATERIALIZED (
    SELECT src_module, dst_module, src_crate, dst_crate, unit_edges FROM module_edges
),
missing AS MATERIALIZED (
    SELECT count(*) AS n FROM snapshot_missing
)
SELECT CASE WHEN b.src_module IS NULL THEN 'added' WHEN a.src_module IS NULL THEN 'removed' ELSE 'kept' END AS status,
       coalesce(a.src_module, b.src_module) AS src_module, coalesce(a.dst_module, b.dst_module) AS dst_module,
       coalesce(a.src_crate, b.src_crate) AS src_crate, coalesce(a.dst_crate, b.dst_crate) AS dst_crate,
       b.unit_edges AS unit_edges_before, a.unit_edges AS unit_edges_after, m.n AS snapshot_missing
FROM after_edges a
FULL JOIN before_edges b USING (src_module, dst_module)
CROSS JOIN before
CROSS JOIN missing m
WHERE getvariable('crate') IS NULL
   OR getvariable('crate') IN (coalesce(a.src_crate, b.src_crate), coalesce(a.dst_crate, b.dst_crate))
ORDER BY list_position(['added', 'removed', 'kept'], status),
         abs(coalesce(a.unit_edges, 0) - coalesce(b.unit_edges, 0)) DESC, src_module, dst_module;
