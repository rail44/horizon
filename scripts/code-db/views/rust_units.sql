-- Prototype: the spike's Rust unit query (languages.py, RUST.unit_query) as SQL views over
-- sitting_duck's read_ast output. Not wired into build.py.
--
-- Inputs (relations the caller creates first):
--   sd_nodes(file_path, node_id, parent_id, type, start_byte, end_byte, start_line, end_line,
--            sibling_index, children_count, descendant_count)
--       read_ast(..., source := 'full'); node_id is the per-file preorder index.
--   sd_source(file_path, content VARCHAR)
--       read_text over the same files; node text is cut from it by byte offsets.
--
-- Output: rust_units(unit_id, file_path, node_id, parent_node_id, kind, name, qualified_name,
--                    start_row, end_row, loc) with 0-based rows like the spike's `units`.
--
-- read_ast exposes no tree-sitter field names and no is_named flag, so the field-based
-- patterns are restated by child type and sibling position (see the comments below).

-- Bytes [s, e) of a file whose content is given hex-encoded. DuckDB has no substring on
-- BLOB and VARCHAR substring counts code points, so slicing goes through hex().
CREATE OR REPLACE MACRO sd_slice(hex_content, s, e) AS
    decode(unhex(substring(hex_content, 2 * s + 1, 2 * (e - s))));

-- syntax.node_text: collapse whitespace runs to one space and strip.
CREATE OR REPLACE MACRO sd_norm(t) AS trim(regexp_replace(t, '\s+', ' ', 'g'));

CREATE OR REPLACE TEMP TABLE sd_source_hex AS
SELECT file_path, hex(content) AS hex FROM sd_source;

-- Cumulative count of non-blank lines per file (syntax.nonblank_lines strips ASCII
-- whitespace from each b"\n"-separated line).
CREATE OR REPLACE TEMP TABLE sd_nonblank AS
WITH lines AS (
    SELECT file_path, unnest(string_split(content, chr(10))) AS line,
           generate_subscripts(string_split(content, chr(10)), 1) - 1 AS row
    FROM sd_source
)
SELECT file_path, row,
       sum(CASE WHEN regexp_full_match(line, '[ \t\r\x0b\x0c]*') THEN 0 ELSE 1 END)
           OVER (PARTITION BY file_path ORDER BY row) AS cum
FROM lines;

CREATE OR REPLACE VIEW rust_unit_candidates AS
SELECT n.file_path, n.node_id, n.type,
       -- read_ast's positions are UINTEGER; widen them before any subtraction.
       n.start_byte::BIGINT AS start_byte, n.end_byte::BIGINT AS end_byte,
       n.start_line::BIGINT AS start_line, n.end_line::BIGINT AS end_line,
       n.descendant_count::BIGINT AS descendant_count,
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
FROM sd_nodes n
LEFT JOIN sd_nodes p ON p.file_path = n.file_path AND p.node_id = n.parent_id
LEFT JOIN sd_nodes g ON g.file_path = p.file_path AND g.node_id = p.parent_id
WHERE n.type IN ('function_item', 'function_signature_item', 'impl_item', 'struct_item', 'enum_item',
                 'union_item', 'type_item', 'trait_item', 'mod_item', 'macro_definition');

-- Name nodes. A field like `name: (identifier)` becomes "the first direct child of that
-- type": the other direct children of these items never have that type.
-- For impl_item, `trait:` and `type:` are the non-comment children right before and right
-- after the anonymous `for` token; without `for`, `type:` is the first child after `impl`
-- that is not type_parameters.
CREATE OR REPLACE VIEW rust_unit_name_nodes AS
WITH children AS (
    SELECT u.file_path, u.node_id AS unit_node, u.type AS unit_type, u.kind,
           c.node_id, c.type, c.sibling_index, c.start_byte, c.end_byte
    FROM rust_unit_candidates u
    JOIN sd_nodes c ON c.file_path = u.file_path AND c.parent_id = u.node_id
    WHERE u.kind IS NOT NULL
),
markers AS (
    SELECT file_path, unit_node,
           min(sibling_index) FILTER (WHERE type = 'for') AS for_idx,
           min(sibling_index) FILTER (WHERE type = 'impl') AS impl_idx
    FROM children WHERE unit_type = 'impl_item' GROUP BY ALL
),
simple AS (
    SELECT file_path, unit_node, arg_min(node_id, sibling_index) AS node_id
    FROM children
    WHERE (unit_type IN ('function_item', 'function_signature_item', 'mod_item', 'macro_definition')
           AND type = 'identifier')
       OR (unit_type IN ('struct_item', 'enum_item', 'union_item', 'type_item', 'trait_item')
           AND type = 'type_identifier')
    GROUP BY ALL
),
impl_trait AS (
    SELECT c.file_path, c.unit_node, arg_max(c.node_id, c.sibling_index) AS node_id
    FROM children c JOIN markers m USING (file_path, unit_node)
    WHERE m.for_idx IS NOT NULL AND c.sibling_index < m.for_idx
      AND c.type NOT IN ('line_comment', 'block_comment')
    GROUP BY ALL
),
impl_type AS (
    SELECT c.file_path, c.unit_node, arg_min(c.node_id, c.sibling_index) AS node_id
    FROM children c JOIN markers m USING (file_path, unit_node)
    WHERE c.type NOT IN ('line_comment', 'block_comment')
      AND CASE WHEN m.for_idx IS NOT NULL THEN c.sibling_index > m.for_idx
               ELSE c.sibling_index > m.impl_idx AND c.type <> 'type_parameters' END
    GROUP BY ALL
)
SELECT file_path, unit_node, node_id FROM simple
UNION ALL SELECT file_path, unit_node, node_id FROM impl_trait
UNION ALL SELECT file_path, unit_node, node_id FROM impl_type;

CREATE OR REPLACE VIEW rust_units_unnested AS
SELECT u.file_path, u.node_id, u.kind, u.start_byte, u.end_byte, u.start_line, u.end_line,
       u.descendant_count,
       -- Several @name captures join in source order with " for " (name_separator).
       string_agg(sd_norm(sd_slice(h.hex, nn.start_byte, nn.end_byte)), ' for ' ORDER BY nn.start_byte) AS name
FROM rust_unit_candidates u
JOIN rust_unit_name_nodes x ON x.file_path = u.file_path AND x.unit_node = u.node_id
JOIN sd_nodes nn ON nn.file_path = x.file_path AND nn.node_id = x.node_id
JOIN sd_source_hex h ON h.file_path = u.file_path
WHERE u.kind IS NOT NULL
GROUP BY ALL;

-- Enclosing units are the unit ancestors in the tree: preorder ids make "a contains u"
-- the interval test a.node_id < u.node_id <= a.node_id + a.descendant_count.
CREATE OR REPLACE VIEW rust_units AS
WITH nested AS (
    SELECT u.file_path, u.node_id, u.kind, u.name, u.start_byte, u.end_byte, u.start_line, u.end_line,
           max(a.node_id) FILTER (WHERE a.node_id < u.node_id) AS parent_node_id,
           string_agg(a.name, '.' ORDER BY a.node_id) AS qualified_name
    FROM rust_units_unnested u
    JOIN rust_units_unnested a ON a.file_path = u.file_path
         AND a.node_id <= u.node_id AND u.node_id <= a.node_id + a.descendant_count
    GROUP BY ALL
)
SELECT row_number() OVER (ORDER BY n.file_path, n.start_byte, -n.end_byte) - 1 AS unit_id,
       n.file_path, n.node_id, n.parent_node_id, n.kind, n.name, n.qualified_name,
       n.start_line - 1 AS start_row, n.end_line - 1 AS end_row,
       e.cum - coalesce(s.cum, 0) AS loc
FROM nested n
JOIN sd_nonblank e ON e.file_path = n.file_path AND e.row = n.end_line - 1
LEFT JOIN sd_nonblank s ON s.file_path = n.file_path AND s.row = n.start_line - 2;
