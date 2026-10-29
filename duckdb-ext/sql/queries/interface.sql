-- Question: which functions and methods ask the most of their callers, and how visible
-- are they?
-- Axis: interface, parameters and visibility (docs/research/code-quality-axes-2026-10-29.md §3.4).
-- Rows: every function and method with its parameter count (self included, from the SCIP
-- signature; views/units.sql), visibility (NULL: a trait's own member or a trait impl's,
-- which take the trait's), the crates other than its own that reference it
-- (fan_in_crates, unit_fan_in) and its signature. Most parameters first. The public
-- surface per crate is queries/public_surface.sql.
-- Evidence: weak. SIG's unit interfacing correlated with defect resolution at 0.19
-- (p = 0.042) in one model version and not significantly in the next (0.05), nor with
-- feature work; Clippy's too_many_arguments fires at 7.
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). Needs the
-- SCIP indexes.
-- Parameters (DuckDB variables; default when unset):
--   min_parameters 0; include_tests false.
WITH params AS (
    SELECT coalesce(CAST(getvariable('min_parameters') AS INTEGER), 0) AS min_parameters,
           coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests
)
SELECT u.path, u.crate, u.kind, u.qualified_name, u.start_line, u.parameters, u.visibility,
       coalesce(fi.fan_in_crates, 0) AS fan_in_crates, u.loc, u.signature
FROM units u
CROSS JOIN params p
LEFT JOIN unit_fan_in fi USING (unit_id)
WHERE u.kind IN ('function', 'method') AND coalesce(u.parameters, 0) >= p.min_parameters
  AND (p.include_tests OR NOT u.is_test)
ORDER BY u.parameters DESC NULLS LAST, fan_in_crates DESC, u.path, u.start_line;
