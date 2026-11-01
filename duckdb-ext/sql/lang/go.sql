-- Go's units: the language knowledge views/units.sql's derived_units reads for the files
-- whose language is 'go', over scip-go's SCIP index (views/scip.sql) and the files' syntax
-- trees (go_file_ast, below).
--
-- go_units: rust_units' columns (lang/rust.sql), for Go.
--
-- One unit per definition occurrence of a non-local symbol in a Go source file whose
-- SCIP kind (symbol_kind) is one of
--   Function -> function; Method, MethodSpecification -> method;
--   Type, Struct, Interface, TypeAlias -> type; Package -> file;
--   Constant, Variable -> constant.
-- A Variable that is not local is a package-level `var`; struct fields (Field) and
-- everything inside a function body (scip-go's `local N` symbols) are not units. A
-- function or method is in no type's range: member_of is 'impl' for a method (impl_key
-- the receiver type's symbol, impl_type its name, so a type's methods across its files
-- share one key) and 'trait' for an interface's method (MethodSpecification); impl_trait
-- and impl_header_line are NULL. Code scip-go does not see (a file whose build
-- constraints exclude it, a directory outside every indexed module) has no units.
--
-- Ranges. A function's or method's is SCIP's enclosing_range, from `func` to the closing
-- brace, its doc comment not included. scip-go gives no other definition a range, so the
-- others come from the syntax tree: a type, constant or variable is the spec that
-- declares its name (type_spec, type_alias, const_spec, var_spec), or the whole
-- declaration (`type X struct { … }`, from `type`) when the declaration holds that spec
-- only; names one spec declares together (`const a, b = 1, 2`) share its range. An
-- interface's method is its method_elem (`Area() float64`). The file
-- unit (is_file_unit) is one per file: the package's definition occurrence at the file's
-- `package` clause, its range the file's whole tree (source_file).
--
-- package is the symbol's package, which scip-go names by the Go module (`example.com/m`).
-- name is the symbol's display name (the Go package's name for a file unit).
-- qualified_name is the symbol's descriptors below the package, joined with `.`: `F`,
-- `T`, `T.M` for a method or an interface's method; a file unit's is its name.
--
-- visibility: 'pub' for an exported name (its first letter is upper case), else
-- 'private'; NULL for an interface's method, which takes the interface's, and for a file
-- unit. parameters counts a function's or method's parameters, the receiver included
-- (as Rust's self), from the signature's parameter list: each name of a group (`a, b
-- int`) counts. has_doc: the symbol has documentation (its doc comment; a package's is
-- the package's doc comment in any of its files, so every file unit of a documented
-- package has it).
--
-- entry: 'entry_point' for `main` in package main and every `init` (the runtime calls
-- them), 'root' for a file of package main (a program's root); NULL otherwise.
--
-- test_by_rule: the unit's file is a Go test file (`*_test.go`) or matches a pattern of
-- extra_test_paths.
-- go_terms(internal, official): Go's names for the two levels the views call package and
-- namespace (views/terms.sql): a Go module and a Go package.
CREATE OR REPLACE VIEW go_terms AS
SELECT * FROM (VALUES ('package', 'module'), ('namespace', 'package')) t(internal, official);

-- go_file_namespaces(file_id, namespace, namespace_path): a Go file belongs to its
-- directory's package (views/graph.sql, file_namespaces): the namespace is the directory
-- (`.` for the repository root), an external test package (`package x_test`) included, and
-- its namespace_path the directory too ('' for the root, as a Rust crate root at the
-- repository root), so a directory's package is an ancestor of those below it.
CREATE OR REPLACE VIEW go_file_namespaces AS
SELECT file_id, coalesce(nullif(regexp_extract(path, '^(.*)/[^/]*$', 1), ''), '.') AS namespace,
       regexp_extract(path, '^(.*)/[^/]*$', 1) AS namespace_path
FROM files WHERE language = 'go';

-- go_visibility_boundary(level): the level whose boundary an exported name opens
-- (views/terms.sql, visibility_boundary): the namespace, a Go package's outside.
CREATE OR REPLACE VIEW go_visibility_boundary AS SELECT 'namespace' AS level;

-- go_namespace_references: none (rust_namespace_references' columns). A Go file unit owns
-- no symbol (views/graph.sql, symbol_units), so a reference to a package counts for none
-- of its files; the package's fan-in is its namespace's (unit_fan_in, fan_in_namespaces).
CREATE OR REPLACE VIEW go_namespace_references AS
SELECT NULL::INTEGER AS owner_file_id, NULL::VARCHAR AS symbol, NULL::INTEGER AS file_id,
       NULL::INTEGER AS start_line, NULL::INTEGER AS start_column
WHERE false;

-- go_file_ast: file_ast's rows (views/files.sql) of the Go files only. read_ast parses
-- every file it is given, whatever the query filters, so this view gives it the Go
-- files' paths (and a path that does not exist, which keeps the list non-empty and which
-- ignore_errors skips): a repository without Go files parses nothing for go_units.
-- refresh.sql points it at the syntax trees it keeps.
CREATE OR REPLACE VIEW go_file_ast AS
SELECT f.file_id, a.* EXCLUDE (file_path)
FROM read_ast(list_filter(getvariable('source_paths'),
                          lambda p: getvariable('languages')[regexp_extract(p, '\.([^./]+)$', 1)] = 'go')
              || [getvariable('repo') || '/target/code-db/.no-such-file.go'],
              source := 'full', peek := 'none', ignore_errors := true) a
JOIN files f ON f.path = substr(a.file_path, length(getvariable('repo')) + 2);

-- go_control_nodes(file_id, node_id, construct, decisions, opens_level, group_id):
-- rust_control_nodes' columns, for the Go files' trees (go_file_ast), named in Go's words
-- and counted as McCabe does:
--   if_statement: `if`, 1, a level; `else_if` when its parent is an if_statement (an
--     `else if`), 1, continuing its chain;
--   expression_switch_statement, type_switch_statement: `switch`, select_statement:
--     `select`, 0, a level; expression_case, type_case, communication_case: `case`, 1 each
--     (a switch is not exhaustive), a default_case none;
--   for_statement: `for` with a for_clause or range_clause, `while` with a condition only,
--     both 1 and a level; `loop` with neither (`for {}`), 0, a level;
--   `&&` and `||` tokens of a binary_expression: `and`, `or`, 1;
--   return_, break_, continue_, goto_statement: `return`, `break`, `continue`, `goto`, 0;
--   func_literal: `func_literal`, 0.
CREATE OR REPLACE VIEW go_control_nodes AS
WITH nodes AS MATERIALIZED (
    SELECT file_id, node_id, type, parent_id FROM go_file_ast
),
typed AS MATERIALIZED (
    SELECT n.file_id, n.node_id, n.type, p.type AS parent_type
    FROM nodes n LEFT JOIN nodes p ON p.file_id = n.file_id AND p.node_id = n.parent_id
    WHERE n.type IN ('if_statement', 'expression_switch_statement', 'type_switch_statement', 'select_statement',
                     'expression_case', 'type_case', 'communication_case', 'for_statement', '&&', '||',
                     'return_statement', 'break_statement', 'continue_statement', 'goto_statement',
                     'func_literal')
),
-- Each for_statement's kind from its children: a clause, a condition, or nothing.
loops AS (
    SELECT f.file_id, f.node_id,
           CASE WHEN bool_or(c.type IN ('for_clause', 'range_clause')) THEN 'for'
                WHEN bool_or(c.type NOT IN ('for', 'block', 'comment')) THEN 'while'
                ELSE 'loop' END AS construct
    FROM typed f JOIN nodes c ON c.file_id = f.file_id AND c.parent_id = f.node_id
    WHERE f.type = 'for_statement'
    GROUP BY f.file_id, f.node_id
),
classified AS (
    SELECT t.file_id, t.node_id,
           CASE WHEN t.type = 'if_statement' AND t.parent_type = 'if_statement' THEN 'else_if'
                WHEN t.type = 'if_statement' THEN 'if'
                WHEN t.type IN ('expression_switch_statement', 'type_switch_statement') THEN 'switch'
                WHEN t.type = 'select_statement' THEN 'select'
                WHEN t.type IN ('expression_case', 'type_case', 'communication_case') THEN 'case'
                WHEN t.type = 'for_statement' THEN l.construct
                WHEN t.type = '&&' AND t.parent_type = 'binary_expression' THEN 'and'
                WHEN t.type = '||' AND t.parent_type = 'binary_expression' THEN 'or'
                WHEN t.type = 'return_statement' THEN 'return'
                WHEN t.type = 'break_statement' THEN 'break'
                WHEN t.type = 'continue_statement' THEN 'continue'
                WHEN t.type = 'goto_statement' THEN 'goto'
                WHEN t.type = 'func_literal' THEN 'func_literal' END AS construct
    FROM typed t LEFT JOIN loops l USING (file_id, node_id)
)
SELECT file_id, node_id, construct,
       CASE WHEN construct IN ('if', 'else_if', 'case', 'for', 'while', 'and', 'or') THEN 1 ELSE 0 END AS decisions,
       construct IN ('if', 'switch', 'select', 'for', 'while', 'loop') AS opens_level, NULL::BIGINT AS group_id
FROM classified
WHERE construct IS NOT NULL;

CREATE OR REPLACE VIEW go_units AS
WITH go_files AS MATERIALIZED (
    SELECT file_id, path FROM files WHERE language = 'go'
),
defs AS MATERIALIZED (
    SELECT o.file_id, o.document, o.symbol, o.start_line AS name_line, o.start_column AS name_column,
           o.enclosing_start_line, o.enclosing_start_column, o.enclosing_end_line, o.enclosing_end_column
    FROM scip_occurrences o
    WHERE o.is_definition AND NOT starts_with(o.symbol, 'local ')
      AND o.file_id IN (SELECT file_id FROM go_files)
),
-- scip-go lists a package's symbol under one of its files only, so the information is
-- joined by symbol.
info AS MATERIALIZED (
    SELECT symbol, any_value(kind) AS kind, any_value(display_name) AS display_name,
           any_value(documentation) AS documentation, any_value(signature) AS signature
    FROM scip_symbols WHERE document IS NOT NULL
    GROUP BY symbol
),
typed AS MATERIALIZED (
    SELECT d.*, i.kind AS symbol_kind, i.display_name, i.documentation, i.signature,
           CASE WHEN i.kind = 'Function' THEN 'function'
                WHEN i.kind IN ('Method', 'MethodSpecification') THEN 'method'
                WHEN i.kind IN ('Type', 'Struct', 'Interface', 'TypeAlias') THEN 'type'
                WHEN i.kind = 'Package' THEN 'file'
                WHEN i.kind IN ('Constant', 'Variable') THEN 'constant' END AS kind
    FROM defs d JOIN info i USING (symbol)
    WHERE i.kind IN ('Function', 'Method', 'MethodSpecification', 'Type', 'Struct', 'Interface', 'TypeAlias',
                     'Package', 'Constant', 'Variable')
),
nodes AS MATERIALIZED (
    SELECT file_id, node_id, type, parent_id, start_line, start_column, end_line, end_column
    FROM go_file_ast
    WHERE type IN ('source_file', 'type_declaration', 'const_declaration', 'var_declaration', 'var_spec_list',
                   'type_spec', 'type_alias', 'const_spec', 'var_spec', 'method_elem')
),
-- Each spec with its declaration (a `var ( … )` group puts a var_spec_list between them)
-- and whether it is the declaration's only spec.
specs AS MATERIALIZED (
    SELECT s.file_id, s.node_id, s.start_line, s.start_column, s.end_line, s.end_column,
           CASE WHEN p.type = 'var_spec_list' THEN p.parent_id ELSE p.node_id END AS declaration_id
    FROM nodes s JOIN nodes p ON p.file_id = s.file_id AND p.node_id = s.parent_id
    WHERE s.type IN ('type_spec', 'type_alias', 'const_spec', 'var_spec')
),
spec_ranges AS MATERIALIZED (
    SELECT s.file_id, s.node_id,
           CASE WHEN count(*) OVER (PARTITION BY s.file_id, s.declaration_id) = 1
                THEN struct_pack(sl := d.start_line, sc := d.start_column, el := d.end_line, ec := d.end_column)
                ELSE struct_pack(sl := s.start_line, sc := s.start_column, el := s.end_line, ec := s.end_column)
           END AS r,
           s.start_line, s.start_column, s.end_line, s.end_column
    FROM specs s JOIN nodes d ON d.file_id = s.file_id AND d.node_id = s.declaration_id
    UNION ALL
    -- An interface's method: its method_elem.
    SELECT file_id, node_id, struct_pack(sl := start_line, sc := start_column, el := end_line, ec := end_column),
           start_line, start_column, end_line, end_column
    FROM nodes WHERE type = 'method_elem'
),
-- The innermost spec (or method_elem) holding each name that SCIP gives no range.
spec_of AS (
    SELECT t.file_id, t.symbol, t.name_line, t.name_column, arg_max(s.r, s.node_id) AS r
    FROM typed t
    JOIN spec_ranges s ON s.file_id = t.file_id
         AND (s.start_line < t.name_line OR (s.start_line = t.name_line AND s.start_column <= t.name_column))
         AND (t.name_line < s.end_line OR (t.name_line = s.end_line AND t.name_column < s.end_column))
    WHERE t.kind IN ('type', 'constant') OR (t.kind = 'method' AND t.enclosing_start_line IS NULL)
    GROUP BY ALL
),
roots AS (
    SELECT file_id, struct_pack(sl := start_line, sc := start_column, el := end_line, ec := end_column) AS r
    FROM nodes WHERE type = 'source_file'
),
ranged AS MATERIALIZED (
    SELECT t.*,
           CASE WHEN t.kind = 'file' THEN rt.r
                WHEN t.kind IN ('function', 'method') AND t.enclosing_start_line IS NOT NULL
                    THEN struct_pack(sl := t.enclosing_start_line, sc := t.enclosing_start_column,
                                     el := t.enclosing_end_line, ec := t.enclosing_end_column)
                ELSE so.r END AS r
    FROM typed t
    LEFT JOIN spec_of so USING (file_id, symbol, name_line, name_column)
    LEFT JOIN roots rt USING (file_id)
),
-- The file unit's symbol is the prefix of the file's descriptors.
file_prefixes AS (
    SELECT file_id, any_value(symbol) AS prefix FROM typed WHERE kind = 'file' GROUP BY file_id
),
named AS MATERIALIZED (
    SELECT r.*, fp.prefix,
           CASE WHEN fp.prefix IS NOT NULL AND starts_with(r.symbol, fp.prefix) THEN substr(r.symbol, length(fp.prefix) + 1)
                ELSE regexp_extract(r.symbol, '^[^ ]+ [^ ]+ [^ ]+ [^ ]+ (.*)$', 1) END AS descriptors,
           -- A method's receiver type: its symbol without the method's descriptor.
           CASE WHEN r.symbol_kind = 'Method'
                THEN regexp_extract(r.symbol, '^(.*#)(?:`[^`]*`|[^/#.!\[\]`])+\(\)\.$', 1) END AS receiver_symbol
    FROM ranged r
    LEFT JOIN file_prefixes fp USING (file_id)
    WHERE r.r IS NOT NULL
),
-- Parameters: the signature's characters and the bracket depth before each; the
-- parameter list is the first parenthesis at depth 0 of a function's signature (`func
-- F[T any](…)`), the second of a method's (`func (*T).M(…)`, after the receiver).
signature_chars AS MATERIALIZED (
    SELECT symbol, file_id, name_line, name_column, pos, c,
           sum(CASE WHEN c IN ('(', '[', '{') THEN 1 WHEN c IN (')', ']', '}') THEN -1 ELSE 0 END)
               OVER (PARTITION BY symbol, file_id, name_line, name_column ORDER BY pos
                     ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS depth,
           is_method
    FROM (
        SELECT symbol, file_id, name_line, name_column, kind = 'method' AS is_method,
               unnest(string_split(signature, '')) AS c, generate_subscripts(string_split(signature, ''), 1) AS pos
        FROM named WHERE kind IN ('function', 'method') AND signature IS NOT NULL
    )
),
parameter_bounds AS MATERIALIZED (
    SELECT symbol, file_id, name_line, name_column, any_value(is_method) AS is_method,
           list(pos ORDER BY pos) FILTER (WHERE c = '(' AND coalesce(depth, 0) = 0) AS opens,
           list(pos ORDER BY pos) FILTER (WHERE c = ')' AND depth = 1) AS closes
    FROM signature_chars
    GROUP BY ALL
),
parameter_lists AS MATERIALIZED (
    SELECT symbol, file_id, name_line, name_column, is_method,
           opens[CASE WHEN is_method THEN 2 ELSE 1 END] AS open_pos,
           closes[CASE WHEN is_method THEN 2 ELSE 1 END] AS close_pos
    FROM parameter_bounds
),
parameters AS (
    SELECT b.symbol, b.file_id, b.name_line, b.name_column,
           (CASE WHEN count(s.c) FILTER (WHERE s.c NOT IN (' ', chr(10), chr(9))) = 0 THEN 0
                 ELSE count(*) FILTER (WHERE s.c = ',' AND s.depth = 1) + 1 END
            + CASE WHEN b.is_method THEN 1 ELSE 0 END)::INTEGER AS parameters
    FROM parameter_lists b
    LEFT JOIN signature_chars s ON s.symbol = b.symbol AND s.file_id = b.file_id AND s.name_line = b.name_line
         AND s.name_column = b.name_column AND s.pos > b.open_pos AND s.pos < b.close_pos
    WHERE b.open_pos IS NOT NULL
    GROUP BY b.symbol, b.file_id, b.name_line, b.name_column, b.is_method
),
-- Each file's package name (its file unit's).
file_packages AS (
    SELECT file_id, any_value(display_name) AS package_name FROM named WHERE kind = 'file' GROUP BY file_id
)
SELECT n.file_id, n.symbol, n.symbol_kind, n.kind, split_part(n.symbol, ' ', 3) AS package, n.display_name AS name,
       CASE WHEN n.kind = 'file' OR n.descriptors = '' THEN n.display_name
            ELSE rtrim(replace(regexp_replace(regexp_replace(n.descriptors, '\(\)\.', '.', 'g'), '[/#!]', '.', 'g'),
                               '`', ''), '.') END AS qualified_name,
       n.r.sl::INTEGER AS start_line, n.r.sc::INTEGER AS start_column,
       n.r.el::INTEGER AS end_line, n.r.ec::INTEGER AS end_column, n.name_line, n.name_column,
       g.path LIKE '%\_test.go' ESCAPE '\'
           OR coalesce(list_bool_or(list_transform(getvariable('extra_test_paths'), lambda p: g.path GLOB p)), false)
           AS test_by_rule,
       CASE WHEN n.kind = 'file' OR n.symbol_kind = 'MethodSpecification' THEN NULL
            WHEN regexp_matches(n.display_name, '^\p{Lu}') THEN 'pub' ELSE 'private' END AS visibility,
       p.parameters, n.signature, coalesce(len(n.documentation), 0) > 0 AS has_doc,
       n.kind = 'file' AS is_file_unit,
       CASE WHEN n.symbol_kind = 'Method' THEN 'impl' WHEN n.symbol_kind = 'MethodSpecification' THEN 'trait' END AS member_of,
       n.receiver_symbol AS impl_key,
       CASE WHEN n.receiver_symbol IS NOT NULL
            THEN replace(regexp_extract(n.receiver_symbol, '((?:`[^`]*`|[^/#.!\[\]`])+)#$', 1), '`', '') END AS impl_type,
       NULL::VARCHAR AS impl_trait, NULL::INTEGER AS impl_header_line,
       -- What runs it without a reference: `main` of package main and every `init`, which
       -- the runtime calls, and the files of package main, a program's root.
       CASE WHEN n.kind = 'function' AND (n.display_name = 'init' OR (n.display_name = 'main' AND fp.package_name = 'main'))
                THEN 'entry_point'
            WHEN n.kind = 'file' AND fp.package_name = 'main' THEN 'root' END AS entry
FROM named n
JOIN go_files g USING (file_id)
LEFT JOIN file_packages fp USING (file_id)
LEFT JOIN parameters p USING (symbol, file_id, name_line, name_column);
