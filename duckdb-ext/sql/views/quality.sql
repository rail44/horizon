-- The current tree's quality axes as per-unit facts (docs/research/code-quality-axes-2026-10-29.md):
-- coupling (fan-in, fan-out), unreferenced definitions, self-declared debt (TODO/FIXME),
-- duplication per unit, and unit_quality, which joins every axis's per-unit columns
-- without combining them. Read after views/graph.sql and views/clones.sql. The queries in
-- queries/ ask one question per axis over these views.

-- unit_scope(unit_id, member_unit_id): what a unit stands for when its fan-in and fan-out
-- are counted: the unit itself and every unit it contains, and for a type also the
-- members of its impls (impl_for) with everything they contain, wherever the impls are.
CREATE OR REPLACE VIEW unit_scope AS
WITH contained AS MATERIALIZED (
    SELECT ancestor_id AS unit_id, unit_id AS member_unit_id FROM unit_ancestors
),
impl_members AS MATERIALIZED (
    SELECT f.type_unit_id AS unit_id, unnest(i.members) AS member_id
    FROM impl_for f JOIN impls i USING (impl_id)
)
SELECT unit_id, member_unit_id FROM contained
UNION
SELECT m.unit_id, c.member_unit_id
FROM impl_members m JOIN contained c ON c.unit_id = m.member_id;

-- unit_fan_in(unit_id, fan_in_files, fan_in_crates, fan_in_units, reference_occurrences):
-- the files other than the unit's own, and the crates other than its own, holding a
-- reference occurrence of a symbol whose definition belongs to the unit's scope
-- (symbol_units: the items, fields, variants and locals-free symbols it defines);
-- fan_in_units the distinct units outside the scope holding one, reference_occurrences
-- all those occurrences. Units nobody outside references have no row.
CREATE OR REPLACE VIEW unit_fan_in AS
WITH unit_rows AS MATERIALIZED (
    SELECT unit_id, file_id, crate FROM units
),
owned AS MATERIALIZED (
    SELECT DISTINCT s.unit_id, su.symbol
    FROM unit_scope s JOIN symbol_units su ON su.unit_id = s.member_unit_id
),
refs AS MATERIALIZED (
    SELECT o.symbol, o.file_id, o.unit_id, r.crate
    FROM unit_occurrences o JOIN unit_rows r USING (unit_id)
    WHERE NOT o.is_definition AND NOT o.is_local
),
scopes AS MATERIALIZED (
    SELECT * FROM unit_scope
)
SELECT w.unit_id,
       count(DISTINCT r.file_id) FILTER (WHERE r.file_id <> u.file_id) AS fan_in_files,
       count(DISTINCT r.crate) FILTER (WHERE r.crate <> u.crate) AS fan_in_crates,
       count(DISTINCT r.unit_id) AS fan_in_units,
       count(*) AS reference_occurrences
FROM owned w
JOIN unit_rows u USING (unit_id)
JOIN refs r ON r.symbol = w.symbol
WHERE NOT EXISTS (SELECT 1 FROM scopes s WHERE s.unit_id = w.unit_id AND s.member_unit_id = r.unit_id)
GROUP BY w.unit_id;

-- unit_fan_out(unit_id, fan_out_files, fan_out_crates, fan_out_units): the files other
-- than the unit's own, the crates other than its own, and the units outside its scope
-- that the unit's scope references (unit_edges from a member to a unit outside the scope).
CREATE OR REPLACE VIEW unit_fan_out AS
WITH unit_rows AS MATERIALIZED (
    SELECT unit_id, file_id, crate FROM units
),
scopes AS MATERIALIZED (
    SELECT * FROM unit_scope
),
edges AS MATERIALIZED (
    SELECT src_unit_id, dst_unit_id FROM unit_edges WHERE NOT is_self
)
SELECT s.unit_id,
       count(DISTINCT d.file_id) FILTER (WHERE d.file_id <> u.file_id) AS fan_out_files,
       count(DISTINCT d.crate) FILTER (WHERE d.crate <> u.crate) AS fan_out_crates,
       count(DISTINCT d.unit_id) AS fan_out_units
FROM scopes s
JOIN unit_rows u ON u.unit_id = s.unit_id
JOIN edges e ON e.src_unit_id = s.member_unit_id
JOIN unit_rows d ON d.unit_id = e.dst_unit_id
WHERE NOT EXISTS (SELECT 1 FROM scopes x WHERE x.unit_id = s.unit_id AND x.member_unit_id = e.dst_unit_id)
GROUP BY s.unit_id;

-- unit_reference_status(unit_id, references, status): the reference occurrences of the
-- unit's own symbol anywhere (its own file included), and status: 'referenced'; else the
-- reason it is structurally unreferenced rather than dead: 'test' (a test unit),
-- 'trait_impl' (a trait impl's member: calls resolve to the trait's item), 'entry_point'
-- (a function named main), 'crate_root' (a crate's root module), 'trait_item' (a trait's
-- own item, which its impls implement; calls on a concrete type resolve to the impl's
-- item); else 'unreferenced'.
-- Other entry points (callbacks registered by name, FFI exports, derive-generated uses,
-- macro bodies, which SCIP does not resolve) show as 'unreferenced'.
CREATE OR REPLACE VIEW unit_reference_status AS
WITH counts AS (
    SELECT symbol, count(*) AS n FROM scip_occurrences WHERE is_reference GROUP BY symbol
)
SELECT u.unit_id, coalesce(c.n, 0) AS references,
       CASE WHEN coalesce(c.n, 0) > 0 THEN 'referenced'
            WHEN u.is_test THEN 'test'
            WHEN u.impl_trait IS NOT NULL THEN 'trait_impl'
            WHEN u.kind = 'function' AND u.name = 'main' THEN 'entry_point'
            WHEN u.is_file_module AND ends_with(u.symbol, ' crate/') THEN 'crate_root'
            WHEN u.member_of = 'trait' THEN 'trait_item'
            ELSE 'unreferenced' END AS status
FROM units u LEFT JOIN counts c USING (symbol);

-- unit_debt_markers(unit_id, debt_lines): the lines holding the word TODO or FIXME
-- (anywhere on the line, comments and strings alike), each counted for the innermost
-- unit containing it, so that an enclosing unit does not repeat its members' lines.
CREATE OR REPLACE VIEW unit_debt_markers AS
WITH marked AS MATERIALIZED (
    SELECT file_id, line FROM source_lines WHERE regexp_matches(text, '\b(TODO|FIXME)\b')
),
unit_lines AS MATERIALIZED (
    SELECT unit_id, file_id, start_line, end_line FROM units
),
innermost AS (
    SELECT m.file_id, m.line, max(u.unit_id) AS unit_id
    FROM marked m JOIN unit_lines u ON u.file_id = m.file_id AND u.start_line <= m.line AND m.line <= u.end_line
    GROUP BY ALL
)
SELECT unit_id, count(*) AS debt_lines FROM innermost GROUP BY unit_id;

-- unit_clones(unit_id, clone_pairs, exact_pairs, normalized_pairs): the clone pairs with a
-- side whose innermost unit is this one (clone_sides), per report.
CREATE OR REPLACE VIEW unit_clones AS
SELECT s.unit_id, count(DISTINCT s.pair_id) AS clone_pairs,
       count(DISTINCT s.pair_id) FILTER (WHERE p.source = 'exact') AS exact_pairs,
       count(DISTINCT s.pair_id) FILTER (WHERE p.source = 'normalized') AS normalized_pairs
FROM clone_sides s JOIN clone_pairs p USING (pair_id)
WHERE s.unit_id IS NOT NULL
GROUP BY s.unit_id;

-- unit_quality: one row per unit with the per-unit columns of every axis, side by side and
-- not combined (no score):
--   identity: unit_id, path, crate, module, kind, qualified_name, start_line, end_line,
--     is_test;
--   size: loc;
--   duplication: clone_pairs, exact_pairs, normalized_pairs;
--   coupling: fan_in_files, fan_in_crates, fan_out_files, fan_out_crates;
--   independence: cycle (the module cycle its module is in, module_cycle_edges), layer;
--   interface: visibility, parameters;
--   intent: has_doc, debt_lines;
--   unreferenced: references, reference_status.
CREATE OR REPLACE VIEW unit_quality AS
WITH cycle_members AS MATERIALIZED (
    SELECT DISTINCT src_module AS module, cycle FROM module_cycle_edges
)
SELECT u.unit_id, u.path, u.crate, m.module, u.kind, u.qualified_name, u.start_line, u.end_line, u.is_test,
       u.loc,
       coalesce(c.clone_pairs, 0) AS clone_pairs, coalesce(c.exact_pairs, 0) AS exact_pairs,
       coalesce(c.normalized_pairs, 0) AS normalized_pairs,
       coalesce(fi.fan_in_files, 0) AS fan_in_files, coalesce(fi.fan_in_crates, 0) AS fan_in_crates,
       coalesce(fo.fan_out_files, 0) AS fan_out_files, coalesce(fo.fan_out_crates, 0) AS fan_out_crates,
       cm.cycle, l.layer,
       u.visibility, u.parameters,
       u.has_doc, coalesce(d.debt_lines, 0) AS debt_lines,
       r.references, r.status AS reference_status
FROM units u
LEFT JOIN unit_module m USING (unit_id)
LEFT JOIN unit_clones c USING (unit_id)
LEFT JOIN unit_fan_in fi USING (unit_id)
LEFT JOIN unit_fan_out fo USING (unit_id)
LEFT JOIN cycle_members cm ON cm.module = m.module
LEFT JOIN module_layers l ON l.module = m.module
LEFT JOIN unit_debt_markers d USING (unit_id)
LEFT JOIN unit_reference_status r USING (unit_id);
