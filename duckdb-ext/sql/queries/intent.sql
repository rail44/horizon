-- Question: where has the author marked code as unfinished, and which units other crates
-- depend on carry no documentation?
-- Axis: clarity of intent, its mechanical part only (docs/research/code-quality-axes-2026-10-29.md
-- §3.15 documentation, §3.18 self-admitted technical debt). Whether a name or a comment
-- states the intent is not derivable and is not here.
-- Rows: every unit with debt_lines (lines holding TODO or FIXME, counted for the innermost
-- unit containing them, unit_debt_markers) or, when undocumented, referenced from another
-- crate (fan_in_crates, unit_fan_in); has_doc is whether its symbol has documentation (doc
-- comments, SCIP's documentation). Debt markers first, then the undocumented units with
-- the widest use.
-- Evidence: weak to none. Computed understandability scores do not capture understanding
-- (survey of 2026-10-24); SIG holds comment counts unrelated to maintainability (a claim,
-- not a measurement); self-admitted technical debt research is not re-checked here
-- (prior knowledge only).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). The marker
-- words are TODO and FIXME. Needs the SCIP indexes.
-- Parameters (DuckDB variables; default when unset): include_tests false.
WITH params AS (
    SELECT coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests
)
SELECT u.path, u.crate, u.kind, u.qualified_name, u.start_line, u.visibility, u.has_doc,
       coalesce(d.debt_lines, 0) AS debt_lines, coalesce(fi.fan_in_crates, 0) AS fan_in_crates,
       coalesce(fi.fan_in_files, 0) AS fan_in_files
FROM units u
CROSS JOIN params p
LEFT JOIN unit_debt_markers d USING (unit_id)
LEFT JOIN unit_fan_in fi USING (unit_id)
WHERE (coalesce(d.debt_lines, 0) > 0 OR (NOT u.has_doc AND coalesce(fi.fan_in_crates, 0) > 0))
  AND (p.include_tests OR NOT u.is_test)
ORDER BY debt_lines DESC, fan_in_crates DESC, fan_in_files DESC, u.path, u.start_line;
