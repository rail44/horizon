-- Question: which units do many files and crates depend on, which depend on many, and
-- which types look like contracts shared across crates?
-- Axis: coupling, fan-in and fan-out (docs/research/code-quality-axes-2026-10-29.md §3.6).
-- Rows: every unit with a fan-in or a fan-out (views/quality.sql): fan_in_files and
-- fan_in_crates count the other files and crates referencing anything the unit stands for
-- (unit_scope: the unit, what it contains, and for a type its impls' members),
-- fan_in_units the units outside it doing so; fan_out_files, fan_out_crates and
-- fan_out_units the same for what it references. contract_candidate marks a type
-- referenced from at least `min_crates` other crates: a type many crates use and whose
-- changes the compiler propagates to them (wire messages, identifiers, events), which the
-- reading checks judged no obstacle when the old fan-in x churn ranked them high; it is a
-- flag to read rows by, derived, not configured. Largest fan-in first. Module-level
-- coupling is queries/module_graph.sql.
-- Evidence: medium. SIG's module coupling correlates with defect resolution (0.55, then
-- 0.36) and feature work (0.69); Mo et al. (TSE 2021) find Unstable Interface (high fan-in
-- and change frequency) among the largest contributors. fan-in alone matched the
-- September sweep no better than the base rate (P@100 0.32 vs 0.29).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). The threshold
-- of contract_candidate is a parameter of this query, not a setting.
-- Needs the SCIP indexes.
-- Parameters (DuckDB variables; default when unset):
--   min_crates 3 (contract_candidate); kind NULL (all kinds; or one); include_tests false
--   (test units left out of the rows; references from tests still count).
WITH params AS (
    SELECT coalesce(CAST(getvariable('min_crates') AS INTEGER), 3) AS min_crates,
           CAST(getvariable('kind') AS VARCHAR) AS kind,
           coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests
)
SELECT u.path, u.crate, u.kind, u.qualified_name, u.start_line, u.loc,
       coalesce(fi.fan_in_files, 0) AS fan_in_files, coalesce(fi.fan_in_crates, 0) AS fan_in_crates,
       coalesce(fi.fan_in_units, 0) AS fan_in_units,
       coalesce(fo.fan_out_files, 0) AS fan_out_files, coalesce(fo.fan_out_crates, 0) AS fan_out_crates,
       coalesce(fo.fan_out_units, 0) AS fan_out_units,
       u.kind = 'type' AND coalesce(fi.fan_in_crates, 0) >= p.min_crates AS contract_candidate
FROM units u
CROSS JOIN params p
LEFT JOIN unit_fan_in fi USING (unit_id)
LEFT JOIN unit_fan_out fo USING (unit_id)
WHERE (fi.unit_id IS NOT NULL OR fo.unit_id IS NOT NULL)
  AND (p.kind IS NULL OR u.kind = p.kind)
  AND (p.include_tests OR NOT u.is_test)
ORDER BY fan_in_files DESC, fan_in_crates DESC, fan_out_files DESC, u.path, u.start_line;
