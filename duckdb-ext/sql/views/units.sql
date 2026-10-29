-- Units: the definitions of the SCIP indexes (views/scip.sql) that are items a reader
-- reviews, with their ranges, names, nesting, size, test flag and interface.
--
-- units(unit_id, file_id, path, crate, kind, symbol_kind, name, qualified_name, symbol,
--       parent_unit_id, start_line, start_column, end_line, end_column, name_line,
--       name_column, loc, is_test, visibility, parameters, signature, has_doc,
--       is_file_module, member_of, impl_key, impl_type, impl_trait, impl_header_line)
--
-- One unit per definition occurrence of a non-local symbol in a source file whose SCIP
-- kind (symbol_kind) is one of
--   Function -> function; Method, StaticMethod, TraitMethod -> method;
--   Struct, Enum, Union, TypeAlias, Trait, AssociatedType -> type; Module -> module;
--   Macro -> macro; Constant, StaticVariable -> constant.
-- Fields, enum variants, parameters and locals are not units; their occurrences belong to
-- the unit that contains them. An impl block is not a unit: rust-analyzer gives it no
-- symbol, and its members' symbols carry it as a descriptor (`…impl#[Type]m().`,
-- `…impl#[Type][Trait]m().`): member_of is 'impl' or 'trait_impl' for such a member and
-- 'trait' for a trait's own item (NULL otherwise), impl_key is that prefix of a member's
-- symbol, impl_type and
-- impl_trait the bracketed names as written (backticks removed), impl_header_line the
-- line of the impl's header found as for is_test below (NULL when none is found: an impl a
-- macro writes). A `mod x;` declaration
-- is a reference to the module, not a unit; the module is a unit where it is defined: a
-- file module (is_file_module, its range the whole file) or an inline `mod x { … }`.
-- Code rust-analyzer does not see (an inactive cfg, a file outside every indexed
-- workspace) has no units.
--
-- Ranges are SCIP's enclosing_range: [start_line, end_line], leading doc comments and
-- attributes included; name_line/name_column is where the definition occurrence (the
-- name) is. parent_unit_id is the innermost other unit whose range contains this one's
-- (two units with the same range, such as several items one macro call defines, are
-- siblings). unit_id numbers the units by file_id, then by range start, a longer range
-- first, so an enclosing unit comes before what it contains and the innermost unit
-- containing a position is the containing unit with the greatest unit_id.
--
-- crate is the symbol's package (the Cargo package). name is the symbol's display name
-- (the package name for a crate root, whose display name is empty). qualified_name is the
-- symbol's descriptors below the file's own module, joined with `.`: `f`, `Type.field`'s
-- owner `Type`, `tests.f`, `Type.m` for an inherent method, `Trait for Type.m` for a
-- trait impl's member; a file module's is its name. Items inside a function body do not
-- name the function (SCIP's descriptors leave it out). symbol is the SCIP symbol.
--
-- loc counts the non-blank lines of the range. visibility comes from the signature
-- (signature_documentation's text): pub, pub(crate), pub(super), pub(in …) as written,
-- else private; NULL for a trait's own members and a trait impl's members, which take the
-- trait's (member_of 'trait', 'trait_impl'). parameters counts a function's or method's parameters, self included, from
-- the signature's parameter list (NULL for other kinds). has_doc: the symbol has
-- documentation (doc comments); NULL for a trait impl's member, to which rust-analyzer
-- gives the trait item's documentation.
--
-- is_test (Rust's rules plus the settings file's): a unit is a test when
--   - its file matches a test path: `tests/` directories, `tests.rs`, `*_tests.rs`,
--     `test_*.rs` (`*` crossing `/`), or a pattern of extra_test_paths;
--   - a line of its lead (from start_line to name_line, comment lines skipped) holds an
--     attribute whose last path segment ends in `test` (#[test], #[tokio::test],
--     #[rstest]), one named in extra_test_attributes, or #[cfg(test)] /
--     #[cfg(all(…test…))]; a file module, when the file has #![cfg(test)];
--   - it is a member of an impl whose header line (the nearest `impl` line above it that
--     names impl_type, outside other functions' bodies) is led by #[cfg(test)];
--   - or a unit whose range contains it is a test.

-- impl_base_name(t): the bare name of a type or trait as a descriptor or a signature
-- writes it: backticks, a leading `&`, `mut`, `dyn`, generic arguments and a path removed
-- (`Box<dyn X>` -> Box, `From<serde_json::Error>` -> From, `a::B` -> B).
CREATE OR REPLACE MACRO impl_base_name(t) AS
    regexp_extract(
        regexp_replace(trim(regexp_replace(replace(t, '`', ''), '<.*$', '')), '^(?:&\s*)?(?:mut\s+)?(?:dyn\s+)?', ''),
        '([A-Za-z0-9_]+)\s*$', 1);

-- impl_generic_arg(t): the bare name of the first generic argument (`From<a::B>` -> B).
CREATE OR REPLACE MACRO impl_generic_arg(t) AS
    impl_base_name(regexp_extract(replace(t, '`', ''), '<\s*([^,>]+)', 1));

CREATE OR REPLACE VIEW derived_units AS
WITH defs AS MATERIALIZED (
    SELECT o.file_id, o.document, o.symbol, o.start_line AS name_line, o.start_column AS name_column,
           o.enclosing_start_line AS start_line, o.enclosing_start_column AS start_column,
           o.enclosing_end_line AS end_line, o.enclosing_end_column AS end_column
    FROM scip_occurrences o
    WHERE o.is_definition AND o.file_id IS NOT NULL AND NOT starts_with(o.symbol, 'local ')
      AND o.enclosing_start_line IS NOT NULL
),
info AS MATERIALIZED (
    SELECT document, symbol, any_value(kind) AS kind, any_value(display_name) AS display_name,
           any_value(documentation) AS documentation, any_value(signature) AS signature
    FROM scip_symbols WHERE document IS NOT NULL
    GROUP BY document, symbol
),
typed AS MATERIALIZED (
    SELECT d.*, i.kind AS symbol_kind, i.display_name, i.documentation, i.signature,
           CASE WHEN i.kind = 'Function' THEN 'function'
                WHEN i.kind IN ('Method', 'StaticMethod', 'TraitMethod') THEN 'method'
                WHEN i.kind IN ('Struct', 'Enum', 'Union', 'TypeAlias', 'Trait', 'AssociatedType') THEN 'type'
                WHEN i.kind = 'Module' THEN 'module'
                WHEN i.kind = 'Macro' THEN 'macro'
                WHEN i.kind IN ('Constant', 'StaticVariable') THEN 'constant' END AS kind,
           split_part(d.symbol, ' ', 3) AS crate,
           -- rust-analyzer places a file module's definition at the file's start, its range
           -- the whole file.
           i.kind = 'Module' AND d.name_line = 1 AND d.name_column = 1 AND d.start_line = 1 AS is_file_module
    FROM defs d JOIN info i USING (document, symbol)
),
numbered AS MATERIALIZED (
    SELECT (row_number() OVER (ORDER BY file_id, start_line, start_column, end_line DESC, end_column DESC,
                                        name_line, name_column, symbol) - 1)::INTEGER AS unit_id, *
    FROM typed WHERE kind IS NOT NULL
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
-- The file module's symbol is the prefix of the file's descriptors; a crate root's
-- (`… crate/`) without its `crate/`.
file_prefixes AS (
    SELECT file_id,
           any_value(CASE WHEN ends_with(symbol, ' crate/') THEN left(symbol, length(symbol) - 6) ELSE symbol END) AS prefix
    FROM numbered WHERE is_file_module
    GROUP BY file_id
),
named AS MATERIALIZED (
    SELECT n.*, p.parent_unit_id,
           coalesce(nullif(n.display_name, ''), n.crate) AS name,
           -- The descriptors after the prefix, or after `scheme manager package version `.
           CASE WHEN fp.prefix IS NOT NULL AND starts_with(n.symbol, fp.prefix) THEN substr(n.symbol, length(fp.prefix) + 1)
                ELSE regexp_extract(n.symbol, '^[^ ]+ [^ ]+ [^ ]+ [^ ]+ (.*)$', 1) END AS descriptors,
           -- impl members: …impl#[Type][Trait]member followed by its suffix.
           regexp_extract(n.symbol, '^(.*impl#\[(?:`[^`]*`|[^\]`])*\](?:\[(?:`[^`]*`|[^\]`])*\])?)(?:`[^`]*`|[^/#.!\[\]`])+(?:\(\)\.|\.|#|!)$', 1) AS impl_key,
           -- The symbol without its last descriptor: what the unit is a member of.
           regexp_extract(n.symbol, '^(.*?)(?:`[^`]*`|[^/#.!\[\]`])+(?:\(\)\.|\.|#|/|!)$', 1) AS owner_symbol
    FROM numbered n
    LEFT JOIN parents p USING (unit_id)
    LEFT JOIN file_prefixes fp USING (file_id)
),
impl_named AS MATERIALIZED (
    SELECT *,
           nullif(replace(regexp_extract(impl_key, 'impl#\[((?:`[^`]*`|[^\]`])*)\]', 1), '`', ''), '') AS impl_type,
           nullif(replace(regexp_extract(impl_key, 'impl#\[(?:`[^`]*`|[^\]`])*\]\[((?:`[^`]*`|[^\]`])*)\]', 1), '`', ''), '') AS impl_trait
    FROM (SELECT * REPLACE (nullif(impl_key, '') AS impl_key) FROM named)
),
-- Lines: blank, comment and test-attribute flags, and for every line the last line
-- before it that is not part of an item's lead (attributes, comments, blank lines).
line_flags AS MATERIALIZED (
    SELECT file_id, line, text, is_blank, is_comment, nonblank_through,
           NOT is_comment AND (
               regexp_matches(text, '#\[\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*[A-Za-z0-9_]*test\s*(?:\]|\()')
               OR regexp_matches(text, '#\[\s*cfg\s*\(\s*(?:test\s*\)|all\s*\(.*\btest\b)')
               OR coalesce(list_bool_or(list_transform(getvariable('extra_test_attributes'),
                      lambda a: regexp_matches(text, '#\[\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*' || a || '\s*(?:\]|\()'))), false)
           ) AS test_attribute,
           NOT is_comment AND regexp_matches(text, '#\[\s*cfg\s*\(\s*(?:test\s*\)|all\s*\(.*\btest\b)') AS cfg_test,
           NOT is_comment AND regexp_matches(text, '^\s*#!\[\s*cfg\s*\(\s*test\s*\)') AS inner_cfg_test,
           NOT is_comment AND regexp_matches(text, '^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:unsafe\s+)?impl\b') AS impl_header,
           max(CASE WHEN NOT (is_blank OR is_comment OR regexp_matches(text, '^\s*#\[')) THEN line END)
               OVER (PARTITION BY file_id ORDER BY line ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS lead_break
    FROM source_lines
),
file_lines AS (
    SELECT file_id, max(line) AS last_line, max(nonblank_through) AS nonblank FROM line_flags GROUP BY file_id
),
headers AS MATERIALIZED (
    SELECT h.file_id, h.line, h.text,
           EXISTS (SELECT 1 FROM line_flags a
                   WHERE a.file_id = h.file_id AND a.line < h.line AND a.line > coalesce(h.lead_break, 0)
                     AND a.cfg_test) AS cfg_test
    FROM line_flags h WHERE h.impl_header
),
bodies AS MATERIALIZED (
    SELECT unit_id, file_id, start_line, end_line FROM impl_named WHERE kind IN ('function', 'method')
),
-- Each impl member's header: the nearest impl line above it that names its type and is
-- not inside the body of a function or method that does not contain the member.
member_headers AS MATERIALIZED (
    SELECT m.unit_id, arg_max(h.line, h.line) AS header_line, arg_max(h.cfg_test, h.line) AS header_cfg_test
    FROM impl_named m
    JOIN headers h ON h.file_id = m.file_id AND h.line <= m.start_line
         AND contains(h.text, impl_base_name(m.impl_type))
    WHERE m.impl_key IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM bodies b
                      WHERE b.file_id = h.file_id AND b.start_line <= h.line AND h.line <= b.end_line
                        AND NOT (b.start_line <= m.start_line AND m.end_line <= b.end_line))
    GROUP BY m.unit_id
),
lead_tests AS (
    SELECT DISTINCT u.unit_id
    FROM impl_named u
    JOIN line_flags l ON l.file_id = u.file_id AND l.line BETWEEN u.start_line AND u.name_line AND l.test_attribute
    UNION
    SELECT u.unit_id FROM impl_named u
    WHERE u.is_file_module AND EXISTS (SELECT 1 FROM line_flags l WHERE l.file_id = u.file_id AND l.inner_cfg_test)
    UNION
    SELECT unit_id FROM member_headers WHERE header_cfg_test
    UNION
    SELECT u.unit_id FROM impl_named u JOIN files f USING (file_id)
    WHERE f.path LIKE '%/tests/%' OR f.path LIKE 'tests/%'
       OR f.path LIKE '%/tests.rs' OR f.path = 'tests.rs'
       OR f.path LIKE '%\_tests.rs' ESCAPE '\'
       OR f.path LIKE '%/test\_%.rs' ESCAPE '\' OR f.path LIKE 'test\_%.rs' ESCAPE '\'
       OR coalesce(list_bool_or(list_transform(getvariable('extra_test_paths'), lambda p: f.path GLOB p)), false)
),
test_ranges AS MATERIALIZED (
    SELECT u.file_id, u.start_line, u.start_column, u.end_line, u.end_column
    FROM impl_named u JOIN lead_tests USING (unit_id)
),
tests AS (
    SELECT DISTINCT u.unit_id
    FROM impl_named u
    JOIN test_ranges t ON t.file_id = u.file_id
         AND (t.start_line < u.start_line OR (t.start_line = u.start_line AND t.start_column <= u.start_column))
         AND (t.end_line > u.end_line OR (t.end_line = u.end_line AND t.end_column >= u.end_column))
),
-- Parameters: the signature's characters, with `->` and `=>` masked so that their `>`
-- does not close a bracket, and the bracket depth before each character.
signature_chars AS MATERIALIZED (
    SELECT unit_id, pos, c,
           sum(CASE WHEN c IN ('(', '<', '[', '{') THEN 1 WHEN c IN (')', '>', ']', '}') THEN -1 ELSE 0 END)
               OVER (PARTITION BY unit_id ORDER BY pos ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS depth,
           fn_pos
    FROM (
        SELECT unit_id, unnest(string_split(s, '')) AS c, generate_subscripts(string_split(s, ''), 1) AS pos,
               strpos(s, 'fn ') AS fn_pos
        FROM (SELECT unit_id, replace(replace(signature, '->', '~~'), '=>', '~~') AS s
              FROM impl_named WHERE kind IN ('function', 'method') AND signature IS NOT NULL)
    )
),
parameter_lists AS (
    SELECT unit_id, min(pos) AS open_pos
    FROM signature_chars WHERE c = '(' AND coalesce(depth, 0) = 0 AND pos > fn_pos
    GROUP BY unit_id
),
parameter_bounds AS MATERIALIZED (
    SELECT l.unit_id, l.open_pos, min(s.pos) AS close_pos
    FROM parameter_lists l JOIN signature_chars s ON s.unit_id = l.unit_id AND s.pos > l.open_pos AND s.c = ')' AND s.depth = 1
    GROUP BY ALL
),
parameters AS (
    SELECT b.unit_id,
           CASE WHEN count(*) FILTER (WHERE s.c NOT IN (' ', chr(10), chr(9))) = 0 THEN 0
                ELSE count(*) FILTER (WHERE s.c = ',' AND s.depth = 1) + 1
                     - CASE WHEN arg_max(s.c, s.pos) FILTER (WHERE s.c NOT IN (' ', chr(10), chr(9))) = ',' THEN 1 ELSE 0 END
           END::INTEGER AS parameters
    FROM parameter_bounds b
    LEFT JOIN signature_chars s ON s.unit_id = b.unit_id AND s.pos > b.open_pos AND s.pos < b.close_pos
    GROUP BY b.unit_id
),
trait_symbols AS MATERIALIZED (
    SELECT DISTINCT symbol FROM impl_named WHERE symbol_kind = 'Trait'
)
SELECT u.unit_id, u.file_id, f.path, u.crate, u.kind, u.symbol_kind, u.name,
       CASE WHEN u.is_file_module OR u.descriptors = '' THEN u.name
            ELSE rtrim(replace(
                regexp_replace(regexp_replace(regexp_replace(regexp_replace(
                    u.descriptors,
                    'impl#\[((?:`[^`]*`|[^\]`])*)\]\[((?:`[^`]*`|[^\]`])*)\]', '\2 for \1.', 'g'),
                    'impl#\[((?:`[^`]*`|[^\]`])*)\]', '\1.', 'g'),
                    '\(\)\.', '.', 'g'),
                    '[/#!]', '.', 'g'), '`', ''), '.') END AS qualified_name,
       u.symbol, u.parent_unit_id, u.start_line, u.start_column, u.end_line, u.end_column, u.name_line,
       u.name_column,
       (coalesce(le.nonblank_through, fl.nonblank) - coalesce(ls.nonblank_through, 0))::INTEGER AS loc,
       t.unit_id IS NOT NULL AS is_test,
       CASE WHEN u.member_of IN ('trait', 'trait_impl') THEN NULL
            ELSE coalesce(nullif(regexp_extract(u.signature, '^\s*(pub(?:\s*\([^)]*\))?)', 1), ''), 'private') END AS visibility,
       p.parameters, u.signature,
       CASE WHEN u.member_of = 'trait_impl' THEN NULL ELSE coalesce(len(u.documentation), 0) > 0 END AS has_doc,
       u.is_file_module, u.member_of, u.impl_key, u.impl_type, u.impl_trait, mh.header_line AS impl_header_line
FROM (
    SELECT *, CASE WHEN impl_trait IS NOT NULL THEN 'trait_impl' WHEN impl_key IS NOT NULL THEN 'impl'
                   WHEN owner_symbol IN (SELECT symbol FROM trait_symbols) THEN 'trait' END AS member_of
    FROM impl_named
) u
JOIN files f USING (file_id)
JOIN file_lines fl USING (file_id)
LEFT JOIN line_flags le ON le.file_id = u.file_id AND le.line = u.end_line
LEFT JOIN line_flags ls ON ls.file_id = u.file_id AND ls.line = u.start_line - 1
LEFT JOIN tests t USING (unit_id)
LEFT JOIN parameters p USING (unit_id)
LEFT JOIN member_headers mh USING (unit_id)
ORDER BY u.unit_id;

-- units reads derived_units; refresh.sql replaces it with a view over its copy.
CREATE OR REPLACE VIEW units AS SELECT * FROM derived_units;

-- unit_ast(unit_id, node_id, type, semantic_type, flags, name, qualified_name,
--          signature_type, parameters, modifiers, annotations, receiver, start_line,
--          end_line, file_start_line, file_end_line, parent_id, depth, sibling_index,
--          children_count, descendant_count, scope_current, scope_function, scope_class,
--          scope_module, scope_stack): the complete tree-sitter syntax tree of every
-- unit's text, one row per node, as sitting_duck's parse_ast_list(text, language) gives
-- it (00_load.sql loads sitting_duck). Nothing is filtered out: what to count is decided
-- by the queries (views/quality.sql, unit_complexity).
--
-- The text is the unit's whole lines [start_line, end_line] of its file, parsed alone
-- with the file's language; a file module's text is therefore the whole file. A unit's
-- tree also holds every unit nested in it (an inline module's items, a fn inside a fn),
-- which has its own tree as well; per-unit counts drop the nodes that lie inside a
-- nested unit's lines (unit_own_ast, views/quality.sql).
--
-- The node columns are parse_ast_list's, without peek (a truncated text preview),
-- file_path (always '<inline>') and language (the file's, files.language). node_id is
-- the node's 0-based position in a pre-order walk of the unit's tree (the root,
-- source_file, is 0), so a node's descendants are node_id + 1 .. node_id +
-- descendant_count. parent_id, depth (the root's is 0) and sibling_index (0-based, among
-- all children, anonymous tokens included) place the node in the tree. type is the
-- tree-sitter node type, of named nodes (`if_expression`) and anonymous tokens (`if`,
-- `&&`, `?`) alike; tree-sitter's field names and is_named are not given. flags are
-- sitting_duck's bits (is_syntax_only(flags) for keywords and punctuation,
-- is_scope(flags), ...); semantic_type, name, qualified_name, signature_type,
-- parameters, modifiers, annotations, receiver and the scope_* columns (the node ids of
-- the enclosing scopes, and their stack) are sitting_duck's readings of the node.
-- start_line and end_line are 1-based, inclusive and relative to the unit's text;
-- file_start_line and file_end_line are the same lines in the file. parse_ast_list gives
-- no columns or byte offsets.
--
-- A macro invocation's arguments are a token_tree of tokens (an `if` there is a keyword
-- token, not an if_expression), and so is a macro_rules! body.
CREATE OR REPLACE VIEW unit_ast AS
WITH file_lines AS MATERIALIZED (
    SELECT s.file_id, f.language, string_split(s.content, chr(10)) AS lines
    FROM sources s JOIN files f USING (file_id)
),
bodies AS MATERIALIZED (
    SELECT u.unit_id, u.start_line AS unit_start_line, l.language,
           array_to_string(l.lines[u.start_line:u.end_line], chr(10)) AS body
    FROM units u JOIN file_lines l USING (file_id)
),
trees AS (
    SELECT unit_id, unit_start_line, unnest(parse_ast_list(body, language)) AS n FROM bodies
)
SELECT unit_id, n.node_id, n.type, n.semantic_type, n.flags, n.name, n.qualified_name, n.signature_type,
       n.parameters, n.modifiers, n.annotations, n.receiver,
       n.start_line::INTEGER AS start_line, n.end_line::INTEGER AS end_line,
       (unit_start_line + n.start_line - 1)::INTEGER AS file_start_line,
       (unit_start_line + n.end_line - 1)::INTEGER AS file_end_line,
       n.parent_id, n.depth::INTEGER AS depth, n.sibling_index, n.children_count::INTEGER AS children_count,
       n.descendant_count::INTEGER AS descendant_count,
       n.scope.current AS scope_current, n.scope.function AS scope_function, n.scope.class AS scope_class,
       n.scope.module AS scope_module, n.scope.stack AS scope_stack
FROM trees;
