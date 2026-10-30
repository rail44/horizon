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
-- A module counts only the references to its own symbol, not to what it contains (the
-- file level of queries/coupling.sql sums a file's contents): the path segments that name
-- it (`a` in `crate::a::f`, `use crate::a;`, an extern crate's name, an alias of it), and
-- not the occurrences rust-analyzer also gives its symbol that name no module: the
-- keywords `crate`, `super` and `self` (in a path and in `pub(crate)`, `pub(super)`), a
-- segment of a `pub(in …)` path, and the `mod a;` declaration. These are told apart by
-- the occurrence's source text. The target roots of one package share its crate root's
-- symbol (rust-analyzer names a symbol by package): the library's root (a path ending in
-- `src/lib.rs`), which is what a crate name in a path reaches, counts those references
-- when the package has one, else every root sharing the symbol does.
CREATE OR REPLACE VIEW unit_fan_in AS
WITH unit_rows AS MATERIALIZED (
    SELECT unit_id, file_id, crate, kind, symbol, path FROM units
),
module_symbols AS MATERIALIZED (
    SELECT DISTINCT symbol FROM unit_rows WHERE kind = 'module'
),
module_owners AS (
    SELECT unit_id, symbol FROM unit_rows
    WHERE kind = 'module'
    QUALIFY ends_with(path, 'src/lib.rs')
         OR NOT bool_or(ends_with(path, 'src/lib.rs')) OVER (PARTITION BY symbol)
),
owned AS MATERIALIZED (
    SELECT DISTINCT s.unit_id, su.symbol
    FROM unit_scope s
    JOIN unit_rows u ON u.unit_id = s.unit_id AND u.kind <> 'module'
    JOIN symbol_units su ON su.unit_id = s.member_unit_id
    UNION
    SELECT unit_id, symbol FROM module_owners
),
-- The occurrences of module symbols that name the module.
module_refs AS MATERIALIZED (
    SELECT o.symbol, o.file_id, o.unit_id
    FROM unit_occurrences o
    JOIN module_symbols m USING (symbol)
    JOIN source_lines l ON l.file_id = o.file_id AND l.line = o.start_line
    WHERE NOT o.is_definition
      AND substr(l.text, o.start_column, o.end_column - o.start_column) NOT IN ('crate', 'super', 'self')
      AND NOT regexp_matches(substr(l.text, 1, o.start_column - 1), 'pub\(in [\w:]*$')
      AND NOT regexp_matches(l.text, '^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*;')
),
refs AS MATERIALIZED (
    SELECT o.symbol, o.file_id, o.unit_id, r.crate
    FROM unit_occurrences o JOIN unit_rows r USING (unit_id)
    WHERE NOT o.is_definition AND NOT o.is_local AND o.symbol NOT IN (SELECT symbol FROM module_symbols)
    UNION ALL
    SELECT m.symbol, m.file_id, m.unit_id, r.crate
    FROM module_refs m JOIN unit_rows r USING (unit_id)
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

-- unit_complexity(unit_id, if_count, else_if_count, match_count, arm_count, guard_count,
--     for_count, while_count, loop_count, try_count, and_count, or_count, let_else_count,
--     return_count, break_count, continue_count, closure_count, decisions, max_nesting):
-- counts over each unit's own nodes (unit_own_ast), one row per unit. Closure bodies are
-- part of the unit that writes them; macro arguments are tokens and count nothing.
--   if_count: if_expression nodes, `if let` and every `else if` included; else_if_count:
--     those whose parent is an else_clause.
--   match_count: match_expression; arm_count: match_arm; guard_count: arms whose
--     match_pattern has an `if` token child.
--   for_count, while_count (`while let` included), loop_count: for_expression,
--     while_expression, loop_expression.
--   try_count: try_expression (`?`).
--   and_count, or_count: `&&` and `||` tokens whose parent is a binary_expression or a
--     let_chain (`if let … && …`).
--   let_else_count: let_declaration nodes with an `else` token child.
--   return_count, break_count, continue_count: return_, break_, continue_expression.
--   closure_count: closure_expression.
--   decisions: McCabe's decision points, so that a function's cyclomatic number is
--     decisions + 1: each if (else if included), while and for; each match's arms but one;
--     each guard, `&&`, `||`, `?` and let-else. A bare `loop` is not a decision (its exits
--     are breaks), as rust-code-analysis issue #1249 expects from McCabe; a closure adds
--     its decisions but no entry of its own.
--   max_nesting: the most control constructs open at one node: if (an `else if` continues
--     its chain and opens no level), match, for, while and loop, counted along the
--     ancestors (node_id ranges); 0 when the unit has none. Closures and plain blocks open
--     no level.
CREATE OR REPLACE VIEW unit_complexity AS
WITH own AS MATERIALIZED (
    SELECT unit_id, node_id, type, parent_id, descendant_count FROM unit_own_ast
),
typed AS MATERIALIZED (
    SELECT o.unit_id, o.node_id, o.type, o.descendant_count, p.type AS parent_type
    FROM own o LEFT JOIN own p ON p.unit_id = o.unit_id AND p.node_id = o.parent_id
    WHERE o.type IN ('if_expression', 'match_expression', 'match_arm', 'for_expression', 'while_expression',
                     'loop_expression', 'try_expression', 'return_expression', 'break_expression',
                     'continue_expression', 'closure_expression', 'if', 'else', '&&', '||')
),
counts AS (
    SELECT unit_id,
           count(*) FILTER (WHERE type = 'if_expression') AS if_count,
           count(*) FILTER (WHERE type = 'if_expression' AND parent_type = 'else_clause') AS else_if_count,
           count(*) FILTER (WHERE type = 'match_expression') AS match_count,
           count(*) FILTER (WHERE type = 'match_arm') AS arm_count,
           count(*) FILTER (WHERE type = 'if' AND parent_type = 'match_pattern') AS guard_count,
           count(*) FILTER (WHERE type = 'for_expression') AS for_count,
           count(*) FILTER (WHERE type = 'while_expression') AS while_count,
           count(*) FILTER (WHERE type = 'loop_expression') AS loop_count,
           count(*) FILTER (WHERE type = 'try_expression') AS try_count,
           count(*) FILTER (WHERE type = '&&' AND parent_type IN ('binary_expression', 'let_chain')) AS and_count,
           count(*) FILTER (WHERE type = '||' AND parent_type IN ('binary_expression', 'let_chain')) AS or_count,
           count(*) FILTER (WHERE type = 'else' AND parent_type = 'let_declaration') AS let_else_count,
           count(*) FILTER (WHERE type = 'return_expression') AS return_count,
           count(*) FILTER (WHERE type = 'break_expression') AS break_count,
           count(*) FILTER (WHERE type = 'continue_expression') AS continue_count,
           count(*) FILTER (WHERE type = 'closure_expression') AS closure_count
    FROM typed GROUP BY unit_id
),
-- Matches with at least one arm: the match_block parents of the arms.
matches AS (
    SELECT o.unit_id, count(DISTINCT o.parent_id) AS armed_matches
    FROM own o WHERE o.type = 'match_arm' GROUP BY o.unit_id
),
levels AS MATERIALIZED (
    SELECT unit_id, node_id, descendant_count FROM typed
    WHERE type IN ('match_expression', 'for_expression', 'while_expression', 'loop_expression')
       OR (type = 'if_expression' AND parent_type IS DISTINCT FROM 'else_clause')
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
SELECT u.unit_id,
       coalesce(c.if_count, 0) AS if_count, coalesce(c.else_if_count, 0) AS else_if_count,
       coalesce(c.match_count, 0) AS match_count, coalesce(c.arm_count, 0) AS arm_count,
       coalesce(c.guard_count, 0) AS guard_count, coalesce(c.for_count, 0) AS for_count,
       coalesce(c.while_count, 0) AS while_count, coalesce(c.loop_count, 0) AS loop_count,
       coalesce(c.try_count, 0) AS try_count, coalesce(c.and_count, 0) AS and_count,
       coalesce(c.or_count, 0) AS or_count, coalesce(c.let_else_count, 0) AS let_else_count,
       coalesce(c.return_count, 0) AS return_count, coalesce(c.break_count, 0) AS break_count,
       coalesce(c.continue_count, 0) AS continue_count, coalesce(c.closure_count, 0) AS closure_count,
       (coalesce(c.if_count, 0) + coalesce(c.while_count, 0) + coalesce(c.for_count, 0)
        + coalesce(c.arm_count, 0) - coalesce(m.armed_matches, 0) + coalesce(c.guard_count, 0)
        + coalesce(c.and_count, 0) + coalesce(c.or_count, 0) + coalesce(c.try_count, 0)
        + coalesce(c.let_else_count, 0)) AS decisions,
       coalesce(n.max_nesting, 0) AS max_nesting
FROM units u
LEFT JOIN counts c USING (unit_id)
LEFT JOIN matches m USING (unit_id)
LEFT JOIN nesting n USING (unit_id);

-- unit_quality: one row per unit with the per-unit columns of every axis, side by side and
-- not combined (no score):
--   identity: unit_id, path, crate, module, kind, qualified_name, start_line, end_line,
--     is_test;
--   size: loc;
--   complexity: decisions, max_nesting (unit_complexity);
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
       x.decisions, x.max_nesting,
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
LEFT JOIN unit_complexity x USING (unit_id)
LEFT JOIN unit_clones c USING (unit_id)
LEFT JOIN unit_fan_in fi USING (unit_id)
LEFT JOIN unit_fan_out fo USING (unit_id)
LEFT JOIN cycle_members cm ON cm.module = m.module
LEFT JOIN module_layers l ON l.module = m.module
LEFT JOIN unit_debt_markers d USING (unit_id)
LEFT JOIN unit_reference_status r USING (unit_id);
