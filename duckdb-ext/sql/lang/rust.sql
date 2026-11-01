-- Rust's units: the language knowledge views/units.sql's derived_units reads for the
-- files whose language is 'rust', over rust-analyzer's SCIP index (views/scip.sql).
--
-- rust_units(file_id, symbol, symbol_kind, kind, package, name, qualified_name, start_line,
--            start_column, end_line, end_column, name_line, name_column, test_by_rule,
--            visibility, parameters, signature, has_doc, is_file_unit, member_of,
--            impl_key, impl_type, impl_trait, impl_header_line): one row per unit, in the
-- columns views/units.sql describes; test_by_rule is the part of is_test that Rust's
-- rules give a unit by itself (below), before views/units.sql adds the units inside a
-- test.
--
-- One unit per definition occurrence of a non-local symbol in a Rust source file whose
-- SCIP kind (symbol_kind) is one of
--   Function -> function; Method, StaticMethod, TraitMethod -> method;
--   Struct, Enum, Union, TypeAlias, Trait, AssociatedType -> type; Module -> namespace;
--   Macro -> macro; Constant, StaticVariable -> constant.
-- An impl block is not a unit: rust-analyzer gives it no symbol, and its members'
-- symbols carry it as a descriptor (`…impl#[Type]m().`, `…impl#[Type][Trait]m().`):
-- member_of is 'impl' or 'trait_impl' for such a member and 'trait' for a trait's own
-- item (NULL otherwise), impl_key is that prefix of a member's symbol, impl_type and
-- impl_trait the bracketed names as written (backticks removed), impl_header_line the
-- line of the impl's header found as for test_by_rule below (NULL when none is found: an
-- impl a macro writes). A `mod x;` declaration is a reference to the module, not a unit;
-- the module is a unit where it is defined: a file module (is_file_unit, its range the
-- whole file) or an inline `mod x { … }`. Code rust-analyzer does not see (an inactive
-- cfg, a file outside every indexed workspace) has no units.
--
-- Ranges are SCIP's enclosing_range, leading doc comments and attributes included. package
-- is the symbol's package (the Cargo package). name is the symbol's display name (the
-- package name for a crate root, whose display name is empty). qualified_name is the
-- symbol's descriptors below the file's own module, joined with `.`: `f`, `Type.field`'s
-- owner `Type`, `tests.f`, `Type.m` for an inherent method, `Trait for Type.m` for a
-- trait impl's member; a file module's is its name. Items inside a function body do not
-- name the function (SCIP's descriptors leave it out).
--
-- visibility comes from the signature (signature_documentation's text): pub, pub(crate),
-- pub(super), pub(in …) as written, else private; NULL for a trait's own members and a
-- trait impl's members, which take the trait's (member_of 'trait', 'trait_impl').
-- parameters counts a function's or method's parameters, self included, from the
-- signature's parameter list (NULL for other kinds). has_doc: the symbol has
-- documentation (doc comments); NULL for a trait impl's member, to which rust-analyzer
-- gives the trait item's documentation.
--
-- test_by_rule (Rust's rules plus the settings file's): a unit is a test when
--   - its file matches a test path: `tests/` directories, `tests.rs`, `*_tests.rs`,
--     `test_*.rs` (`*` crossing `/`), or a pattern of extra_test_paths;
--   - a line of its lead (from start_line to name_line, comment lines skipped) holds an
--     attribute whose last path segment ends in `test` (#[test], #[tokio::test],
--     #[rstest]), one named in extra_test_attributes, or #[cfg(test)] /
--     #[cfg(all(…test…))]; a file module, when the file has #![cfg(test)];
--   - or it is a member of an impl whose header line (the nearest `impl` line above it
--     that names impl_type, outside other functions' bodies) is led by #[cfg(test)].

-- rust_terms(internal, official): Rust's names for the two levels the views call package
-- and namespace (views/terms.sql): a Cargo package and a module; the control constructs
-- keep their internal names, and Rust has no goto.
CREATE OR REPLACE VIEW rust_terms AS
SELECT * FROM (VALUES ('package', 'package'), ('namespace', 'module'), ('goto', NULL::VARCHAR))
    t(internal, official);

-- rust_control_nodes(file_id, node_id, construct, decisions, opens_level, group_id): the
-- nodes of the Rust files' trees (file_ast) that are control constructs (views/quality.sql,
-- unit_complexity), with the decision points each adds to McCabe's count, whether it
-- opens a nesting level, and the group of alternatives it is one of where all but one
-- decide (a unit counts one decision fewer per group it holds):
--   if_expression: `if` (an `if let` included), 1, a level; `else_if` when its parent is an
--     else_clause, 1, continuing its chain;
--   match_expression: `match`, 0, a level; match_arm: `arm`, 1, its group its match_block
--     (a match is exhaustive: its arms but one decide);
--   an `if` token whose parent is a match_pattern: `guard`, 1;
--   for_expression, while_expression (`while let` included): `for`, `while`, 1, a level;
--     loop_expression: `loop`, 0 (its exits are breaks, as rust-code-analysis issue #1249
--     expects from McCabe), a level;
--   try_expression (`?`): `try`, 1;
--   `&&` and `||` tokens whose parent is a binary_expression or a let_chain (`if let … &&
--     …`): `and`, `or`, 1;
--   an `else` token whose parent is a let_declaration: `let_else`, 1;
--   return_, break_, continue_expression: `return`, `break`, `continue`, 0;
--   closure_expression: `closure`, 0 (its body's constructs count for the unit that
--     writes it; it adds no entry of its own).
-- A macro invocation's arguments are tokens and hold none.
CREATE OR REPLACE VIEW rust_control_nodes AS
WITH nodes AS MATERIALIZED (
    SELECT a.file_id, a.node_id, a.type, a.parent_id
    FROM file_ast a JOIN files f USING (file_id)
    WHERE f.language = 'rust'
),
typed AS MATERIALIZED (
    SELECT n.file_id, n.node_id, n.type, n.parent_id, p.type AS parent_type
    FROM nodes n LEFT JOIN nodes p ON p.file_id = n.file_id AND p.node_id = n.parent_id
    WHERE n.type IN ('if_expression', 'match_expression', 'match_arm', 'for_expression', 'while_expression',
                     'loop_expression', 'try_expression', 'return_expression', 'break_expression',
                     'continue_expression', 'closure_expression', 'if', 'else', '&&', '||')
),
classified AS (
    SELECT file_id, node_id,
           CASE WHEN type = 'if_expression' AND parent_type = 'else_clause' THEN 'else_if'
                WHEN type = 'if_expression' THEN 'if'
                WHEN type = 'match_expression' THEN 'match'
                WHEN type = 'match_arm' THEN 'arm'
                WHEN type = 'if' AND parent_type = 'match_pattern' THEN 'guard'
                WHEN type = 'for_expression' THEN 'for'
                WHEN type = 'while_expression' THEN 'while'
                WHEN type = 'loop_expression' THEN 'loop'
                WHEN type = 'try_expression' THEN 'try'
                WHEN type = '&&' AND parent_type IN ('binary_expression', 'let_chain') THEN 'and'
                WHEN type = '||' AND parent_type IN ('binary_expression', 'let_chain') THEN 'or'
                WHEN type = 'else' AND parent_type = 'let_declaration' THEN 'let_else'
                WHEN type = 'return_expression' THEN 'return'
                WHEN type = 'break_expression' THEN 'break'
                WHEN type = 'continue_expression' THEN 'continue'
                WHEN type = 'closure_expression' THEN 'closure' END AS construct,
           CASE WHEN type = 'match_arm' THEN parent_id END AS group_id
    FROM typed
)
SELECT file_id, node_id, construct,
       CASE WHEN construct IN ('if', 'else_if', 'arm', 'guard', 'for', 'while', 'try', 'and', 'or', 'let_else')
            THEN 1 ELSE 0 END AS decisions,
       construct IN ('if', 'match', 'for', 'while', 'loop') AS opens_level, group_id
FROM classified
WHERE construct IS NOT NULL;

-- rust_file_namespaces(file_id, namespace, namespace_path): each Rust file is its own
-- namespace, its file module (views/graph.sql, file_namespaces), named by the file's path
-- and placed by Rust's file-module convention: a crate root (`…/lib.rs`, `…/main.rs`) and
-- a `…/mod.rs` are their directory, any other file its path without `.rs`. The path
-- convention rather than the descriptors places modules because SCIP names every target
-- of a package's crate root `crate/`: an integration test's root would otherwise be an
-- ancestor of every module of the library. `#[path]` attributes are not read, and a
-- binary under src/bin/ is placed under the library's root.
CREATE OR REPLACE VIEW rust_file_namespaces AS
SELECT file_id, path AS namespace,
       CASE WHEN regexp_matches(path, '(^|/)(lib|main|mod)\.rs$')
            THEN regexp_replace(path, '/?(lib|main|mod)\.rs$', '')
            ELSE regexp_replace(path, '\.rs$', '') END AS namespace_path
FROM files WHERE language = 'rust';

-- rust_visibility_boundary(level): the level whose boundary `pub` opens (views/terms.sql,
-- visibility_boundary): the package, a crate's outside.
CREATE OR REPLACE VIEW rust_visibility_boundary AS SELECT 'package' AS level;

-- rust_namespace_references(owner_file_id, symbol, file_id, start_line, start_column): the
-- reference occurrences that name a Rust module by its own symbol, with the file of the
-- module's definition that takes them (views/quality.sql, unit_fan_in): the path segments
-- that name it (`a` in `crate::a::f`, `use crate::a;`, an extern crate's name, an alias of
-- it), and not the occurrences rust-analyzer also gives its symbol that name no module:
-- the keywords `crate`, `super` and `self` (in a path and in `pub(crate)`, `pub(super)`),
-- a segment of a `pub(in …)` path, and the `mod a;` declaration, told apart by the
-- occurrence's source text. The target roots of one package share its crate root's symbol
-- (rust-analyzer names a symbol by package): the library's root (a path ending in
-- `src/lib.rs`), which is what a package name in a path reaches, takes those references
-- when the package has one, else every root sharing the symbol does.
CREATE OR REPLACE VIEW rust_namespace_references AS
WITH modules AS MATERIALIZED (
    SELECT DISTINCT o.symbol, o.file_id, f.path
    FROM scip_occurrences o
    JOIN files f USING (file_id)
    JOIN (SELECT DISTINCT document, symbol FROM scip_symbols WHERE kind = 'Module') s USING (document, symbol)
    WHERE f.language = 'rust' AND o.is_definition AND NOT starts_with(o.symbol, 'local ')
      AND o.enclosing_start_line IS NOT NULL
),
owners AS (
    SELECT symbol, file_id FROM modules
    QUALIFY ends_with(path, 'src/lib.rs') OR NOT bool_or(ends_with(path, 'src/lib.rs')) OVER (PARTITION BY symbol)
),
mentions AS (
    SELECT o.symbol, o.file_id, o.start_line, o.start_column
    FROM scip_occurrences o
    JOIN source_lines l ON l.file_id = o.file_id AND l.line = o.start_line
    WHERE NOT o.is_definition AND o.symbol IN (SELECT symbol FROM modules)
      AND substr(l.text, o.start_column, o.end_column - o.start_column) NOT IN ('crate', 'super', 'self')
      AND NOT regexp_matches(substr(l.text, 1, o.start_column - 1), 'pub\(in [\w:]*$')
      AND NOT regexp_matches(l.text, '^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*;')
)
SELECT w.file_id AS owner_file_id, m.symbol, m.file_id, m.start_line, m.start_column
FROM owners w JOIN mentions m USING (symbol);

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

CREATE OR REPLACE VIEW rust_units AS
WITH defs AS MATERIALIZED (
    SELECT o.file_id, o.document, o.symbol, o.start_line AS name_line, o.start_column AS name_column,
           o.enclosing_start_line AS start_line, o.enclosing_start_column AS start_column,
           o.enclosing_end_line AS end_line, o.enclosing_end_column AS end_column
    FROM scip_occurrences o
    WHERE o.is_definition AND o.file_id IS NOT NULL AND NOT starts_with(o.symbol, 'local ')
      AND o.enclosing_start_line IS NOT NULL
      AND o.file_id IN (SELECT file_id FROM files WHERE language = 'rust')
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
                WHEN i.kind = 'Module' THEN 'namespace'
                WHEN i.kind = 'Macro' THEN 'macro'
                WHEN i.kind IN ('Constant', 'StaticVariable') THEN 'constant' END AS kind,
           split_part(d.symbol, ' ', 3) AS package,
           -- rust-analyzer places a file module's definition at the file's start, its range
           -- the whole file.
           i.kind = 'Module' AND d.name_line = 1 AND d.name_column = 1 AND d.start_line = 1 AS is_file_unit
    FROM defs d JOIN info i USING (document, symbol)
),
-- unit_id here only keys this view's rows; views/units.sql numbers the units.
numbered AS MATERIALIZED (
    SELECT (row_number() OVER (ORDER BY file_id, start_line, start_column, end_line DESC, end_column DESC,
                                        name_line, name_column, symbol) - 1)::INTEGER AS unit_id, *
    FROM typed WHERE kind IS NOT NULL
),
-- The file module's symbol is the prefix of the file's descriptors; a crate root's
-- (`… crate/`) without its `crate/`.
file_prefixes AS (
    SELECT file_id,
           any_value(CASE WHEN ends_with(symbol, ' crate/') THEN left(symbol, length(symbol) - 6) ELSE symbol END) AS prefix
    FROM numbered WHERE is_file_unit
    GROUP BY file_id
),
named AS MATERIALIZED (
    SELECT n.*,
           coalesce(nullif(n.display_name, ''), n.package) AS name,
           -- The descriptors after the prefix, or after `scheme manager package version `.
           CASE WHEN fp.prefix IS NOT NULL AND starts_with(n.symbol, fp.prefix) THEN substr(n.symbol, length(fp.prefix) + 1)
                ELSE regexp_extract(n.symbol, '^[^ ]+ [^ ]+ [^ ]+ [^ ]+ (.*)$', 1) END AS descriptors,
           -- impl members: …impl#[Type][Trait]member followed by its suffix.
           regexp_extract(n.symbol, '^(.*impl#\[(?:`[^`]*`|[^\]`])*\](?:\[(?:`[^`]*`|[^\]`])*\])?)(?:`[^`]*`|[^/#.!\[\]`])+(?:\(\)\.|\.|#|!)$', 1) AS impl_key,
           -- The symbol without its last descriptor: what the unit is a member of.
           regexp_extract(n.symbol, '^(.*?)(?:`[^`]*`|[^/#.!\[\]`])+(?:\(\)\.|\.|#|/|!)$', 1) AS owner_symbol
    FROM numbered n
    LEFT JOIN file_prefixes fp USING (file_id)
),
impl_named AS MATERIALIZED (
    SELECT *,
           nullif(replace(regexp_extract(impl_key, 'impl#\[((?:`[^`]*`|[^\]`])*)\]', 1), '`', ''), '') AS impl_type,
           nullif(replace(regexp_extract(impl_key, 'impl#\[(?:`[^`]*`|[^\]`])*\]\[((?:`[^`]*`|[^\]`])*)\]', 1), '`', ''), '') AS impl_trait
    FROM (SELECT * REPLACE (nullif(impl_key, '') AS impl_key) FROM named)
),
-- Lines: comment and test-attribute flags, and for every line the last line before it
-- that is not part of an item's lead (attributes, comments, blank lines).
line_flags AS MATERIALIZED (
    SELECT file_id, line, text, is_blank, is_comment,
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
    WHERE file_id IN (SELECT file_id FROM files WHERE language = 'rust')
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
    WHERE u.is_file_unit AND EXISTS (SELECT 1 FROM line_flags l WHERE l.file_id = u.file_id AND l.inner_cfg_test)
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
SELECT u.file_id, u.symbol, u.symbol_kind, u.kind, u.package, u.name,
       CASE WHEN u.is_file_unit OR u.descriptors = '' THEN u.name
            ELSE rtrim(replace(
                regexp_replace(regexp_replace(regexp_replace(regexp_replace(
                    u.descriptors,
                    'impl#\[((?:`[^`]*`|[^\]`])*)\]\[((?:`[^`]*`|[^\]`])*)\]', '\2 for \1.', 'g'),
                    'impl#\[((?:`[^`]*`|[^\]`])*)\]', '\1.', 'g'),
                    '\(\)\.', '.', 'g'),
                    '[/#!]', '.', 'g'), '`', ''), '.') END AS qualified_name,
       u.start_line, u.start_column, u.end_line, u.end_column, u.name_line, u.name_column,
       t.unit_id IS NOT NULL AS test_by_rule,
       CASE WHEN u.member_of IN ('trait', 'trait_impl') THEN NULL
            ELSE coalesce(nullif(regexp_extract(u.signature, '^\s*(pub(?:\s*\([^)]*\))?)', 1), ''), 'private') END AS visibility,
       p.parameters, u.signature,
       CASE WHEN u.member_of = 'trait_impl' THEN NULL ELSE coalesce(len(u.documentation), 0) > 0 END AS has_doc,
       u.is_file_unit, u.member_of, u.impl_key, u.impl_type, u.impl_trait, mh.header_line AS impl_header_line
FROM (
    SELECT *, CASE WHEN impl_trait IS NOT NULL THEN 'trait_impl' WHEN impl_key IS NOT NULL THEN 'impl'
                   WHEN owner_symbol IN (SELECT symbol FROM trait_symbols) THEN 'trait' END AS member_of
    FROM impl_named
) u
LEFT JOIN (SELECT DISTINCT unit_id FROM lead_tests) t USING (unit_id)
LEFT JOIN parameters p USING (unit_id)
LEFT JOIN member_headers mh USING (unit_id);
