-- files(file_id, path, language, mode, blob_id): the current source files of the
-- repository.
--
-- The set is the snapshot `source_files` taken by 00_load.sql (or refresh.sql) from
-- git_tracked_files: tracked regular files, plus untracked ones that are not ignored
-- when `include_untracked` is set, with an extension listed in `languages`, minus
-- `exclude_patterns`. `mode` and `blob_id` are git's mode string and the id of the
-- content as of the snapshot (contents themselves are read live by the views below).
-- `path` is repository-relative with `/`. `file_id` is the position in the sorted list
-- (0-based) and is stable only while `source_files` is unchanged. A file that no SCIP
-- index covers (outside every Cargo workspace, or code rust-analyzer leaves out) is listed
-- here and has no units.

CREATE OR REPLACE VIEW files AS
SELECT (i - 1)::INTEGER AS file_id,
       f.path,
       getvariable('languages')[regexp_extract(f.path, '\.([^./]+)$', 1)] AS language,
       f.mode,
       f.blob_id
FROM (
    SELECT unnest(getvariable('source_files')) AS f,
           generate_subscripts(getvariable('source_files'), 1) AS i
);

-- sources(file_id, content): the text of every source file, read live.
CREATE OR REPLACE VIEW sources AS
SELECT f.file_id, t.content
FROM read_text(getvariable('source_paths')) t
JOIN files f ON f.path = substr(t.filename, length(getvariable('repo')) + 2);

-- source_lines(file_id, line, text, is_blank, is_comment, nonblank_through): every
-- 1-based line of every source file with its text (without the newline). is_blank: the
-- line holds only ASCII space, tab, CR, VT or FF. is_comment: its first non-blank
-- characters are `//` (line and doc comments; a block comment's inner lines are not
-- recognised). nonblank_through counts the non-blank lines up to and including it, so
-- the non-blank lines of [s, e] are nonblank_through(e) - nonblank_through(s - 1).
CREATE OR REPLACE VIEW source_lines AS
WITH lines AS (
    SELECT file_id, unnest(string_split(content, chr(10))) AS text,
           generate_subscripts(string_split(content, chr(10)), 1) AS line
    FROM sources
)
SELECT file_id, line::INTEGER AS line, text,
       regexp_full_match(text, '[ \t\r\x0b\x0c]*') AS is_blank,
       regexp_matches(text, '^\s*//') AS is_comment,
       sum(CASE WHEN regexp_full_match(text, '[ \t\r\x0b\x0c]*') THEN 0 ELSE 1 END)
           OVER (PARTITION BY file_id ORDER BY line) AS nonblank_through
FROM lines;

-- file_ast(file_id, node_id, type, semantic_type, flags, name, qualified_name,
--          signature_type, parameters, modifiers, annotations, receiver, language,
--          start_line, end_line, start_column, end_column, start_byte, end_byte, parent_id,
--          depth, sibling_index, children_count, descendant_count, scope): the complete
-- tree-sitter syntax tree of every source file, one row per node, as sitting_duck's
-- read_ast gives it (00_load.sql loads sitting_duck), parsed live. Nothing is filtered:
-- what to count is decided by the queries. refresh.sql keeps the trees per content id
-- (blob_id) and parses only the files whose content id it does not hold.
--
-- The columns are read_ast's with source := 'full' (columns and byte offsets), without
-- file_path (file_id instead) and peek. node_id is the node's 0-based position in a
-- pre-order walk of the file's tree (the root, source_file, is 0), so its descendants are
-- node_id + 1 .. node_id + descendant_count; parent_id, depth (the root's is 0) and
-- sibling_index (0-based, among all children, anonymous tokens included) place it in the
-- tree. type is the tree-sitter node type, of named nodes (`if_expression`) and anonymous
-- tokens (`if`, `&&`, `?`) alike; tree-sitter's field names and is_named are not given.
-- Lines are 1-based and inclusive, columns 1-based with the end the column after the last
-- byte (UTF-8 bytes, as SCIP's positions here), bytes 0-based with the end exclusive.
-- flags are sitting_duck's bits (is_syntax_only(flags), is_scope(flags), ...);
-- semantic_type, name, qualified_name, signature_type, parameters, modifiers,
-- annotations, receiver and scope (the node ids of the enclosing scopes, and their
-- stack) are sitting_duck's readings of the node in its file. language is the grammar
-- read_ast chose from the file's extension.
--
-- A macro invocation's arguments are a token_tree of tokens (an `if` there is a keyword
-- token, not an if_expression), and so is a macro_rules! body.
CREATE OR REPLACE VIEW file_ast AS
SELECT f.file_id, a.* EXCLUDE (file_path)
FROM read_ast(getvariable('source_paths'), source := 'full', peek := 'none') a
JOIN files f ON f.path = substr(a.file_path, length(getvariable('repo')) + 2);
