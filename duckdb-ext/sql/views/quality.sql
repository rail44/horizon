-- The current tree's quality axes as per-unit facts (docs/research/code-quality-axes-2026-10-29.md):
-- coupling (fan-in, fan-out), unreferenced definitions, self-declared debt (TODO/FIXME),
-- duplication per unit, and unit_quality, which joins every axis's per-unit columns
-- without combining them. Read after views/graph.sql and views/clones.sql. The queries in
-- queries/ ask one question per axis over these views.

-- unit_scope(unit_id, member_unit_id): what a unit stands for when its fan-in and fan-out
-- are counted: the unit itself and every unit it contains, and for a type also the
-- members (unit_owner_types) with everything they contain, wherever they are.
CREATE OR REPLACE VIEW unit_scope AS
WITH contained AS MATERIALIZED (
    SELECT ancestor_id AS unit_id, unit_id AS member_unit_id FROM unit_ancestors
),
type_members AS MATERIALIZED (
    SELECT f.to_unit_id AS unit_id, f.unit_id AS member_id
    FROM type_fact_units f WHERE f.fact = 'member' AND f.to_unit_id IS NOT NULL
)
SELECT unit_id, member_unit_id FROM contained
UNION
SELECT m.unit_id, c.member_unit_id
FROM type_members m JOIN contained c ON c.unit_id = m.member_id;

-- unit_fan_in(unit_id, fan_in_files, fan_in_namespaces, fan_in_packages, fan_in_units,
--     reference_occurrences): the files other than the unit's own, the namespaces other
-- than its own (unit_namespace) and the packages other than its own holding a reference
-- occurrence of a symbol whose definition belongs to the unit's scope (symbol_units: the
-- items, fields, variants and locals-free symbols it defines); fan_in_units the distinct
-- units outside the scope holding one, reference_occurrences all those occurrences. Units
-- nobody outside references have no row.
-- A namespace counts only the references to its own symbol, not to what it contains (the
-- file and namespace levels of queries/coupling.sql sum a file's and a namespace's
-- contents): the occurrences its language reads as naming it (namespace_references,
-- lang/<language>.sql), taken by the definition its language names. A file unit that is
-- no namespace (a Go file) counts none.
CREATE OR REPLACE VIEW namespace_references AS
SELECT * FROM rust_namespace_references
UNION ALL
SELECT * FROM go_namespace_references;

CREATE OR REPLACE VIEW unit_fan_in AS
WITH unit_rows AS MATERIALIZED (
    SELECT u.unit_id, u.file_id, u.package, u.kind, u.symbol, u.name_line, n.namespace
    FROM units u LEFT JOIN unit_namespace n USING (unit_id)
),
owned AS MATERIALIZED (
    SELECT DISTINCT s.unit_id, su.symbol
    FROM unit_scope s
    JOIN unit_rows u ON u.unit_id = s.unit_id AND u.kind NOT IN ('namespace', 'file')
    JOIN symbol_units su ON su.unit_id = s.member_unit_id
),
occurrences AS MATERIALIZED (
    SELECT symbol, file_id, start_line, start_column, unit_id
    FROM unit_occurrences WHERE NOT is_definition AND NOT is_local AND unit_id IS NOT NULL
),
-- Every reference with the unit it counts for: an item's scope, or the namespace unit
-- whose own symbol it names.
referencing AS (
    SELECT w.unit_id, o.file_id, o.unit_id AS ref_unit_id
    FROM owned w JOIN occurrences o USING (symbol)
    UNION ALL
    SELECT u.unit_id, o.file_id, o.unit_id
    FROM namespace_references r
    JOIN unit_rows u ON u.kind = 'namespace' AND u.file_id = r.owner_file_id AND u.symbol = r.symbol
    JOIN occurrences o ON o.file_id = r.file_id AND o.symbol = r.symbol
         AND o.start_line = r.start_line AND o.start_column = r.start_column
),
scopes AS MATERIALIZED (
    SELECT * FROM unit_scope
)
SELECT a.unit_id,
       count(DISTINCT a.file_id) FILTER (WHERE a.file_id <> u.file_id) AS fan_in_files,
       count(DISTINCT r.namespace) FILTER (WHERE r.namespace IS DISTINCT FROM u.namespace) AS fan_in_namespaces,
       count(DISTINCT r.package) FILTER (WHERE r.package <> u.package) AS fan_in_packages,
       count(DISTINCT a.ref_unit_id) AS fan_in_units,
       count(*) AS reference_occurrences
FROM referencing a
JOIN unit_rows u ON u.unit_id = a.unit_id
JOIN unit_rows r ON r.unit_id = a.ref_unit_id
WHERE NOT EXISTS (SELECT 1 FROM scopes s WHERE s.unit_id = a.unit_id AND s.member_unit_id = a.ref_unit_id)
GROUP BY a.unit_id;

-- unit_fan_out(unit_id, fan_out_files, fan_out_namespaces, fan_out_packages, fan_out_units):
-- the files other than the unit's own, the namespaces and the packages other than its own,
-- and the units outside its scope
-- that the unit's scope references (unit_edges from a member to a unit outside the scope).
CREATE OR REPLACE VIEW unit_fan_out AS
WITH unit_rows AS MATERIALIZED (
    SELECT u.unit_id, u.file_id, u.package, n.namespace
    FROM units u LEFT JOIN unit_namespace n USING (unit_id)
),
scopes AS MATERIALIZED (
    SELECT * FROM unit_scope
),
edges AS MATERIALIZED (
    SELECT src_unit_id, dst_unit_id FROM unit_edges WHERE NOT is_self
)
SELECT s.unit_id,
       count(DISTINCT d.file_id) FILTER (WHERE d.file_id <> u.file_id) AS fan_out_files,
       count(DISTINCT d.namespace) FILTER (WHERE d.namespace IS DISTINCT FROM u.namespace) AS fan_out_namespaces,
       count(DISTINCT d.package) FILTER (WHERE d.package <> u.package) AS fan_out_packages,
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
-- 'implementation' (a member implementing an interface's member, member_implementations:
-- calls through the interface resolve to its item), 'entry_point' or 'root' (the unit's
-- entry, views/units.sql: an entry function such as main, a build target's or a program's
-- root), 'interface_item' (an interface's own item, which its implementations implement;
-- calls on a concrete type resolve to the implementation); else 'unreferenced'.
-- Other entry points (callbacks registered by name, FFI exports, derive-generated uses,
-- macro bodies, which SCIP does not resolve) show as 'unreferenced'.
CREATE OR REPLACE VIEW unit_reference_status AS
WITH counts AS (
    SELECT symbol, count(*) AS n FROM scip_occurrences WHERE is_reference GROUP BY symbol
)
SELECT u.unit_id, coalesce(c.n, 0) AS references,
       CASE WHEN coalesce(c.n, 0) > 0 THEN 'referenced'
            WHEN u.is_test THEN 'test'
            WHEN u.unit_id IN (SELECT unit_id FROM member_implementations) THEN 'implementation'
            WHEN u.entry IS NOT NULL THEN u.entry
            WHEN u.member_of = 'interface_member' THEN 'interface_item'
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
-- side whose innermost unit is this one (clone_sides), and those each report found (a
-- pair both found counts in both).
CREATE OR REPLACE VIEW unit_clones AS
SELECT s.unit_id, count(DISTINCT s.pair_id) AS clone_pairs,
       count(DISTINCT s.pair_id) FILTER (WHERE list_contains(p.sources, 'exact')) AS exact_pairs,
       count(DISTINCT s.pair_id) FILTER (WHERE list_contains(p.sources, 'normalized')) AS normalized_pairs
FROM clone_sides s JOIN clone_pairs p USING (pair_id)
WHERE s.unit_id IS NOT NULL
GROUP BY s.unit_id;

-- unit_own_ast: the nodes of unit_ast that belong to the unit itself and not to a unit
-- nested in it: a node whose file lines lie within the lines of a unit the unit contains
-- (parent_unit_id) is that unit's, as unit_occurrences attributes positions to the
-- innermost unit. Nodes that span a nested unit (the enclosing block, the root) stay.
CREATE OR REPLACE VIEW unit_own_ast AS
WITH nested AS MATERIALIZED (
    SELECT parent_unit_id AS unit_id, start_line, end_line FROM units WHERE parent_unit_id IS NOT NULL
)
SELECT a.*
FROM unit_ast a
WHERE NOT EXISTS (SELECT 1 FROM nested n
                  WHERE n.unit_id = a.unit_id AND n.start_line <= a.file_start_line AND a.file_end_line <= n.end_line);

-- control_nodes(file_id, node_id, construct, decisions, opens_level, group_id): every
-- language's control constructs (lang/<language>.sql, <language>_control_nodes), each
-- named in its language's words, with its McCabe weight, whether it opens a nesting level,
-- and its group of alternatives. The language files' comments have the rules.
CREATE OR REPLACE VIEW control_nodes AS
SELECT * FROM rust_control_nodes
UNION ALL
SELECT * FROM go_control_nodes;

-- unit_constructs(unit_id, construct, count): how many of each control construct each
-- unit's own nodes (unit_own_ast) hold, in its language's words (Rust's match and arm,
-- Go's switch and case); a construct a unit holds none of has no row. Closure bodies are
-- part of the unit that writes them.
--
-- unit_complexity(unit_id, decisions, max_nesting): one row per unit.
--   decisions: McCabe's decision points, the constructs' weights summed less one per
--     group of alternatives the unit holds (a Rust match's arms but one decide), so that a
--     function's cyclomatic number is decisions + 1.
--   max_nesting: the most constructs that open a level open at one node, counted along the
--     ancestors (node_id ranges); 0 when the unit has none.
CREATE OR REPLACE VIEW unit_constructs AS
SELECT o.unit_id, c.construct, count(*) AS count
FROM unit_own_ast o
JOIN units u USING (unit_id)
JOIN control_nodes c ON c.file_id = u.file_id AND c.node_id = o.node_id
GROUP BY o.unit_id, c.construct;

CREATE OR REPLACE VIEW unit_complexity AS
WITH own AS MATERIALIZED (
    SELECT o.unit_id, o.node_id, o.descendant_count, c.decisions, c.opens_level, c.group_id
    FROM unit_own_ast o
    JOIN units u USING (unit_id)
    JOIN control_nodes c ON c.file_id = u.file_id AND c.node_id = o.node_id
),
counts AS (
    SELECT unit_id, sum(decisions) - count(DISTINCT group_id) AS decisions FROM own GROUP BY unit_id
),
levels AS MATERIALIZED (
    SELECT unit_id, node_id, descendant_count FROM own WHERE opens_level
),
nesting AS (
    SELECT unit_id, max(open_levels) AS max_nesting
    FROM (
        SELECT c.unit_id, c.node_id, count(*) AS open_levels
        FROM levels c JOIN levels a ON a.unit_id = c.unit_id
             AND a.node_id <= c.node_id AND c.node_id <= a.node_id + a.descendant_count
        GROUP BY c.unit_id, c.node_id
    )
    GROUP BY unit_id
)
SELECT u.unit_id, coalesce(c.decisions, 0)::BIGINT AS decisions, coalesce(n.max_nesting, 0) AS max_nesting
FROM units u
LEFT JOIN counts c USING (unit_id)
LEFT JOIN nesting n USING (unit_id);

-- unit_quality: one row per unit with the per-unit columns of every axis, side by side and
-- not combined (no score):
--   identity: unit_id, path, package, namespace, kind, qualified_name, start_line, end_line,
--     is_test;
--   size: loc;
--   complexity: decisions, max_nesting (unit_complexity);
--   duplication: clone_pairs, exact_pairs, normalized_pairs;
--   coupling: fan_in_files, fan_in_namespaces, fan_in_packages, fan_out_files,
--     fan_out_namespaces, fan_out_packages;
--   independence: cycle (the namespace cycle its namespace is in, namespace_cycle_edges), layer;
--   interface: visibility, parameters;
--   intent: has_doc, debt_lines;
--   unreferenced: references, reference_status.
CREATE OR REPLACE VIEW unit_quality AS
WITH cycle_members AS MATERIALIZED (
    SELECT DISTINCT src_namespace AS namespace, cycle FROM namespace_cycle_edges
)
SELECT u.unit_id, u.path, u.package, m.namespace, u.kind, u.qualified_name, u.start_line, u.end_line, u.is_test,
       u.loc,
       x.decisions, x.max_nesting,
       coalesce(c.clone_pairs, 0) AS clone_pairs, coalesce(c.exact_pairs, 0) AS exact_pairs,
       coalesce(c.normalized_pairs, 0) AS normalized_pairs,
       coalesce(fi.fan_in_files, 0) AS fan_in_files, coalesce(fi.fan_in_namespaces, 0) AS fan_in_namespaces,
       coalesce(fi.fan_in_packages, 0) AS fan_in_packages,
       coalesce(fo.fan_out_files, 0) AS fan_out_files, coalesce(fo.fan_out_namespaces, 0) AS fan_out_namespaces,
       coalesce(fo.fan_out_packages, 0) AS fan_out_packages,
       cm.cycle, l.layer,
       u.visibility, u.parameters,
       u.has_doc, coalesce(d.debt_lines, 0) AS debt_lines,
       r.references, r.status AS reference_status
FROM units u
LEFT JOIN unit_namespace m USING (unit_id)
LEFT JOIN unit_complexity x USING (unit_id)
LEFT JOIN unit_clones c USING (unit_id)
LEFT JOIN unit_fan_in fi USING (unit_id)
LEFT JOIN unit_fan_out fo USING (unit_id)
LEFT JOIN cycle_members cm ON cm.namespace = m.namespace
LEFT JOIN namespace_layers l ON l.namespace = m.namespace
LEFT JOIN unit_debt_markers d USING (unit_id)
LEFT JOIN unit_reference_status r USING (unit_id);
