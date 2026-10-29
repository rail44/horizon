-- Question: which code is duplicated, in which units, and is a duplicate a conversion
-- between two types that are meant to mirror each other?
-- Axis: duplication (docs/research/code-quality-axes-2026-10-29.md §3.5).
-- Rows: one per jscpd clone pair (clone_pairs, views/clones.sql; the `exact` report keeps
-- identifiers and literals, `normalized` ignores them), with each side's innermost unit
-- (clone_sides: the innermost unit whose lines contain the whole fragment, a file module
-- at worst): path, kind, qualified_name and whether it is a test. same_unit marks a pair
-- whose two fragments lie in one unit. conversion_pair marks a pair whose two sides belong
-- to two types joined by a conversion impl in either direction (`impl From<B> for A`,
-- `Into`, `TryFrom`, `TryInto`; the impl's descriptor `impl#[A][From<B>]`): a side
-- belongs to a type when it is the type or lies in one of its impls (unit_impl_type), and
-- B is matched by its bare name. Such pairs are the mirrored copies a conversion exists to
-- keep apart (a persisted form of a model type, a wire form of an event), flagged rather
-- than dropped. Largest pairs first.
-- Evidence: medium. Inconsistent changes to clones, co-change and developer awareness are
-- supported as predictors of harm, length is not (survey of 2026-10-24); SIG's duplication
-- correlates with defect resolution time (0.34 to 0.38), not with feature work. Type-1/2
-- clones in Rust are mostly per-type trait implementations.
-- Judgment from the settings file: what is scanned (scope.exclude: jscpd scans the files
-- of `files`) and what is a test (tests.*). Needs the jscpd reports (`code-db index`) and
-- the SCIP indexes.
-- Parameters (DuckDB variables; default when unset):
--   source NULL (both reports; or 'exact', 'normalized'); include_tests false (pairs with
--   a test unit on either side are left out).
WITH params AS (
    SELECT CAST(getvariable('source') AS VARCHAR) AS source,
           coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests
),
unit_rows AS MATERIALIZED (
    SELECT u.unit_id, u.path, u.kind, u.qualified_name, u.name, u.is_test,
           CASE WHEN u.kind = 'type' THEN u.unit_id ELSE t.type_unit_id END AS type_unit_id
    FROM units u LEFT JOIN unit_impl_type t USING (unit_id)
),
type_names AS MATERIALIZED (
    SELECT unit_id, name FROM units WHERE kind = 'type'
),
-- (type, the bare name of the type it converts from or into), both directions.
conversions AS MATERIALIZED (
    SELECT DISTINCT f.type_unit_id AS a, impl_generic_arg(i.impl_trait) AS b_name
    FROM impls i JOIN impl_for f USING (impl_id)
    WHERE impl_base_name(i.impl_trait) IN ('From', 'Into', 'TryFrom', 'TryInto')
),
sides AS (
    SELECT s.pair_id,
           any_value(s.unit_id) FILTER (WHERE s.side = 'a') AS unit_a,
           any_value(s.unit_id) FILTER (WHERE s.side = 'b') AS unit_b
    FROM clone_sides s GROUP BY s.pair_id
)
SELECT p.pair_id, p.source, p.lines, p.tokens,
       p.file_a, p.start_line_a, p.end_line_a, ua.kind AS kind_a, ua.qualified_name AS unit_a, ua.is_test AS test_a,
       p.file_b, p.start_line_b, p.end_line_b, ub.kind AS kind_b, ub.qualified_name AS unit_b, ub.is_test AS test_b,
       s.unit_a = s.unit_b AS same_unit,
       coalesce(ua.type_unit_id <> ub.type_unit_id AND (
           EXISTS (SELECT 1 FROM conversions c JOIN type_names n ON n.unit_id = ub.type_unit_id
                   WHERE c.a = ua.type_unit_id AND c.b_name = n.name)
           OR EXISTS (SELECT 1 FROM conversions c JOIN type_names n ON n.unit_id = ua.type_unit_id
                      WHERE c.a = ub.type_unit_id AND c.b_name = n.name)), false) AS conversion_pair
FROM clone_pairs p
CROSS JOIN params x
JOIN sides s USING (pair_id)
LEFT JOIN unit_rows ua ON ua.unit_id = s.unit_a
LEFT JOIN unit_rows ub ON ub.unit_id = s.unit_b
WHERE (x.source IS NULL OR p.source = x.source)
  AND (x.include_tests OR NOT (coalesce(ua.is_test, false) OR coalesce(ub.is_test, false)))
ORDER BY p.lines DESC, p.tokens DESC, p.source, p.pair_id;
