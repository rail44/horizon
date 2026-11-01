-- Question: which units do many files and packages depend on, which depend on many, and
-- which types look like contracts shared across packages?
-- Axis: coupling, fan-in and fan-out (docs/research/code-quality-axes-2026-10-29.md §3.6).
-- Rows, by `level`:
--   'unit' (default): every unit with a fan-in or a fan-out (views/quality.sql):
--     fan_in_files and fan_in_packages count the other files and packages referencing
--     anything the unit stands for (unit_scope: the unit, what it contains, and for a type
--     its impls' members), fan_in_units the units outside it doing so; a namespace counts
--     only the references to its own symbol (a path naming it), not those to what it
--     contains, which the file level sums. fan_out_files, fan_out_packages and
--     fan_out_units the same for what it references (a namespace's: what it and its contents
--     reference). contract_candidate marks a type referenced from at least `min_packages`
--     other packages: a type many packages use and whose changes the compiler propagates to
--     them (wire messages, identifiers, events), which the reading checks judged no
--     obstacle when the old fan-in x churn ranked them high; it is a flag to read rows
--     by, derived, not configured;
--   'file': every source file with units and a fan-in or a fan-out, summed over its
--     units (unit_edges between two files): the other files, the packages other than its
--     own and the units in other files referencing a unit of the file, and the same for
--     what its units reference; loc is its file unit's;
--   'package': every package (SCIP package) the same way across packages: the files, packages and
--     units of other packages referencing a unit of the package, and those it references; loc
--     is the sum of its file units'.
-- Columns: level, path, package, kind, qualified_name, start_line, loc, fan_in_files,
-- fan_in_packages, fan_in_units, fan_out_files, fan_out_packages, fan_out_units,
-- contract_candidate (NULL where a level has none). Largest fan-in first. Module-level
-- coupling is queries/namespace_graph.sql.
-- Evidence: medium. SIG's module coupling correlates with defect resolution (0.55, then
-- 0.36) and feature work (0.69); Mo et al. (TSE 2021) find Unstable Interface (high fan-in
-- and change frequency) among the largest contributors. fan-in alone matched the
-- September sweep no better than the base rate (P@100 0.32 vs 0.29).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). The threshold
-- of contract_candidate is a parameter of this query, not a setting.
-- Needs the SCIP indexes.
-- Parameters (DuckDB variables; default when unset):
--   level 'unit' ('file', 'package'); min_packages 3 (contract_candidate); kind NULL (unit
--   level: all kinds; or one); include_tests false (unit and file level: test units, and
--   files whose namespace is a test, are left out of the rows; references from tests still
--   count).
WITH params AS (
    SELECT coalesce(CAST(getvariable('level') AS VARCHAR), 'unit') AS level,
           coalesce(CAST(getvariable('min_packages') AS INTEGER), 3) AS min_packages,
           CAST(getvariable('kind') AS VARCHAR) AS kind,
           coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests
),
checked AS (
    SELECT CASE WHEN level NOT IN ('unit', 'file', 'package') THEN error('coupling: level is unit, file or package')
                ELSE level END AS level, min_packages, kind, include_tests
    FROM params
),
unit_rows AS MATERIALIZED (
    SELECT unit_id, file_id, package FROM units
),
-- The unit edges between two files with both ends' file and package (file and
-- package levels only).
edges AS MATERIALIZED (
    SELECT s.unit_id AS src_unit_id, s.file_id AS src_file_id, s.package AS src_package,
           d.unit_id AS dst_unit_id, d.file_id AS dst_file_id, d.package AS dst_package
    FROM unit_edges e
    JOIN unit_rows s ON s.unit_id = e.src_unit_id
    JOIN unit_rows d ON d.unit_id = e.dst_unit_id
    WHERE s.file_id <> d.file_id AND (SELECT level FROM checked) <> 'unit'
),
file_in AS (
    SELECT dst_file_id AS file_id, count(DISTINCT src_file_id) AS fan_in_files,
           count(DISTINCT src_package) FILTER (WHERE src_package <> dst_package) AS fan_in_packages,
           count(DISTINCT src_unit_id) AS fan_in_units
    FROM edges GROUP BY dst_file_id
),
file_out AS (
    SELECT src_file_id AS file_id, count(DISTINCT dst_file_id) AS fan_out_files,
           count(DISTINCT dst_package) FILTER (WHERE dst_package <> src_package) AS fan_out_packages,
           count(DISTINCT dst_unit_id) AS fan_out_units
    FROM edges GROUP BY src_file_id
),
package_in AS (
    SELECT dst_package AS package, count(DISTINCT src_file_id) AS fan_in_files,
           count(DISTINCT src_package) AS fan_in_packages, count(DISTINCT src_unit_id) AS fan_in_units
    FROM edges WHERE src_package <> dst_package GROUP BY dst_package
),
package_out AS (
    SELECT src_package AS package, count(DISTINCT dst_file_id) AS fan_out_files,
           count(DISTINCT dst_package) AS fan_out_packages, count(DISTINCT dst_unit_id) AS fan_out_units
    FROM edges WHERE src_package <> dst_package GROUP BY src_package
),
package_loc AS (
    SELECT package, sum(loc) AS loc FROM units WHERE is_file_unit GROUP BY package
)
SELECT 'unit' AS level, u.path, u.package, u.kind, u.qualified_name, u.start_line, u.loc,
       coalesce(fi.fan_in_files, 0) AS fan_in_files, coalesce(fi.fan_in_packages, 0) AS fan_in_packages,
       coalesce(fi.fan_in_units, 0) AS fan_in_units,
       coalesce(fo.fan_out_files, 0) AS fan_out_files, coalesce(fo.fan_out_packages, 0) AS fan_out_packages,
       coalesce(fo.fan_out_units, 0) AS fan_out_units,
       u.kind = 'type' AND coalesce(fi.fan_in_packages, 0) >= p.min_packages AS contract_candidate
FROM units u
CROSS JOIN checked p
LEFT JOIN unit_fan_in fi USING (unit_id)
LEFT JOIN unit_fan_out fo USING (unit_id)
WHERE p.level = 'unit'
  AND (fi.unit_id IS NOT NULL OR fo.unit_id IS NOT NULL)
  AND (p.kind IS NULL OR u.kind = p.kind)
  AND (p.include_tests OR NOT u.is_test)
UNION ALL
SELECT 'file', f.path, f.package, NULL, NULL, NULL, f.loc,
       coalesce(fi.fan_in_files, 0), coalesce(fi.fan_in_packages, 0), coalesce(fi.fan_in_units, 0),
       coalesce(fo.fan_out_files, 0), coalesce(fo.fan_out_packages, 0), coalesce(fo.fan_out_units, 0), NULL
FROM units f
CROSS JOIN checked p
LEFT JOIN file_in fi USING (file_id)
LEFT JOIN file_out fo USING (file_id)
WHERE p.level = 'file' AND f.is_file_unit
  AND (fi.file_id IS NOT NULL OR fo.file_id IS NOT NULL)
  AND (p.include_tests OR NOT f.is_test)
UNION ALL
SELECT 'package', NULL, c.package, NULL, NULL, NULL, c.loc,
       coalesce(ci.fan_in_files, 0), coalesce(ci.fan_in_packages, 0), coalesce(ci.fan_in_units, 0),
       coalesce(co.fan_out_files, 0), coalesce(co.fan_out_packages, 0), coalesce(co.fan_out_units, 0), NULL
FROM package_loc c
CROSS JOIN checked p
LEFT JOIN package_in ci USING (package)
LEFT JOIN package_out co USING (package)
WHERE p.level = 'package'
ORDER BY fan_in_files DESC, fan_in_packages DESC, fan_out_files DESC, path, package, start_line;
