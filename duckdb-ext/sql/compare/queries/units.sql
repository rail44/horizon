-- Question: which units appeared, disappeared or changed between the two snapshots?
-- Rows: one per unit of compare_units (identified by crate, kind and SCIP descriptor; a
-- rename or a move to another module is a disappeared and an appeared row) that is on one
-- side only, whose text changed, or whose axis columns changed (include_unchanged: every
-- unit). changes lists the changed axis columns of a kept unit as `column before -> after`
-- (compare_unit_axes). path is AFTER's, BEFORE's for a disappeared unit. Production
-- before test; within each, the largest change of loc first (a kept unit's difference, a
-- unit on one side its loc), then the most changed columns.
-- Run by `code-db compare` (README.md, Comparison). Thresholds: none.
-- Parameters (DuckDB variables; default when unset):
--   include_unchanged false; status NULL (all; or kept, appeared, disappeared); kind NULL.
WITH params AS (
    SELECT coalesce(CAST(getvariable('include_unchanged') AS BOOLEAN), false) AS include_unchanged,
           CAST(getvariable('status') AS VARCHAR) AS status, CAST(getvariable('kind') AS VARCHAR) AS kind
),
changes AS (
    SELECT unit_key,
           list(column_name || ' ' || coalesce(before_value, 'NULL') || ' -> ' || coalesce(after_value, 'NULL')
                ORDER BY column_name) AS changes
    FROM compare_unit_axes
    WHERE status = 'kept' AND change <> 'unchanged'
    GROUP BY unit_key
)
SELECT u.status, CASE WHEN u.is_test THEN 'test' ELSE 'production' END AS scope, u.crate, u.kind,
       u.qualified_name, coalesce(u.path_after, u.path_before) AS path,
       CASE WHEN u.path_before <> u.path_after THEN u.path_before END AS path_before,
       u.text_changed, u.ambiguous, u.loc_before, u.loc_after, u.loc_after - u.loc_before AS loc_delta,
       c.changes
FROM compare_units u
CROSS JOIN params p
LEFT JOIN changes c USING (unit_key)
WHERE (p.include_unchanged OR u.status <> 'kept' OR u.text_changed OR c.changes IS NOT NULL)
  AND (p.status IS NULL OR u.status = p.status)
  AND (p.kind IS NULL OR u.kind = p.kind)
ORDER BY u.is_test, coalesce(abs(u.loc_after - u.loc_before), u.loc_after, u.loc_before) DESC,
         len(coalesce(c.changes, [])) DESC, u.status, u.unit_key;
