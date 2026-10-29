-- Question: after a change between two snapshots, did the obstacles go down, and did new
-- ones appear?
-- Rows: one per obstacle list and scope (production, test): the units; each axis column
-- of unit_quality (compare_unit_axes); jscpd's clone pairs per report; module and crate
-- edges; module cycles; layer violations. Columns:
--   before_total, after_total: the list's size on each side: units, the sum of a numeric
--     axis column over all units (a 'lower' rule), the units whose value is an obstacle
--     (the other rules), clone pairs, edges, cycles, violations; NULL for a column whose
--     rule is 'none';
--   kept: the rows on both sides (units: all kept units; axis columns: kept units whose
--     value changed; edges, clones, cycles: the kept rows);
--   improved, regressed: kept rows that went down or up (units: NULL; axis columns: by
--     compare_column_rules; clone pairs: pairs between the two units; edges: unit edges;
--     cycles: member modules; violations: unit edges);
--   changed: kept rows that changed otherwise (units: the text changed; axis columns: a
--     change the rule does not order; cycles: the same size with other members);
--   appeared, disappeared: rows on one side only (axis columns: units on one side whose
--     value is an obstacle; NULL for 'none').
-- Edges are counted per scope by their unit edges of that scope: an edge is in production
-- on a side when it has a unit edge with no test at either end there, in test when it has
-- one with a test end. Units, clones, cycles and violations take their is_test.
-- Run by `code-db compare` (README.md, Comparison), which needs the SCIP indexes and the
-- jscpd reports of both snapshots. Thresholds: none.
WITH axes AS (
    SELECT CASE WHEN is_test THEN 'test' ELSE 'production' END AS scope, axis, column_name, rule,
           sum(CASE WHEN rule = 'lower' THEN TRY_CAST(before_value AS DOUBLE)
                    WHEN rule <> 'none' THEN obstacle_before::INTEGER END) AS before_total,
           sum(CASE WHEN rule = 'lower' THEN TRY_CAST(after_value AS DOUBLE)
                    WHEN rule <> 'none' THEN obstacle_after::INTEGER END) AS after_total,
           count(*) FILTER (WHERE status = 'kept' AND change <> 'unchanged') AS kept,
           count(*) FILTER (WHERE effect = 'improved') AS improved,
           count(*) FILTER (WHERE effect = 'regressed') AS regressed,
           count(*) FILTER (WHERE status = 'kept' AND change <> 'unchanged' AND effect IS NULL) AS changed,
           CASE WHEN rule <> 'none' THEN count(*) FILTER (WHERE status = 'appeared' AND obstacle_after) END AS appeared,
           CASE WHEN rule <> 'none' THEN count(*) FILTER (WHERE status = 'disappeared' AND obstacle_before) END
               AS disappeared
    FROM compare_unit_axes
    GROUP BY scope, axis, column_name, rule
),
units AS (
    SELECT CASE WHEN is_test THEN 'test' ELSE 'production' END AS scope,
           count(*) FILTER (WHERE status <> 'appeared') AS before_total,
           count(*) FILTER (WHERE status <> 'disappeared') AS after_total,
           count(*) FILTER (WHERE status = 'kept') AS kept,
           count(*) FILTER (WHERE text_changed) AS changed,
           count(*) FILTER (WHERE status = 'appeared') AS appeared,
           count(*) FILTER (WHERE status = 'disappeared') AS disappeared
    FROM compare_units
    GROUP BY ALL
),
clones AS (
    SELECT CASE WHEN is_test THEN 'test' ELSE 'production' END AS scope, source,
           sum(pairs_before) AS before_total, sum(pairs_after) AS after_total,
           count(*) FILTER (WHERE status = 'kept') AS kept,
           count(*) FILTER (WHERE status = 'kept' AND pairs_delta < 0) AS improved,
           count(*) FILTER (WHERE status = 'kept' AND pairs_delta > 0) AS regressed,
           count(*) FILTER (WHERE status = 'appeared') AS appeared,
           count(*) FILTER (WHERE status = 'disappeared') AS disappeared
    FROM compare_clones
    GROUP BY ALL
),
edge_scopes AS (
    SELECT level, 'production' AS scope, production_unit_edges_before AS w_before,
           production_unit_edges_after AS w_after
    FROM compare_edges
    UNION ALL
    SELECT level, 'test', test_unit_edges_before, test_unit_edges_after FROM compare_edges
),
edges AS (
    SELECT scope, level,
           count(*) FILTER (WHERE w_before > 0) AS before_total, count(*) FILTER (WHERE w_after > 0) AS after_total,
           count(*) FILTER (WHERE w_before > 0 AND w_after > 0) AS kept,
           count(*) FILTER (WHERE w_before > 0 AND w_after > 0 AND w_after < w_before) AS improved,
           count(*) FILTER (WHERE w_before > 0 AND w_after > 0 AND w_after > w_before) AS regressed,
           count(*) FILTER (WHERE w_before = 0 AND w_after > 0) AS appeared,
           count(*) FILTER (WHERE w_before > 0 AND w_after = 0) AS disappeared
    FROM edge_scopes
    GROUP BY ALL
),
cycles AS (
    SELECT CASE WHEN is_test THEN 'test' ELSE 'production' END AS scope,
           count(DISTINCT cycle_before) AS before_total, count(DISTINCT cycle_after) AS after_total,
           count(*) FILTER (WHERE status = 'kept') AS kept,
           count(*) FILTER (WHERE status = 'kept' AND modules_delta < 0) AS improved,
           count(*) FILTER (WHERE status = 'kept' AND modules_delta > 0) AS regressed,
           count(*) FILTER (WHERE status = 'kept' AND modules_delta = 0
                            AND (len(members_added) > 0 OR len(members_removed) > 0)) AS changed,
           count(*) FILTER (WHERE status = 'appeared') AS appeared,
           count(*) FILTER (WHERE status = 'disappeared') AS disappeared
    FROM compare_cycles
    GROUP BY ALL
),
violations AS (
    SELECT CASE WHEN is_test THEN 'test' ELSE 'production' END AS scope,
           count(*) FILTER (WHERE status <> 'appeared') AS before_total,
           count(*) FILTER (WHERE status <> 'disappeared') AS after_total,
           count(*) FILTER (WHERE status = 'kept') AS kept,
           count(*) FILTER (WHERE status = 'kept' AND unit_edges_delta < 0) AS improved,
           count(*) FILTER (WHERE status = 'kept' AND unit_edges_delta > 0) AS regressed,
           count(*) FILTER (WHERE status = 'appeared') AS appeared,
           count(*) FILTER (WHERE status = 'disappeared') AS disappeared
    FROM compare_layer_violations
    GROUP BY ALL
),
summary AS (
    SELECT 0 AS o, 'units' AS section, 'units' AS measure, scope, before_total::DOUBLE AS before_total,
           after_total::DOUBLE AS after_total, kept, NULL::BIGINT AS improved, NULL::BIGINT AS regressed, changed,
           appeared, disappeared
    FROM units
    UNION ALL
    SELECT 1, axis, column_name, scope, before_total, after_total, kept, improved, regressed, changed, appeared,
           disappeared
    FROM axes
    UNION ALL
    SELECT 2, 'duplication', 'clone pairs (' || source || ')', scope, before_total, after_total, kept, improved,
           regressed, NULL, appeared, disappeared
    FROM clones
    UNION ALL
    SELECT 3, 'coupling', level || ' edges', scope, before_total, after_total, kept, improved, regressed, NULL,
           appeared, disappeared
    FROM edges
    UNION ALL
    SELECT 4, 'independence', 'module cycles', scope, before_total, after_total, kept, improved, regressed, changed,
           appeared, disappeared
    FROM cycles
    UNION ALL
    SELECT 5, 'independence', 'layer violations', scope, before_total, after_total, kept, improved, regressed, NULL,
           appeared, disappeared
    FROM violations
)
SELECT section, measure, scope, before_total, after_total, kept, improved, regressed, changed, appeared, disappeared
FROM summary
ORDER BY o, section, measure, scope;
