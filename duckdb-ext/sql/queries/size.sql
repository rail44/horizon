-- Question: where is the code large: which units, files and packages hold the most lines?
-- Axis: size (docs/research/code-quality-axes-2026-10-29.md §3.1 volume, §3.2 unit size).
-- Rows, by `level`:
--   'unit' (default): every unit but namespaces (a file unit is the file level), with its
--     loc (non-blank lines of its range, leading docs and attributes included; an
--     enclosing unit counts what it contains), largest first;
--   'file': every source file with units: its file unit's loc and its unit count;
--   'package': every package (SCIP package): loc (the sum of its file units'), units, and
--     test_loc_pct, the share of that loc inside test units (the outermost ones: a test
--     file's module, a `mod tests`, a test function in a non-test module).
-- Columns: level, package, path, kind, qualified_name, start_line, loc, units, is_test,
-- test_loc_pct (NULL where a level has none).
-- Evidence: medium. Unit size is SIG's most stable property (correlation 0.53 with defect
-- resolution time across model versions); volume correlates with defect resolution (0.33
-- to 0.39) and feature work (0.61) but is a confounder of most metrics. No Rust study;
-- thresholds are relative to a benchmark, so rows are ranked, not cut.
-- Judgment from the settings file: what counts (scope.exclude) and what is a test
-- (tests.paths, tests.extra_attributes). Needs the SCIP indexes.
-- Parameters (DuckDB variables; default when unset):
--   level 'unit' ('file', 'package'); kind NULL (unit level: all kinds but namespace; or one
--   kind); include_tests false (unit and file level: test units, and files whose namespace
--   is a test, are left out).
WITH params AS (
    SELECT coalesce(CAST(getvariable('level') AS VARCHAR), 'unit') AS level,
           CAST(getvariable('kind') AS VARCHAR) AS kind,
           coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests
),
checked AS (
    SELECT CASE WHEN level NOT IN ('unit', 'file', 'package') THEN error('size: level is unit, file or package')
                ELSE level END AS level, kind, include_tests
    FROM params
),
unit_rows AS MATERIALIZED (
    SELECT * FROM units
),
-- The outermost test units: a test whose parent is not one (a test file's module, a
-- `mod tests`, a test function in a non-test namespace).
test_roots AS MATERIALIZED (
    SELECT u.package, u.loc
    FROM unit_rows u LEFT JOIN unit_rows p ON p.unit_id = u.parent_unit_id
    WHERE u.is_test AND NOT coalesce(p.is_test, false)
),
package_rows AS (
    SELECT f.package, count(*) AS files, sum(f.loc) AS loc,
           (SELECT count(*) FROM unit_rows u WHERE u.package = f.package AND NOT u.is_file_unit) AS units,
           round(100.0 * coalesce((SELECT sum(t.loc) FROM test_roots t WHERE t.package = f.package), 0) / sum(f.loc), 1)
               AS test_loc_pct
    FROM unit_rows f WHERE f.is_file_unit
    GROUP BY f.package
)
SELECT 'unit' AS level, u.package, u.path, u.kind, u.qualified_name, u.start_line, u.loc, NULL::BIGINT AS units,
       u.is_test, NULL::DOUBLE AS test_loc_pct
FROM unit_rows u, checked c
WHERE c.level = 'unit' AND u.kind <> 'namespace' AND (c.kind IS NULL OR u.kind = c.kind)
  AND (c.include_tests OR NOT u.is_test)
UNION ALL
SELECT 'file', f.package, f.path, NULL, NULL, NULL, f.loc,
       (SELECT count(*) FROM unit_rows u WHERE u.file_id = f.file_id AND NOT u.is_file_unit), f.is_test, NULL
FROM unit_rows f, checked c
WHERE c.level = 'file' AND f.is_file_unit AND (c.include_tests OR NOT f.is_test)
UNION ALL
SELECT 'package', r.package, NULL, NULL, NULL, NULL, r.loc, r.units, NULL, r.test_loc_pct
FROM package_rows r, checked c
WHERE c.level = 'package'
ORDER BY loc DESC, path, start_line;
