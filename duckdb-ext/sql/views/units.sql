-- Units: the definitions of the SCIP indexes (views/scip.sql) that are items a reader
-- reviews, with their ranges, names, nesting, size, test flag and interface.
--
-- units(unit_id, file_id, path, package, kind, symbol_kind, name, qualified_name, symbol,
--       parent_unit_id, start_line, start_column, end_line, end_column, name_line,
--       name_column, loc, is_test, visibility, parameters, signature, has_doc,
--       is_file_unit, member_of, impl_key, impl_type, impl_trait, impl_header_line)
--
-- Which definitions are units, and their kinds, ranges, names, visibility, parameters,
-- documentation, impl membership and test rules, is language knowledge: each language's
-- view under lang/ gives its files' units (lang/rust.sql: rust_units, lang/go.sql:
-- go_units), and this file numbers, nests and sizes them all alike. A file whose
-- language has no view, or that no SCIP index covers, has no units.
--
-- One unit per definition occurrence of a non-local symbol; fields, enum variants,
-- parameters and locals are not units, and their occurrences belong to the unit that
-- contains them. kind is the neutral kind (function, method, type, namespace, macro,
-- constant), symbol_kind the SCIP kind. Every indexed file has its file unit
-- (is_file_unit), whose range is the whole file. member_of is 'impl' (a member of an
-- impl block or of a type's method set), 'trait_impl' (a member of a trait impl) or
-- 'trait' (a trait's or interface's own item), NULL otherwise; impl_key groups the
-- members of one impl, impl_type and impl_trait name its type and trait as written, and
-- impl_header_line is the impl's header line (NULL when the language has none).
--
-- Ranges are [start_line, end_line] as the language's view gives them; name_line/
-- name_column is where the definition occurrence (the name) is. parent_unit_id is the
-- innermost other unit whose range contains this one's (two units with the same range,
-- such as several items one macro call defines, are siblings). unit_id numbers the
-- units by file_id, then by range start, a longer range first, so an enclosing unit comes
-- before what it contains and the innermost unit containing a position is the
-- containing unit with the greatest unit_id.
--
-- package is the symbol's package (the Cargo package, the Go module). name is the
-- symbol's display name. qualified_name is the symbol's descriptors below the file's own
-- namespace, joined with `.`. symbol is the SCIP symbol. loc counts the non-blank lines of
-- the range. visibility is the language's (NULL where a unit takes another's);
-- parameters counts a function's or method's parameters, its receiver included (NULL for
-- other kinds); has_doc: the symbol has documentation.
--
-- is_test: the language's rules make the unit a test by itself (test_by_rule in its
-- view), or a unit whose range contains it is one.

CREATE OR REPLACE VIEW derived_units AS
WITH language_units AS MATERIALIZED (
    SELECT * FROM rust_units
    UNION ALL BY NAME
    SELECT * FROM go_units
),
numbered AS MATERIALIZED (
    SELECT (row_number() OVER (ORDER BY file_id, start_line, start_column, end_line DESC, end_column DESC,
                                        name_line, name_column, symbol) - 1)::INTEGER AS unit_id, *
    FROM language_units
),
parents AS (
    SELECT c.unit_id, max(p.unit_id) AS parent_unit_id
    FROM numbered c
    JOIN numbered p ON p.file_id = c.file_id AND p.unit_id < c.unit_id
         AND (p.start_line < c.start_line OR (p.start_line = c.start_line AND p.start_column <= c.start_column))
         AND (p.end_line > c.end_line OR (p.end_line = c.end_line AND p.end_column >= c.end_column))
         AND NOT (p.start_line = c.start_line AND p.start_column = c.start_column
                  AND p.end_line = c.end_line AND p.end_column = c.end_column)
    GROUP BY c.unit_id
),
lines AS MATERIALIZED (
    SELECT file_id, line, nonblank_through FROM source_lines
),
file_lines AS (
    SELECT file_id, max(nonblank_through) AS nonblank FROM lines GROUP BY file_id
),
test_ranges AS MATERIALIZED (
    SELECT file_id, start_line, start_column, end_line, end_column FROM numbered WHERE test_by_rule
),
tests AS (
    SELECT DISTINCT u.unit_id
    FROM numbered u
    JOIN test_ranges t ON t.file_id = u.file_id
         AND (t.start_line < u.start_line OR (t.start_line = u.start_line AND t.start_column <= u.start_column))
         AND (t.end_line > u.end_line OR (t.end_line = u.end_line AND t.end_column >= u.end_column))
)
SELECT u.unit_id, u.file_id, f.path, u.package, u.kind, u.symbol_kind, u.name, u.qualified_name,
       u.symbol, p.parent_unit_id, u.start_line, u.start_column, u.end_line, u.end_column, u.name_line,
       u.name_column,
       (coalesce(le.nonblank_through, fl.nonblank) - coalesce(ls.nonblank_through, 0))::INTEGER AS loc,
       t.unit_id IS NOT NULL AS is_test,
       u.visibility, u.parameters, u.signature, u.has_doc,
       u.is_file_unit, u.member_of, u.impl_key, u.impl_type, u.impl_trait, u.impl_header_line
FROM numbered u
JOIN files f USING (file_id)
JOIN file_lines fl USING (file_id)
LEFT JOIN lines le ON le.file_id = u.file_id AND le.line = u.end_line
LEFT JOIN lines ls ON ls.file_id = u.file_id AND ls.line = u.start_line - 1
LEFT JOIN parents p USING (unit_id)
LEFT JOIN tests t USING (unit_id)
ORDER BY u.unit_id;

-- units reads derived_units; refresh.sql replaces it with a view over its copy.
CREATE OR REPLACE VIEW units AS SELECT * FROM derived_units;

-- unit_ast(unit_id, node_id, type, semantic_type, flags, name, qualified_name,
--          signature_type, parameters, modifiers, annotations, receiver, start_line,
--          end_line, file_start_line, file_end_line, parent_id, depth, sibling_index,
--          children_count, descendant_count, scope_current, scope_function, scope_class,
--          scope_module, scope_stack, start_column, end_column, start_byte, end_byte):
-- every unit's syntax tree, one row per node: the nodes of its file's tree (file_ast,
-- views/files.sql) that lie inside the unit's range, from (start_line, start_column) to
-- (end_line, end_column); a file unit's tree is its file's whole tree. Nothing is
-- filtered out: what to count is decided by the queries (views/quality.sql,
-- unit_complexity).
--
-- A unit's range starts at its leading doc comments and attributes, which tree-sitter
-- makes siblings of the item, so a unit's top is the run of sibling nodes that lie
-- inside the range (a file unit's top is the file's root, source_file). A unit's tree
-- also holds every unit nested in it (an inline namespace's items, a fn inside a fn), which
-- has its own tree as well; per-unit counts drop the nodes that lie inside a nested
-- unit's lines (unit_own_ast, views/quality.sql).
--
-- The node columns are file_ast's. node_id, parent_id and the scope_* columns are node
-- ids of the file's tree, so a node's descendants are node_id + 1 .. node_id +
-- descendant_count and a top node's parent lies outside the unit. depth is relative to
-- the unit's top (0 there). start_line and end_line are 1-based, inclusive and relative to
-- the unit's first line; file_start_line and file_end_line are the same lines in the
-- file, and start_column, end_column, start_byte and end_byte the file's columns and
-- bytes (file_ast). sitting_duck's readings (semantic_type, name, qualified_name, ...,
-- scope) are of the node in its file.
CREATE OR REPLACE VIEW unit_ast AS
WITH inside AS (
    SELECT u.unit_id, u.start_line AS unit_start_line, a.*
    FROM units u
    JOIN file_ast a
      ON a.file_id = u.file_id
     AND (u.is_file_unit
          OR ((a.start_line > u.start_line OR (a.start_line = u.start_line AND a.start_column >= u.start_column))
              AND (a.end_line < u.end_line OR (a.end_line = u.end_line AND a.end_column <= u.end_column))))
)
SELECT unit_id, node_id, type, semantic_type, flags, name, qualified_name, signature_type,
       parameters, modifiers, annotations, receiver,
       (start_line - unit_start_line + 1)::INTEGER AS start_line,
       (end_line - unit_start_line + 1)::INTEGER AS end_line,
       start_line::INTEGER AS file_start_line, end_line::INTEGER AS file_end_line,
       parent_id, (depth - min(depth) OVER (PARTITION BY unit_id))::INTEGER AS depth, sibling_index,
       children_count::INTEGER AS children_count, descendant_count::INTEGER AS descendant_count,
       scope.current AS scope_current, scope.function AS scope_function, scope.class AS scope_class,
       scope.module AS scope_module, scope.stack AS scope_stack,
       start_column::INTEGER AS start_column, end_column::INTEGER AS end_column,
       start_byte::INTEGER AS start_byte, end_byte::INTEGER AS end_byte
FROM inside;
