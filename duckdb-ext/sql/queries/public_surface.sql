-- Question: how large is each crate's surface as other crates actually use it, against
-- what it declares public?
-- Axis: interface, the public surface (docs/research/code-quality-axes-2026-10-29.md §3.4).
-- Rows: one per crate (SCIP package) with units: pub_units, its units declared `pub`
-- (visibility, from the signature); used_from_other_crates, its units whose own symbol
-- other crates reference (a unit counted for its own symbol, not for what it contains:
-- a type's fields and variants and its impls' members are units or symbols of their own);
-- pub_unused_outside, the `pub` units no other crate references; other_crates, the
-- distinct crates referencing any of its units' symbols. Largest used surface first.
-- Re-exports (`pub use`) are references, not declarations: a re-exported item is counted
-- in the crate that defines it.
-- Evidence: weak (see interface.sql); the survey of 2026-10-24 found no effect study of
-- public-API tooling (cargo-public-api, cargo-semver-checks).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). Needs the
-- SCIP indexes.
-- Parameters (DuckDB variables; default when unset): include_tests false (test units and
-- references from them are left out).
WITH params AS (
    SELECT coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests
),
unit_rows AS MATERIALIZED (
    SELECT u.unit_id, u.crate, u.symbol, u.visibility, u.is_test
    FROM units u, params p WHERE NOT u.is_file_module AND (p.include_tests OR NOT u.is_test)
),
outside AS MATERIALIZED (
    SELECT DISTINCT d.unit_id, r.crate AS from_crate
    FROM unit_rows d
    JOIN unit_occurrences o ON o.symbol = d.symbol AND NOT o.is_definition
    JOIN units r ON r.unit_id = o.unit_id
    CROSS JOIN params p
    WHERE r.crate <> d.crate AND (p.include_tests OR NOT r.is_test)
)
SELECT d.crate,
       count(*) FILTER (WHERE d.visibility = 'pub') AS pub_units,
       count(*) FILTER (WHERE d.unit_id IN (SELECT unit_id FROM outside)) AS used_from_other_crates,
       count(*) FILTER (WHERE d.visibility = 'pub' AND d.unit_id NOT IN (SELECT unit_id FROM outside)) AS pub_unused_outside,
       (SELECT count(DISTINCT o.from_crate) FROM outside o JOIN unit_rows x USING (unit_id) WHERE x.crate = d.crate)
           AS other_crates
FROM unit_rows d
GROUP BY d.crate
ORDER BY used_from_other_crates DESC, pub_units DESC, d.crate;
