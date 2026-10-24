-- Prototype: the spike's Rust tag queries and file tags (languages.py, RUST.tag_queries and
-- RUST.file_tags) as SQL views over sitting_duck's read_ast output. Not wired into build.py.
--
-- Inputs: sd_nodes and sd_source as described in rust_units.sql; run rust_units.sql first
-- (it defines sd_slice and sd_source_hex).
--
-- Output: rust_tags(file_path, node_id, tag), one row per distinct (node, tag) like the
-- spike's `tags`. File-level tags sit on the root node (node_id 0).

-- Text of the identifier-like nodes the predicates compare (#eq?, #any-of?).
CREATE OR REPLACE TEMP TABLE rust_tag_idents AS
SELECT n.file_path, n.node_id, n.parent_id, n.type, n.sibling_index,
       sd_slice(h.hex, n.start_byte, n.end_byte) AS text
FROM sd_nodes n
JOIN sd_nodes p ON p.file_path = n.file_path AND p.node_id = n.parent_id
JOIN sd_source_hex h ON h.file_path = n.file_path
WHERE n.type = 'identifier' AND p.type IN ('attribute', 'scoped_identifier', 'token_tree');

-- Named children of token_tree, numbered. Tree-sitter's `.` anchor counts named siblings
-- only; read_ast has no is_named, so it is approximated from the type name: punctuation
-- (any type that is not a lowercase identifier-like name) and the anonymous keyword
-- tokens listed below are dropped. A type name shared by a named node and an anonymous
-- token (block, lifetime, item) cannot be told apart this way.
CREATE OR REPLACE VIEW rust_token_tree_named AS
SELECT c.file_path, c.parent_id AS tree_id, c.node_id, c.type,
       row_number() OVER (PARTITION BY c.file_path, c.parent_id ORDER BY c.sibling_index) AS pos,
       count(*) OVER (PARTITION BY c.file_path, c.parent_id) AS n
FROM sd_nodes c
JOIN sd_nodes t ON t.file_path = c.file_path AND t.node_id = c.parent_id AND t.type = 'token_tree'
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
    'tt', 'ty', 'vis');

-- RUST_CFG_TEST: (token_tree . (identifier) "test" .)  or
--                (token_tree . (identifier) "all" . (token_tree (identifier) "test"))
CREATE OR REPLACE VIEW rust_cfg_test_trees AS
SELECT a.file_path, a.tree_id
FROM rust_token_tree_named a
JOIN rust_tag_idents i ON i.file_path = a.file_path AND i.node_id = a.node_id
WHERE a.pos = 1 AND a.n = 1 AND i.text = 'test'
UNION
SELECT a.file_path, a.tree_id
FROM rust_token_tree_named a
JOIN rust_tag_idents i ON i.file_path = a.file_path AND i.node_id = a.node_id AND i.text = 'all'
JOIN rust_token_tree_named b ON b.file_path = a.file_path AND b.tree_id = a.tree_id
     AND b.pos = a.pos + 1 AND b.type = 'token_tree'
JOIN rust_tag_idents t ON t.file_path = b.file_path AND t.parent_id = b.node_id AND t.text = 'test'
WHERE a.pos = 1;

-- Attributes, classified: `test_like` for #[test] / #[x::test] / rstest / test_case / bench
-- (an identifier child of the attribute, or the last child of a scoped_identifier child);
-- `cfg_test` for cfg(...) whose argument tree matches RUST_CFG_TEST.
CREATE OR REPLACE VIEW rust_attribute_kinds AS
SELECT attr.file_path, attr.parent_id AS item_id, 'test_like' AS what
FROM sd_nodes attr
JOIN rust_tag_idents i ON i.file_path = attr.file_path
WHERE attr.type = 'attribute'
  AND i.text IN ('test', 'rstest', 'test_case', 'bench')
  AND (i.parent_id = attr.node_id
       OR i.parent_id IN (SELECT s.node_id FROM sd_nodes s
                          WHERE s.file_path = attr.file_path AND s.parent_id = attr.node_id
                            AND s.type = 'scoped_identifier'
                            AND i.sibling_index = s.children_count - 1))
UNION
SELECT attr.file_path, attr.parent_id AS item_id, 'cfg_test' AS what
FROM sd_nodes attr
JOIN rust_tag_idents c ON c.file_path = attr.file_path AND c.parent_id = attr.node_id AND c.text = 'cfg'
JOIN rust_cfg_test_trees t ON t.file_path = attr.file_path
JOIN sd_nodes tt ON tt.file_path = t.file_path AND tt.node_id = t.tree_id AND tt.parent_id = attr.node_id
WHERE attr.type = 'attribute';

-- `(attribute_item ...) . [(attribute_item) (line_comment) (block_comment)]* . (item) @tagged`:
-- for each item, the maximal run of attribute items and comments directly before it among
-- its named siblings; the item is tagged when one attribute item in that run qualifies.
CREATE OR REPLACE VIEW rust_attribute_runs AS
WITH sib AS (
    SELECT n.file_path, n.parent_id, n.node_id, n.type, n.sibling_index,
           -- index of the latest sibling (at or before this one) that breaks a run
           max(CASE WHEN n.type IN ('attribute_item', 'line_comment', 'block_comment') THEN NULL
                    ELSE n.sibling_index END)
               OVER (PARTITION BY n.file_path, n.parent_id ORDER BY n.sibling_index
                     ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS prev_break
    FROM sd_nodes n
    WHERE n.parent_id IS NOT NULL
      -- named siblings only: drop punctuation, the only anonymous tokens that sit next to
      -- items and attributes in their containers
      AND regexp_full_match(n.type, '[a-z_]+')
)
SELECT item.file_path, item.node_id AS item_node, item.type AS item_type, run.node_id AS attr_node
FROM sib item
JOIN sib run ON run.file_path = item.file_path AND run.parent_id = item.parent_id
     AND run.type = 'attribute_item'
     AND run.sibling_index < item.sibling_index
     AND run.sibling_index > coalesce(item.prev_break, -1)
WHERE item.type NOT IN ('attribute_item', 'line_comment', 'block_comment');

CREATE OR REPLACE VIEW rust_tags AS
SELECT DISTINCT r.file_path, r.item_node AS node_id, 'test' AS tag
FROM rust_attribute_runs r
JOIN rust_attribute_kinds k ON k.file_path = r.file_path AND k.item_id = r.attr_node
WHERE (k.what = 'test_like' AND r.item_type = 'function_item')
   OR (k.what = 'cfg_test' AND r.item_type IN (
        'function_item', 'mod_item', 'impl_item', 'struct_item', 'enum_item', 'union_item',
        'trait_item', 'type_item', 'const_item', 'static_item', 'use_declaration',
        'macro_invocation', 'macro_definition'))
UNION
-- #![cfg(test)] as a direct child of source_file tags the root.
SELECT DISTINCT r.file_path, r.node_id, 'test'
FROM sd_nodes r
JOIN sd_nodes ia ON ia.file_path = r.file_path AND ia.parent_id = r.node_id AND ia.type = 'inner_attribute_item'
JOIN sd_nodes attr ON attr.file_path = ia.file_path AND attr.parent_id = ia.node_id AND attr.type = 'attribute'
JOIN rust_attribute_kinds k ON k.file_path = attr.file_path AND k.item_id = ia.node_id AND k.what = 'cfg_test'
WHERE r.type = 'source_file' AND r.parent_id IS NULL
UNION
-- File tags: fnmatch globs, where `*` also matches `/` and `**/` also matches at the root.
SELECT r.file_path, r.node_id, 'test'
FROM sd_nodes r
WHERE r.parent_id IS NULL AND (
       r.file_path LIKE '%/tests/%' OR r.file_path LIKE 'tests/%'
    OR r.file_path LIKE '%/tests.rs' OR r.file_path = 'tests.rs'
    OR r.file_path LIKE '%\_tests.rs' ESCAPE '\'
    OR r.file_path LIKE '%/test\_%.rs' ESCAPE '\' OR r.file_path LIKE 'test\_%.rs' ESCAPE '\')
UNION
-- (impl_item trait: (_)): an impl with a direct `for` token.
SELECT i.file_path, i.node_id, 'trait_impl'
FROM sd_nodes i
WHERE i.type = 'impl_item'
  AND EXISTS (SELECT 1 FROM sd_nodes f WHERE f.file_path = i.file_path AND f.parent_id = i.node_id AND f.type = 'for')
UNION
SELECT file_path, node_id, 'generated_or_macro'
FROM sd_nodes WHERE type IN ('macro_invocation', 'macro_definition');
