-- Question: which duplicates appeared or disappeared, and between which units did the
-- number of clone pairs change?
-- Rows: one per report and pair of units of compare_clones (jscpd's pairs grouped by
-- their two innermost units, identified by the units' keys) that is on one side only or
-- whose number of pairs changed (include_unchanged: every row). Production before test
-- (a pair is test when a side is a test unit); the largest change of duplicated lines
-- first.
-- Run by `code-db compare` (README.md, Comparison). Thresholds: none.
-- Parameters (DuckDB variables; default when unset):
--   include_unchanged false; source NULL (both reports; or exact, normalized).
WITH params AS (
    SELECT coalesce(CAST(getvariable('include_unchanged') AS BOOLEAN), false) AS include_unchanged,
           CAST(getvariable('source') AS VARCHAR) AS source
)
SELECT c.status, CASE WHEN c.is_test THEN 'test' ELSE 'production' END AS scope, c.source, c.unit_a, c.path_a,
       c.unit_b, c.path_b, c.pairs_before, c.pairs_after, c.pairs_delta, c.lines_before, c.lines_after
FROM compare_clones c
CROSS JOIN params p
WHERE (p.include_unchanged OR c.status <> 'kept' OR c.pairs_delta <> 0)
  AND (p.source IS NULL OR c.source = p.source)
ORDER BY c.is_test, abs(c.lines_after - c.lines_before) DESC, c.status, c.source, c.key_a, c.key_b;
