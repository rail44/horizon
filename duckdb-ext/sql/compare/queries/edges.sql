-- Question: which module and crate dependencies appeared, disappeared, or got heavier or
-- lighter?
-- Rows: one per edge of compare_edges (module edges by their two modules, crate edges by
-- their two crates) that is on one side only or whose unit edges or occurrences changed
-- (include_unchanged: every edge), with the unit edges in total, without a test end
-- (production) and with one (test), and the occurrences, on both sides. Production before
-- test (an edge is test when no unit edge without a test end exists on either side);
-- modules before crates; the largest change of unit edges first.
-- Run by `code-db compare` (README.md, Comparison). Thresholds: none.
-- Parameters (DuckDB variables; default when unset):
--   include_unchanged false; level NULL (both; or module, crate).
WITH params AS (
    SELECT coalesce(CAST(getvariable('include_unchanged') AS BOOLEAN), false) AS include_unchanged,
           CAST(getvariable('level') AS VARCHAR) AS level
)
SELECT e.status, CASE WHEN e.is_test THEN 'test' ELSE 'production' END AS scope, e.level, e.src, e.dst,
       e.nested_modules, e.unit_edges_before, e.unit_edges_after, e.unit_edges_delta,
       e.production_unit_edges_before, e.production_unit_edges_after, e.test_unit_edges_before,
       e.test_unit_edges_after, e.occurrences_before, e.occurrences_after
FROM compare_edges e
CROSS JOIN params p
WHERE (p.include_unchanged OR e.status <> 'kept' OR e.unit_edges_delta <> 0 OR e.occurrences_delta <> 0)
  AND (p.level IS NULL OR e.level = p.level)
ORDER BY e.is_test, e.level DESC, abs(e.unit_edges_delta) DESC, abs(e.occurrences_delta) DESC, e.status, e.src, e.dst;
