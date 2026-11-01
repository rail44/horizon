-- Question: how large is the surface of each unit the language's visibility boundary
-- encloses (a Rust package, a Go package) as the rest of the code actually uses it, against
-- what it declares public?
-- Axis: interface, the public surface (docs/research/code-quality-axes-2026-10-29.md §3.4).
-- Rows: one per package, or per namespace where the boundary is the namespace's
-- (views/terms.sql, visibility_boundary: Go; namespace is NULL otherwise), with units:
-- pub_units, its units declared public (visibility `pub`: Rust's from the signature, Go's
-- exported names); used_outside, its units whose own symbol is referenced from outside it
-- (a unit counted for its own symbol, not for what it contains: a type's fields and
-- variants and its impls' members are units or symbols of their own); pub_unused_outside,
-- the public units nothing outside references; outside_users, the distinct packages (or
-- namespaces) referencing any of its units' symbols. Largest used surface first.
-- Re-exports (`pub use`) are references, not declarations: a re-exported item is counted
-- where it is defined.
-- Evidence: weak (see interface.sql); the survey of 2026-10-24 found no effect study of
-- public-API tooling (cargo-public-api, cargo-semver-checks).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). Needs the
-- SCIP indexes.
-- Parameters (DuckDB variables; default when unset): include_tests false (test units and
-- references from them are left out).
WITH params AS (
    SELECT coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests,
           visibility_boundary() AS boundary
),
placed AS MATERIALIZED (
    SELECT u.unit_id, u.package, n.namespace, u.symbol, u.visibility, u.is_test, u.is_file_unit,
           CASE p.boundary WHEN 'namespace' THEN n.namespace ELSE u.package END AS enclosure
    FROM units u CROSS JOIN params p LEFT JOIN unit_namespace n USING (unit_id)
),
unit_rows AS MATERIALIZED (
    SELECT u.* FROM placed u, params p WHERE NOT u.is_file_unit AND (p.include_tests OR NOT u.is_test)
),
outside AS MATERIALIZED (
    SELECT DISTINCT d.unit_id, r.enclosure AS from_enclosure
    FROM unit_rows d
    JOIN unit_occurrences o ON o.symbol = d.symbol AND NOT o.is_definition
    JOIN placed r ON r.unit_id = o.unit_id
    CROSS JOIN params p
    WHERE r.enclosure IS DISTINCT FROM d.enclosure AND (p.include_tests OR NOT r.is_test)
)
SELECT d.package, CASE WHEN p.boundary = 'namespace' THEN d.enclosure END AS namespace,
       count(*) FILTER (WHERE d.visibility = 'pub') AS pub_units,
       count(*) FILTER (WHERE d.unit_id IN (SELECT unit_id FROM outside)) AS used_outside,
       count(*) FILTER (WHERE d.visibility = 'pub' AND d.unit_id NOT IN (SELECT unit_id FROM outside)) AS pub_unused_outside,
       (SELECT count(DISTINCT o.from_enclosure) FROM outside o JOIN unit_rows x USING (unit_id)
        WHERE x.enclosure = d.enclosure) AS outside_users
FROM unit_rows d
CROSS JOIN params p
GROUP BY d.package, d.enclosure, p.boundary
ORDER BY used_outside DESC, pub_units DESC, d.package, namespace;
