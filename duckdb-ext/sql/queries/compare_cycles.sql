-- Question: which module cycles did compare_before..HEAD create, remove or change?
-- Rows: the module cycles (module_cycles, views/graph.sql: strongly connected groups of
-- modules over the cycle edges, `cycle_edges`) of HEAD and of compare_before's tree
-- (snapshot_module_cycles, the same derivation over views/snapshot.sql and scip_before),
-- with the before members named as at HEAD (module_renames, as in compare_graph). A
-- before cycle and a HEAD cycle correspond when they share a member.
--   status     kept (a before and a HEAD cycle that share members; one row per such
--              pair), appeared (a HEAD cycle sharing no member with a before cycle),
--              disappeared (a before cycle sharing none with a HEAD cycle)
--   ambiguous  the cycle on one side shares members with several on the other (cycles
--              merged or split)
-- cycle_before/cycle_after name a cycle by its least member (the before one by its
-- renamed members); modules_*, unit_edges_* and members_* are each side's size, unit
-- edges inside it and members; shared_modules the members in both. snapshot_missing as
-- in compare_graph.
-- Ordering: status in the order appeared, disappeared, kept; then the larger size
-- descending, then the names.
-- Needs views/graph.sql, scip_before, and the persistent cache.
-- Parameters (DuckDB variables; default when unset): compare_before (required),
--   scip_before (required); cycle_edges (views/graph.sql, 'collapsed').
WITH before AS MATERIALIZED (
    SELECT CASE WHEN getvariable('scip_before') IS NULL
                THEN error('compare_cycles needs scip_before, the path of a SCIP index of compare_before''s tree')
                ELSE ord END AS ord
    FROM compare_commit
),
renamed AS MATERIALIZED (
    SELECT module_before, module FROM module_renames
),
before_members AS MATERIALIZED (
    SELECT DISTINCT c.cycle AS cycle_id, coalesce(r.module, u.member) AS module, c.unit_edges
    FROM snapshot_module_cycles c, unnest(c.members) u(member)
    LEFT JOIN renamed r ON r.module_before = u.member
),
before_cycles AS MATERIALIZED (
    SELECT cycle_id, min(module) AS cycle, count(*) AS modules, list(module ORDER BY module) AS members,
           any_value(unit_edges) AS unit_edges
    FROM before_members GROUP BY cycle_id
),
after_members AS MATERIALIZED (
    SELECT c.cycle, u.member AS module
    FROM module_cycles c, unnest(c.members) u(member)
),
after_cycles AS MATERIALIZED (
    SELECT cycle, modules, members, unit_edges FROM module_cycles
),
shared_pairs AS MATERIALIZED (
    SELECT b.cycle_id, a.cycle, count(*) AS shared_modules
    FROM before_members b JOIN after_members a USING (module)
    GROUP BY ALL
),
missing AS MATERIALIZED (
    SELECT count(*) AS n FROM snapshot_missing
),
rows_ AS (
    SELECT 'kept' AS status,
           count(*) OVER (PARTITION BY o.cycle_id) > 1 OR count(*) OVER (PARTITION BY o.cycle) > 1 AS ambiguous,
           b.cycle AS cycle_before, a.cycle AS cycle_after, b.modules AS modules_before, a.modules AS modules_after,
           o.shared_modules, b.unit_edges AS unit_edges_before, a.unit_edges AS unit_edges_after,
           b.members AS members_before, a.members AS members_after
    FROM shared_pairs o
    JOIN before_cycles b ON b.cycle_id = o.cycle_id
    JOIN after_cycles a ON a.cycle = o.cycle
    UNION ALL
    SELECT 'appeared', false, NULL, a.cycle, NULL, a.modules, 0, NULL, a.unit_edges, NULL, a.members
    FROM after_cycles a WHERE a.cycle NOT IN (SELECT cycle FROM shared_pairs)
    UNION ALL
    SELECT 'disappeared', false, b.cycle, NULL, b.modules, NULL, 0, b.unit_edges, NULL, b.members, NULL
    FROM before_cycles b WHERE b.cycle_id NOT IN (SELECT cycle_id FROM shared_pairs)
)
SELECT r.*, m.n AS snapshot_missing
FROM rows_ r
CROSS JOIN before
CROSS JOIN missing m
ORDER BY list_position(['appeared', 'disappeared', 'kept'], r.status),
         greatest(coalesce(r.modules_before, 0), coalesce(r.modules_after, 0)) DESC,
         r.cycle_after, r.cycle_before;
