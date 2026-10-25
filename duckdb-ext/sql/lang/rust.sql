-- Rust units and tags as views over `nodes` (sitting_duck's read_ast) and `sources`,
-- and the units of historical blobs over the same rules.
--
-- Ported from the code database spike's Rust adapter (its tree-sitter queries
-- restated in SQL; the restatement reproduced the spike's units and tags exactly,
-- docs/research/sitting-duck-evaluation-2026-10-24.md §5). Lines are 1-based and
-- inclusive.
--
-- Outputs:
--   rust_units(unit_id, file_id, node_id, parent_unit_id, kind, name, qualified_name,
--              start_line, end_line, loc)
--     kind: function, method, impl, type, module, macro. qualified_name joins the
--     names of the enclosing units, outermost first, with `.`; parent_unit_id is the
--     innermost enclosing unit; loc counts non-blank lines.
--   rust_blob_units: the columns of rust_units for the blobs of views/blobs.sql, with
--     file_id holding the blob's position in the blob snapshot (views/units.sql maps it
--     back to the blob id).
--   rust_tags(file_id, node_id, tag, start_line, end_line)
--     tag: test, trait_impl, generated_or_macro; one row per distinct (node, tag), with
--     the tagged node's lines. File-level tags sit on the root node (node_id 0).
--     Inheritance is not materialized: a range is tagged when a tagged node of the
--     same file contains it.
--
-- One scan per view: each view reads `nodes` once, into the MATERIALIZED common table
-- expression `rust_nodes`, and builds its intermediate steps as further expressions in
-- the same statement. A view (or an expression that is not materialized) is expanded
-- at every reference, and every expansion of `nodes` runs read_ast over all files
-- again (docs/research/code-db-views-2026-10-24.md §6). Intermediate steps that are
-- referenced more than once, or that slice source text, are MATERIALIZED as well.
--
-- The unit rules are the table macro rust_units_over(nodes_t, bytes_t, lines_t), whose
-- arguments name the node, byte and line relations (columns of scope_nodes,
-- source_bytes and source_lines, keyed by file_id); query_table reads them. rust_units
-- applies it to the current files, rust_blob_units to the blobs, so
-- the two sets of units come from one text. Tags have no blob counterpart yet.
--
-- Both views over the current files cover the files of `scope_files`
-- (views/files.sql): all files, or the subset refresh.sql recomputes. Every input
-- (scope_nodes, sources, file paths) is restricted to that set, so the rows of a file
-- depend on that file alone; the blob inputs cover the blob snapshot of
-- views/blobs.sql, which refresh.sql narrows the same way. unit_id numbers the units of the scope, ordered by file_id first, so a file's
-- units are consecutive. The node filter (views/nodes.sql, scope_nodes) tests
-- `scope_paths IS NULL` first: with the bare `file_id IN (…)`, a live `units` or `tags`
-- peaked about 200 MB higher on this repository; with the test first, at the same
-- memory as without any filter.
--
-- read_ast exposes no tree-sitter field names and no is_named flag, so field-based
-- patterns are restated by child type and sibling position (see the comments below).

CREATE OR REPLACE MACRO rust_units_over(nodes_t, bytes_t, lines_t) AS TABLE
WITH rust_nodes AS MATERIALIZED (
    SELECT file_id, node_id, parent_id, type,
           -- read_ast's positions are UINTEGER; widen them before any subtraction.
           start_byte::BIGINT AS start_byte, end_byte::BIGINT AS end_byte,
           start_line::BIGINT AS start_line, end_line::BIGINT AS end_line,
           sibling_index, descendant_count::BIGINT AS descendant_count
    FROM query_table(nodes_t) WHERE language = 'rust'
),
rust_unit_candidates AS MATERIALIZED (
    SELECT n.file_id, n.node_id, n.type, n.start_byte, n.end_byte, n.start_line, n.end_line,
           n.descendant_count,
           CASE
               -- (impl_item|trait_item body: (declaration_list (function_item ...) @unit.method))
               WHEN n.type = 'function_item' AND p.type = 'declaration_list'
                    AND g.type IN ('impl_item', 'trait_item') THEN 'method'
               -- (trait_item body: (declaration_list (function_signature_item ...) @unit.method))
               WHEN n.type = 'function_signature_item' AND p.type = 'declaration_list'
                    AND g.type = 'trait_item' THEN 'method'
               WHEN n.type = 'function_item' THEN 'function'
               WHEN n.type = 'impl_item' THEN 'impl'
               WHEN n.type IN ('struct_item', 'enum_item', 'union_item', 'type_item', 'trait_item') THEN 'type'
               WHEN n.type = 'mod_item' THEN 'module'
               WHEN n.type = 'macro_definition' THEN 'macro'
           END AS kind
    FROM rust_nodes n
    LEFT JOIN rust_nodes p ON p.file_id = n.file_id AND p.node_id = n.parent_id
    LEFT JOIN rust_nodes g ON g.file_id = p.file_id AND g.node_id = p.parent_id
    WHERE n.type IN ('function_item', 'function_signature_item', 'impl_item', 'struct_item', 'enum_item',
                     'union_item', 'type_item', 'trait_item', 'mod_item', 'macro_definition')
),
-- Name nodes. A field like `name: (identifier)` becomes "the first direct child of that
-- type": the other direct children of these items never have that type.
-- For impl_item, `trait:` and `type:` are the non-comment children right before and right
-- after the anonymous `for` token; without `for`, `type:` is the first child after `impl`
-- that is not type_parameters.
children AS MATERIALIZED (
    SELECT u.file_id, u.node_id AS unit_node, u.type AS unit_type, u.kind,
           c.node_id, c.type, c.sibling_index, c.start_byte, c.end_byte
    FROM rust_unit_candidates u
    JOIN rust_nodes c ON c.file_id = u.file_id AND c.parent_id = u.node_id
    WHERE u.kind IS NOT NULL
),
markers AS (
    SELECT file_id, unit_node,
           min(sibling_index) FILTER (WHERE type = 'for') AS for_idx,
           min(sibling_index) FILTER (WHERE type = 'impl') AS impl_idx
    FROM children WHERE unit_type = 'impl_item' GROUP BY ALL
),
simple AS (
    SELECT file_id, unit_node, arg_min(node_id, sibling_index) AS node_id
    FROM children
    WHERE (unit_type IN ('function_item', 'function_signature_item', 'mod_item', 'macro_definition')
           AND type = 'identifier')
       OR (unit_type IN ('struct_item', 'enum_item', 'union_item', 'type_item', 'trait_item')
           AND type = 'type_identifier')
    GROUP BY ALL
),
impl_trait AS (
    SELECT c.file_id, c.unit_node, arg_max(c.node_id, c.sibling_index) AS node_id
    FROM children c JOIN markers m USING (file_id, unit_node)
    WHERE m.for_idx IS NOT NULL AND c.sibling_index < m.for_idx
      AND c.type NOT IN ('line_comment', 'block_comment')
    GROUP BY ALL
),
impl_type AS (
    SELECT c.file_id, c.unit_node, arg_min(c.node_id, c.sibling_index) AS node_id
    FROM children c JOIN markers m USING (file_id, unit_node)
    WHERE c.type NOT IN ('line_comment', 'block_comment')
      AND CASE WHEN m.for_idx IS NOT NULL THEN c.sibling_index > m.for_idx
               ELSE c.sibling_index > m.impl_idx AND c.type <> 'type_parameters' END
    GROUP BY ALL
),
rust_unit_name_nodes AS (
    SELECT file_id, unit_node, node_id FROM simple
    UNION ALL SELECT file_id, unit_node, node_id FROM impl_trait
    UNION ALL SELECT file_id, unit_node, node_id FROM impl_type
),
-- Source text is sliced in a step of its own, joining the file bytes only to the
-- materialized spans: in a join of several inputs the planner may join the bytes to all
-- nodes of a file first (one row per node carrying the file's bytes: a draft of this view
-- peaked at 7 GB live and at 44 GB over a node table in this repository).
rust_unit_name_spans AS MATERIALIZED (
    SELECT x.file_id, x.unit_node, nn.start_byte, nn.end_byte
    FROM rust_unit_name_nodes x
    JOIN rust_nodes nn ON nn.file_id = x.file_id AND nn.node_id = x.node_id
),
rust_unit_names AS MATERIALIZED (
    SELECT x.file_id, x.unit_node, x.start_byte,
           normalize_ws(byte_slice(b.bytes, x.start_byte, x.end_byte)) AS text
    FROM rust_unit_name_spans x
    JOIN query_table(bytes_t) b ON b.file_id = x.file_id
),
rust_units_unnested AS MATERIALIZED (
    SELECT u.file_id, u.node_id, u.kind, u.start_byte, u.end_byte, u.start_line, u.end_line,
           u.descendant_count,
           -- Several name nodes join in source order with " for " (impls: `Trait for Type`).
           string_agg(x.text, ' for ' ORDER BY x.start_byte) AS name
    FROM rust_unit_candidates u
    JOIN rust_unit_names x ON x.file_id = u.file_id AND x.unit_node = u.node_id
    WHERE u.kind IS NOT NULL
    GROUP BY ALL
),
-- Enclosing units are the unit ancestors in the tree: preorder ids make "a contains u"
-- the interval test a.node_id < u.node_id <= a.node_id + a.descendant_count.
nested AS (
    SELECT u.file_id, u.node_id, u.kind, u.name, u.start_byte, u.end_byte, u.start_line, u.end_line,
           max(a.node_id) FILTER (WHERE a.node_id < u.node_id) AS parent_node_id,
           string_agg(a.name, '.' ORDER BY a.node_id) AS qualified_name
    FROM rust_units_unnested u
    JOIN rust_units_unnested a ON a.file_id = u.file_id
         AND a.node_id <= u.node_id AND u.node_id <= a.node_id + a.descendant_count
    GROUP BY ALL
),
numbered AS MATERIALIZED (
    SELECT row_number() OVER (ORDER BY n.file_id, n.start_byte, -n.end_byte) - 1 AS unit_id,
           n.*
    FROM nested n
),
lines AS MATERIALIZED (
    SELECT * FROM query_table(lines_t)
)
SELECT n.unit_id, n.file_id, n.node_id, p.unit_id AS parent_unit_id, n.kind, n.name,
       n.qualified_name, n.start_line, n.end_line,
       e.nonblank_through - coalesce(s.nonblank_through, 0) AS loc
FROM numbered n
LEFT JOIN numbered p ON p.file_id = n.file_id AND p.node_id = n.parent_node_id
JOIN lines e ON e.file_id = n.file_id AND e.line = n.end_line
LEFT JOIN lines s ON s.file_id = n.file_id AND s.line = n.start_line - 1;

CREATE OR REPLACE VIEW rust_units AS
FROM rust_units_over('scope_nodes', 'source_bytes', 'source_lines');

CREATE OR REPLACE VIEW rust_blob_units AS
FROM rust_units_over('blob_scope_nodes', 'blob_source_bytes', 'blob_source_lines');

CREATE OR REPLACE VIEW rust_tags AS
WITH rust_nodes AS MATERIALIZED (
    SELECT file_id, node_id, parent_id, type,
           start_byte::BIGINT AS start_byte, end_byte::BIGINT AS end_byte,
           start_line::BIGINT AS start_line, end_line::BIGINT AS end_line,
           sibling_index, children_count
    FROM scope_nodes WHERE language = 'rust'
),
-- Text of the identifier-like nodes the predicates compare (#eq?, #any-of?), sliced in a
-- step of its own as in rust_units.
rust_tag_ident_spans AS MATERIALIZED (
    SELECT n.file_id, n.node_id, n.parent_id, n.type, n.sibling_index, n.start_byte, n.end_byte
    FROM rust_nodes n
    JOIN rust_nodes p ON p.file_id = n.file_id AND p.node_id = n.parent_id
    WHERE n.type = 'identifier' AND p.type IN ('attribute', 'scoped_identifier', 'token_tree')
),
rust_tag_idents AS MATERIALIZED (
    SELECT n.file_id, n.node_id, n.parent_id, n.type, n.sibling_index,
           byte_slice(b.bytes, n.start_byte, n.end_byte) AS text
    FROM rust_tag_ident_spans n
    JOIN source_bytes b ON b.file_id = n.file_id
),
-- Named children of token_tree, numbered. Tree-sitter's `.` anchor counts named siblings
-- only; read_ast has no is_named, so it is approximated from the type name: punctuation
-- (any type that is not a lowercase identifier-like name) and the anonymous keyword
-- tokens listed below are dropped. A type name shared by a named node and an anonymous
-- token (block, lifetime, item) cannot be told apart this way.
rust_token_tree_named AS MATERIALIZED (
    SELECT c.file_id, c.parent_id AS tree_id, c.node_id, c.type,
           row_number() OVER (PARTITION BY c.file_id, c.parent_id ORDER BY c.sibling_index) AS pos,
           count(*) OVER (PARTITION BY c.file_id, c.parent_id) AS n
    FROM rust_nodes c
    JOIN rust_nodes t ON t.file_id = c.file_id AND t.node_id = c.parent_id AND t.type = 'token_tree'
    WHERE regexp_full_match(c.type, '[a-z_]+') AND c.type NOT IN (
        -- keyword tokens that are anonymous in tree-sitter-rust 0.24 (named keywords such as
        -- self/crate/super are kept)
        'as', 'async', 'await', 'break', 'const', 'continue', 'default', 'dyn', 'else', 'enum',
        'extern', 'fn', 'for', 'gen', 'if', 'impl', 'in', 'let', 'loop', 'macro_rules', 'match',
        'mod', 'move', 'pub', 'raw', 'ref', 'return', 'static', 'struct', 'trait', 'type', 'union',
        'unsafe', 'use', 'where', 'while', 'yield', 'true', 'false', 'u8', 'i8', 'u16', 'i16', 'u32',
        'i32', 'u64', 'i64', 'u128', 'i128', 'isize', 'usize', 'f32', 'f64', 'bool', 'str', 'char',
        -- `_` and the macro_rules fragment specifiers that do not collide with a named type
        '_', 'expr', 'expr_2021', 'ident', 'literal', 'meta', 'pat', 'pat_param', 'path', 'stmt',
        'tt', 'ty', 'vis')
),
-- cfg(test) argument trees: (token_tree . (identifier) "test" .)  or
--                           (token_tree . (identifier) "all" . (token_tree (identifier) "test"))
rust_cfg_test_trees AS (
    SELECT a.file_id, a.tree_id
    FROM rust_token_tree_named a
    JOIN rust_tag_idents i ON i.file_id = a.file_id AND i.node_id = a.node_id
    WHERE a.pos = 1 AND a.n = 1 AND i.text = 'test'
    UNION
    SELECT a.file_id, a.tree_id
    FROM rust_token_tree_named a
    JOIN rust_tag_idents i ON i.file_id = a.file_id AND i.node_id = a.node_id AND i.text = 'all'
    JOIN rust_token_tree_named b ON b.file_id = a.file_id AND b.tree_id = a.tree_id
         AND b.pos = a.pos + 1 AND b.type = 'token_tree'
    JOIN rust_tag_idents t ON t.file_id = b.file_id AND t.parent_id = b.node_id AND t.text = 'test'
    WHERE a.pos = 1
),
-- Attributes, classified: `test_like` for #[test] / #[x::test] / rstest / test_case / bench
-- (an identifier child of the attribute, or the last child of a scoped_identifier child);
-- `cfg_test` for cfg(...) whose argument tree is a cfg(test) tree.
rust_attribute_kinds AS MATERIALIZED (
    SELECT attr.file_id, attr.parent_id AS item_id, 'test_like' AS what
    FROM rust_nodes attr
    JOIN rust_tag_idents i ON i.file_id = attr.file_id
    WHERE attr.type = 'attribute'
      AND i.text IN ('test', 'rstest', 'test_case', 'bench')
      AND (i.parent_id = attr.node_id
           OR i.parent_id IN (SELECT s.node_id FROM rust_nodes s
                              WHERE s.file_id = attr.file_id AND s.parent_id = attr.node_id
                                AND s.type = 'scoped_identifier'
                                AND i.sibling_index = s.children_count - 1))
    UNION
    SELECT attr.file_id, attr.parent_id AS item_id, 'cfg_test' AS what
    FROM rust_nodes attr
    JOIN rust_tag_idents c ON c.file_id = attr.file_id AND c.parent_id = attr.node_id AND c.text = 'cfg'
    JOIN rust_cfg_test_trees t ON t.file_id = attr.file_id
    JOIN rust_nodes tt ON tt.file_id = t.file_id AND tt.node_id = t.tree_id AND tt.parent_id = attr.node_id
    WHERE attr.type = 'attribute'
),
-- `(attribute_item ...) . [(attribute_item) (line_comment) (block_comment)]* . (item) @tagged`:
-- for each item, the maximal run of attribute items and comments directly before it among
-- its named siblings; the item is tagged when one attribute item in that run qualifies.
sib AS MATERIALIZED (
    SELECT n.file_id, n.parent_id, n.node_id, n.type, n.sibling_index,
           -- index of the latest sibling (at or before this one) that breaks a run
           max(CASE WHEN n.type IN ('attribute_item', 'line_comment', 'block_comment') THEN NULL
                    ELSE n.sibling_index END)
               OVER (PARTITION BY n.file_id, n.parent_id ORDER BY n.sibling_index
                     ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS prev_break
    FROM rust_nodes n
    WHERE n.parent_id IS NOT NULL
      -- named siblings only: drop punctuation, the only anonymous tokens that sit next to
      -- items and attributes in their containers
      AND regexp_full_match(n.type, '[a-z_]+')
),
rust_attribute_runs AS (
    SELECT item.file_id, item.node_id AS item_node, item.type AS item_type, run.node_id AS attr_node
    FROM sib item
    JOIN sib run ON run.file_id = item.file_id AND run.parent_id = item.parent_id
         AND run.type = 'attribute_item'
         AND run.sibling_index < item.sibling_index
         AND run.sibling_index > coalesce(item.prev_break, -1)
    WHERE item.type NOT IN ('attribute_item', 'line_comment', 'block_comment')
),
tagged AS (
    SELECT DISTINCT r.file_id, r.item_node AS node_id, 'test' AS tag
    FROM rust_attribute_runs r
    JOIN rust_attribute_kinds k ON k.file_id = r.file_id AND k.item_id = r.attr_node
    WHERE (k.what = 'test_like' AND r.item_type = 'function_item')
       OR (k.what = 'cfg_test' AND r.item_type IN (
            'function_item', 'mod_item', 'impl_item', 'struct_item', 'enum_item', 'union_item',
            'trait_item', 'type_item', 'const_item', 'static_item', 'use_declaration',
            'macro_invocation', 'macro_definition'))
    UNION
    -- #![cfg(test)] as a direct child of source_file tags the root.
    SELECT DISTINCT r.file_id, r.node_id, 'test'
    FROM rust_nodes r
    JOIN rust_nodes ia ON ia.file_id = r.file_id AND ia.parent_id = r.node_id AND ia.type = 'inner_attribute_item'
    JOIN rust_nodes attr ON attr.file_id = ia.file_id AND attr.parent_id = ia.node_id AND attr.type = 'attribute'
    JOIN rust_attribute_kinds k ON k.file_id = attr.file_id AND k.item_id = ia.node_id AND k.what = 'cfg_test'
    WHERE r.type = 'source_file' AND r.parent_id IS NULL
    UNION
    -- File tags: the spike's fnmatch globs **/tests/**, **/tests.rs, **/*_tests.rs and
    -- **/test_*.rs, where `*` also matches `/` and `**/` also matches at the root.
    SELECT f.file_id, 0, 'test'
    FROM scope_files f
    WHERE f.language = 'rust' AND (
           f.path LIKE '%/tests/%' OR f.path LIKE 'tests/%'
        OR f.path LIKE '%/tests.rs' OR f.path = 'tests.rs'
        OR f.path LIKE '%\_tests.rs' ESCAPE '\'
        OR f.path LIKE '%/test\_%.rs' ESCAPE '\' OR f.path LIKE 'test\_%.rs' ESCAPE '\')
    UNION
    -- (impl_item trait: (_)): an impl with a direct `for` token.
    SELECT i.file_id, i.node_id, 'trait_impl'
    FROM rust_nodes i
    WHERE i.type = 'impl_item'
      AND EXISTS (SELECT 1 FROM rust_nodes f WHERE f.file_id = i.file_id AND f.parent_id = i.node_id AND f.type = 'for')
    UNION
    SELECT file_id, node_id, 'generated_or_macro'
    FROM rust_nodes WHERE type IN ('macro_invocation', 'macro_definition')
)
SELECT t.file_id, t.node_id, t.tag, n.start_line, n.end_line
FROM tagged t
JOIN rust_nodes n ON n.file_id = t.file_id AND n.node_id = t.node_id;

-- rust_impl_parts(file_id, node_id, part, start_line, start_column, head_end_line,
--                 head_end_column): the trait and the self type an impl names, for
-- views/graph.sql's impl_for and implements. node_id is the impl_item (the impl unit's
-- node). Part `trait` is the child right before the `for` token, part `type` the child
-- right after it, or without `for` the first child after `impl` that is not
-- type_parameters (the children rust_units_over names the impl by). The head of a part
-- is its span up to its first type_arguments descendant, or the whole part: the head's
-- last occurrence is the named trait or type (`From` in `From<Foo>`, `Baz` in
-- `a::b::Baz<T>` and in `&'a Baz`), not a type argument. head_end is exclusive: the
-- type arguments' start, else the part's end (1-based lines, byte columns).
CREATE OR REPLACE VIEW rust_impl_parts AS
WITH rust_nodes AS MATERIALIZED (
    SELECT file_id, node_id, parent_id, type, sibling_index,
           start_line::BIGINT AS start_line, start_column::BIGINT AS start_column,
           end_line::BIGINT AS end_line, end_column::BIGINT AS end_column,
           descendant_count::BIGINT AS descendant_count
    FROM scope_nodes WHERE language = 'rust'
),
children AS MATERIALIZED (
    SELECT c.file_id, i.node_id AS impl_node, c.node_id, c.type, c.sibling_index
    FROM rust_nodes i
    JOIN rust_nodes c ON c.file_id = i.file_id AND c.parent_id = i.node_id
    WHERE i.type = 'impl_item'
),
markers AS (
    SELECT file_id, impl_node,
           min(sibling_index) FILTER (WHERE type = 'for') AS for_idx,
           min(sibling_index) FILTER (WHERE type = 'impl') AS impl_idx
    FROM children GROUP BY ALL
),
parts AS MATERIALIZED (
    SELECT c.file_id, c.impl_node, 'trait' AS part, arg_max(c.node_id, c.sibling_index) AS part_node
    FROM children c JOIN markers m USING (file_id, impl_node)
    WHERE m.for_idx IS NOT NULL AND c.sibling_index < m.for_idx
      AND c.type NOT IN ('line_comment', 'block_comment')
    GROUP BY ALL
    UNION ALL
    SELECT c.file_id, c.impl_node, 'type', arg_min(c.node_id, c.sibling_index)
    FROM children c JOIN markers m USING (file_id, impl_node)
    WHERE c.type NOT IN ('line_comment', 'block_comment')
      AND CASE WHEN m.for_idx IS NOT NULL THEN c.sibling_index > m.for_idx
               ELSE c.sibling_index > m.impl_idx AND c.type <> 'type_parameters' END
    GROUP BY ALL
),
part_spans AS MATERIALIZED (
    SELECT p.file_id, p.impl_node, p.part, n.node_id, n.descendant_count,
           n.start_line, n.start_column, n.end_line, n.end_column
    FROM parts p JOIN rust_nodes n ON n.file_id = p.file_id AND n.node_id = p.part_node
),
type_arguments AS MATERIALIZED (
    SELECT file_id, node_id, start_line, start_column FROM rust_nodes WHERE type = 'type_arguments'
),
first_arguments AS (
    SELECT p.file_id, p.impl_node, p.part,
           arg_min(a.start_line, a.node_id) AS start_line, arg_min(a.start_column, a.node_id) AS start_column
    FROM part_spans p
    JOIN type_arguments a ON a.file_id = p.file_id
         AND a.node_id > p.node_id AND a.node_id <= p.node_id + p.descendant_count
    GROUP BY ALL
)
SELECT p.file_id, p.impl_node AS node_id, p.part, p.start_line, p.start_column,
       coalesce(a.start_line, p.end_line) AS head_end_line,
       coalesce(a.start_column, p.end_column) AS head_end_column
FROM part_spans p
LEFT JOIN first_arguments a USING (file_id, impl_node, part);
