-- Question: for each axis column, which kept units got better or worse, and by how much?
-- Rows: one per kept unit and axis column of unit_quality whose value changed
-- (compare_unit_axes), with both values, the difference of a number, and the effect by
-- the column's rule (compare_column_rules: improved, regressed, or NULL for a change the
-- rule does not order). text_changed says whether the unit's text changed too (a value
-- can change with the text unchanged: a caller elsewhere, a module that joined a cycle).
-- Production before test; by axis and column; the largest difference first.
-- Run by `code-db compare` (README.md, Comparison). Thresholds: none.
-- Parameters (DuckDB variables; default when unset):
--   axis NULL (all; or size, duplication, coupling, independence, interface, intent,
--   unreferenced, or a column's own name for a column with no listed axis); column NULL;
--   effect NULL (all; or improved, regressed).
WITH params AS (
    SELECT CAST(getvariable('axis') AS VARCHAR) AS axis, CAST(getvariable('column') AS VARCHAR) AS column_name,
           CAST(getvariable('effect') AS VARCHAR) AS effect
)
SELECT CASE WHEN x.is_test THEN 'test' ELSE 'production' END AS scope, x.axis, x.column_name, x.crate, x.kind,
       x.qualified_name, x.before_value, x.after_value, x.delta, x.effect, x.text_changed
FROM compare_unit_axes x
CROSS JOIN params p
WHERE x.status = 'kept' AND x.change <> 'unchanged'
  AND (p.axis IS NULL OR x.axis = p.axis)
  AND (p.column_name IS NULL OR x.column_name = p.column_name)
  AND (p.effect IS NULL OR x.effect = p.effect)
ORDER BY x.is_test, x.axis, x.column_name, abs(x.delta) DESC NULLS LAST, x.effect, x.unit_key;
