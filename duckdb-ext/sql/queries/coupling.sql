-- Question: which units do many files, namespaces and packages depend on, which depend on
-- many, and which types look like contracts shared across the language's visibility
-- boundary?
-- Axis: coupling, fan-in and fan-out (docs/research/code-quality-axes-2026-10-29.md §3.6).
-- Rows, by `level`:
--   'unit' (default): every unit with a fan-in or a fan-out (views/quality.sql):
--     fan_in_files, fan_in_namespaces and fan_in_packages count the other files,
--     namespaces and packages referencing
--     anything the unit stands for (unit_scope: the unit, what it contains, and for a type
--     its members), fan_in_units the units outside it doing so; a namespace counts
--     only the references to its own symbol (a path naming it), not those to what it
--     contains, which the file and namespace levels sum. fan_out_files,
--     fan_out_namespaces, fan_out_packages and fan_out_units the same for what it
--     references (a namespace's: what it and its contents reference). contract_candidate
--     marks a type referenced from at least `min_packages` other packages (the language's
--     visibility boundary, views/terms.sql: a Rust package, a Go package, whose threshold
--     is then `min_namespaces`): a type many of them use and whose changes the compiler
--     propagates to them (wire messages, identifiers, events), which the reading checks judged no
--     obstacle when the old fan-in x churn ranked them high; it is a flag to read rows
--     by, derived, not configured;
--   'file': every source file with units and a fan-in or a fan-out, summed over its
--     units (unit_edges between two files): the other files, the packages other than its
--     own and the units in other files referencing a unit of the file, and the same for
--     what its units reference; loc is its file unit's;
--   'namespace': every namespace the same way across namespaces (unit_namespace): the
--     files, namespaces, packages and units of other namespaces referencing a unit of the
--     namespace, and those it references; loc is its namespace unit's, or the sum of its
--     file units' for a namespace of several files (a Go package);
--   'package': every package (SCIP package) the same way across packages: the files, packages and
--     units of other packages referencing a unit of the package, and those it references; loc
--     is the sum of its file units'.
-- Columns: level, path, namespace, package, kind, qualified_name, start_line, loc,
-- fan_in_files, fan_in_namespaces, fan_in_packages, fan_in_units, fan_out_files,
-- fan_out_namespaces, fan_out_packages, fan_out_units, contract_candidate (NULL where a
-- level has none). Largest fan-in first. Module-level
-- coupling is queries/namespace_graph.sql.
-- Evidence: medium. SIG's module coupling correlates with defect resolution (0.55, then
-- 0.36) and feature work (0.69); Mo et al. (TSE 2021) find Unstable Interface (high fan-in
-- and change frequency) among the largest contributors. fan-in alone matched the
-- September sweep no better than the base rate (P@100 0.32 vs 0.29).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). The threshold
-- of contract_candidate is a parameter of this query, not a setting.
-- Needs the SCIP indexes.
-- Parameters (DuckDB variables; default when unset):
--   level 'unit' ('file', 'namespace', 'package'); min_packages or min_namespaces (the
--   boundary's, above) 3 (contract_candidate); kind NULL (unit
--   level: all kinds; or one); include_tests false (unit and file level: test units, and
--   files whose namespace is a test, are left out of the rows; references from tests still
--   count).
WITH params AS (
    SELECT coalesce(CAST(getvariable('level') AS VARCHAR), 'unit') AS level,
           coalesce(CAST(CASE visibility_boundary() WHEN 'namespace' THEN getvariable('min_namespaces')
                             ELSE getvariable('min_packages') END AS INTEGER), 3) AS min_fan_in,
           visibility_boundary() AS boundary,
           CAST(getvariable('kind') AS VARCHAR) AS kind,
           coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests
),
checked AS (
    SELECT CASE WHEN level NOT IN ('unit', 'file', 'namespace', 'package')
                THEN error('coupling: level is unit, file, namespace or package')
                ELSE level END AS level, min_fan_in, boundary, kind, include_tests
    FROM params
),
unit_rows AS MATERIALIZED (
    SELECT u.unit_id, u.file_id, u.package, n.namespace
    FROM units u LEFT JOIN unit_namespace n USING (unit_id)
),
-- The unit edges between two files or two namespaces with both ends' file, namespace and
-- package (file, namespace and package levels only).
all_edges AS MATERIALIZED (
    SELECT s.unit_id AS src_unit_id, s.file_id AS src_file_id, s.namespace AS src_namespace,
           s.package AS src_package, d.unit_id AS dst_unit_id, d.file_id AS dst_file_id,
           d.namespace AS dst_namespace, d.package AS dst_package
    FROM unit_edges e
    JOIN unit_rows s ON s.unit_id = e.src_unit_id
    JOIN unit_rows d ON d.unit_id = e.dst_unit_id
    WHERE (s.file_id <> d.file_id OR s.namespace IS DISTINCT FROM d.namespace)
      AND (SELECT level FROM checked) <> 'unit'
),
edges AS MATERIALIZED (
    SELECT * FROM all_edges WHERE src_file_id <> dst_file_id
),
file_in AS (
    SELECT dst_file_id AS file_id, count(DISTINCT src_file_id) AS fan_in_files,
           count(DISTINCT src_namespace) FILTER (WHERE src_namespace IS DISTINCT FROM dst_namespace)
               AS fan_in_namespaces,
           count(DISTINCT src_package) FILTER (WHERE src_package <> dst_package) AS fan_in_packages,
           count(DISTINCT src_unit_id) AS fan_in_units
    FROM edges GROUP BY dst_file_id
),
file_out AS (
    SELECT src_file_id AS file_id, count(DISTINCT dst_file_id) AS fan_out_files,
           count(DISTINCT dst_namespace) FILTER (WHERE dst_namespace IS DISTINCT FROM src_namespace)
               AS fan_out_namespaces,
           count(DISTINCT dst_package) FILTER (WHERE dst_package <> src_package) AS fan_out_packages,
           count(DISTINCT dst_unit_id) AS fan_out_units
    FROM edges GROUP BY src_file_id
),
namespace_in AS (
    SELECT dst_namespace AS namespace, count(DISTINCT src_file_id) AS fan_in_files,
           count(DISTINCT src_namespace) AS fan_in_namespaces,
           count(DISTINCT src_package) FILTER (WHERE src_package <> dst_package) AS fan_in_packages,
           count(DISTINCT src_unit_id) AS fan_in_units
    FROM all_edges WHERE src_namespace IS DISTINCT FROM dst_namespace GROUP BY dst_namespace
),
namespace_out AS (
    SELECT src_namespace AS namespace, count(DISTINCT dst_file_id) AS fan_out_files,
           count(DISTINCT dst_namespace) AS fan_out_namespaces,
           count(DISTINCT dst_package) FILTER (WHERE dst_package <> src_package) AS fan_out_packages,
           count(DISTINCT dst_unit_id) AS fan_out_units
    FROM all_edges WHERE src_namespace IS DISTINCT FROM dst_namespace GROUP BY src_namespace
),
namespace_loc AS (
    SELECT n.namespace, n.package, coalesce(any_value(o.loc), sum(f.loc)) AS loc
    FROM namespaces n
    LEFT JOIN units o ON o.unit_id = n.namespace_unit_id
    LEFT JOIN file_namespaces fn ON n.namespace_unit_id IS NULL AND fn.namespace = n.namespace
    LEFT JOIN units f ON f.file_id = fn.file_id AND f.is_file_unit
    GROUP BY n.namespace, n.package
),
package_in AS (
    SELECT dst_package AS package, count(DISTINCT src_file_id) AS fan_in_files,
           count(DISTINCT src_namespace) AS fan_in_namespaces,
           count(DISTINCT src_package) AS fan_in_packages, count(DISTINCT src_unit_id) AS fan_in_units
    FROM edges WHERE src_package <> dst_package GROUP BY dst_package
),
package_out AS (
    SELECT src_package AS package, count(DISTINCT dst_file_id) AS fan_out_files,
           count(DISTINCT dst_namespace) AS fan_out_namespaces,
           count(DISTINCT dst_package) AS fan_out_packages, count(DISTINCT dst_unit_id) AS fan_out_units
    FROM edges WHERE src_package <> dst_package GROUP BY src_package
),
package_loc AS (
    SELECT package, sum(loc) AS loc FROM units WHERE is_file_unit GROUP BY package
)
SELECT 'unit' AS level, u.path, n.namespace, u.package, u.kind, u.qualified_name, u.start_line, u.loc,
       coalesce(fi.fan_in_files, 0) AS fan_in_files, coalesce(fi.fan_in_namespaces, 0) AS fan_in_namespaces,
       coalesce(fi.fan_in_packages, 0) AS fan_in_packages, coalesce(fi.fan_in_units, 0) AS fan_in_units,
       coalesce(fo.fan_out_files, 0) AS fan_out_files, coalesce(fo.fan_out_namespaces, 0) AS fan_out_namespaces,
       coalesce(fo.fan_out_packages, 0) AS fan_out_packages, coalesce(fo.fan_out_units, 0) AS fan_out_units,
       u.kind = 'type'
           AND coalesce(CASE p.boundary WHEN 'namespace' THEN fi.fan_in_namespaces ELSE fi.fan_in_packages END, 0)
               >= p.min_fan_in AS contract_candidate
FROM units u
CROSS JOIN checked p
LEFT JOIN unit_namespace n USING (unit_id)
LEFT JOIN unit_fan_in fi USING (unit_id)
LEFT JOIN unit_fan_out fo USING (unit_id)
WHERE p.level = 'unit'
  AND (fi.unit_id IS NOT NULL OR fo.unit_id IS NOT NULL)
  AND (p.kind IS NULL OR u.kind = p.kind)
  AND (p.include_tests OR NOT u.is_test)
UNION ALL
SELECT 'file', f.path, n.namespace, f.package, NULL, NULL, NULL, f.loc,
       coalesce(fi.fan_in_files, 0), coalesce(fi.fan_in_namespaces, 0), coalesce(fi.fan_in_packages, 0),
       coalesce(fi.fan_in_units, 0), coalesce(fo.fan_out_files, 0), coalesce(fo.fan_out_namespaces, 0),
       coalesce(fo.fan_out_packages, 0), coalesce(fo.fan_out_units, 0), NULL
FROM units f
CROSS JOIN checked p
LEFT JOIN file_namespaces n USING (file_id)
LEFT JOIN file_in fi USING (file_id)
LEFT JOIN file_out fo USING (file_id)
WHERE p.level = 'file' AND f.is_file_unit
  AND (fi.file_id IS NOT NULL OR fo.file_id IS NOT NULL)
  AND (p.include_tests OR NOT f.is_test)
UNION ALL
SELECT 'namespace', NULL, c.namespace, c.package, NULL, NULL, NULL, c.loc,
       coalesce(ci.fan_in_files, 0), coalesce(ci.fan_in_namespaces, 0), coalesce(ci.fan_in_packages, 0),
       coalesce(ci.fan_in_units, 0), coalesce(co.fan_out_files, 0), coalesce(co.fan_out_namespaces, 0),
       coalesce(co.fan_out_packages, 0), coalesce(co.fan_out_units, 0), NULL
FROM namespace_loc c
CROSS JOIN checked p
LEFT JOIN namespace_in ci USING (namespace)
LEFT JOIN namespace_out co USING (namespace)
WHERE p.level = 'namespace' AND (ci.namespace IS NOT NULL OR co.namespace IS NOT NULL)
UNION ALL
SELECT 'package', NULL, NULL, c.package, NULL, NULL, NULL, c.loc,
       coalesce(ci.fan_in_files, 0), coalesce(ci.fan_in_namespaces, 0), coalesce(ci.fan_in_packages, 0),
       coalesce(ci.fan_in_units, 0), coalesce(co.fan_out_files, 0), coalesce(co.fan_out_namespaces, 0),
       coalesce(co.fan_out_packages, 0), coalesce(co.fan_out_units, 0), NULL
FROM package_loc c
CROSS JOIN checked p
LEFT JOIN package_in ci USING (package)
LEFT JOIN package_out co USING (package)
WHERE p.level = 'package'
ORDER BY fan_in_files DESC, fan_in_packages DESC, fan_out_files DESC, path, namespace, package, start_line;
