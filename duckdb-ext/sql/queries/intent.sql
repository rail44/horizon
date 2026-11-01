-- Question: where has the author marked code as unfinished, and which units used across
-- the language's visibility boundary carry no documentation?
-- Axis: clarity of intent, its mechanical part only (docs/research/code-quality-axes-2026-10-29.md
-- §3.15 documentation, §3.18 self-admitted technical debt). Whether a name or a comment
-- states the intent is not derivable and is not here.
-- Rows: every unit with debt_lines (lines holding TODO or FIXME, counted for the innermost
-- unit containing them, unit_debt_markers) or, when undocumented, referenced from across
-- the language's visibility boundary (views/terms.sql, visibility_boundary: another Rust
-- package, another Go package; fan_in_packages or fan_in_namespaces, unit_fan_in); has_doc is whether its symbol has documentation (doc
-- comments, SCIP's documentation). Debt markers first, then the undocumented units with
-- the widest use.
-- Evidence: weak to none. Computed understandability scores do not capture understanding
-- (survey of 2026-10-24); SIG holds comment counts unrelated to maintainability (a claim,
-- not a measurement); self-admitted technical debt research is not re-checked here
-- (prior knowledge only).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). The marker
-- words are TODO and FIXME. Needs the SCIP indexes.
-- Parameters (DuckDB variables; default when unset): include_tests false.
WITH settings AS (
    SELECT coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests,
           visibility_boundary() AS boundary
)
SELECT u.path, u.package, u.kind, u.qualified_name, u.start_line, u.visibility, u.has_doc,
       coalesce(d.debt_lines, 0) AS debt_lines, coalesce(fi.fan_in_packages, 0) AS fan_in_packages,
       coalesce(fi.fan_in_namespaces, 0) AS fan_in_namespaces, coalesce(fi.fan_in_files, 0) AS fan_in_files
FROM units u
CROSS JOIN settings s
LEFT JOIN unit_debt_markers d USING (unit_id)
LEFT JOIN unit_fan_in fi USING (unit_id)
CROSS JOIN LATERAL (
    SELECT s.include_tests,
           CASE s.boundary WHEN 'namespace' THEN fi.fan_in_namespaces ELSE fi.fan_in_packages END AS outside
) p
WHERE (coalesce(d.debt_lines, 0) > 0 OR (NOT u.has_doc AND coalesce(p.outside, 0) > 0))
  AND (p.include_tests OR NOT u.is_test)
ORDER BY debt_lines DESC, coalesce(p.outside, 0) DESC, fan_in_files DESC, u.path, u.start_line;
