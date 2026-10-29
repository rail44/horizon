-- SCIP facts: every definition and reference occurrence of the source and the symbols'
-- information, from the SCIP indexes of the repository's Cargo workspaces (`code-db index`
-- runs `rust-analyzer scip` once per directory whose Cargo.toml has a `[workspace]`
-- table, README.md, Running). The indexes are the list `scip_indexes` (00_load.sql); they
-- describe the trees they were generated from, which nothing checks against `files`.
--
-- scip_index_occurrences, scip_index_symbols and scip_index_metadata are the indexes' rows
-- as SCIP encodes them (0-based lines and characters, `local N` symbols unqualified,
-- documents relative to each index's project root), with the index file in index_path:
--   scip_index_occurrences(document, symbol, start_line, start_char, end_line, end_char,
--       roles, is_definition, is_import, is_write, is_read, is_generated, is_test,
--       is_forward_definition, enclosing_start_line, enclosing_start_char,
--       enclosing_end_line, enclosing_end_char, index_path)
--   scip_index_symbols(document, symbol, kind, display_name, enclosing_symbol,
--       documentation, relationship_count, signature, index_path): `document` is NULL for
--       an external symbol; signature is the text of signature_documentation
--   scip_index_metadata(version, tool_name, tool_version, tool_arguments, project_root,
--       text_document_encoding, document_count, occurrence_count, external_symbol_count,
--       index_path)
-- They are read_scip, read_scip_symbols and read_scip_metadata of the scip_index
-- extension over the list (duckdb-ext/README.md, "scip_index").
--
-- The views the other files read follow this layer's conventions:
--   scip_occurrences(file_id, document, symbol, start_line, start_column, end_line,
--       end_column, is_definition, is_reference, enclosing_start_line,
--       enclosing_start_column, enclosing_end_line, enclosing_end_column)
--   scip_symbols(document, symbol, kind, display_name, documentation, signature)
-- `document` is repository-relative: the index's project root (scip_index_roots) joined
-- with the document's relative path. Lines are 1-based and inclusive, columns 1-based
-- (start_char + 1; the end is the column after the last character), counted in the unit
-- of the document's position encoding (rust-analyzer: UTF-8 bytes). `file_id` comes from
-- joining `document` to files.path and is NULL for a document that is not a source file
-- (excluded by the settings, for example). SCIP local symbols (`local N`) are unique
-- only within a document, so they are qualified as `local N <document>`. is_reference is
-- NOT is_definition: rust-analyzer 1.99.1 sets no role bit but Definition.
-- Occurrences without a symbol are dropped. A non-local symbol can be listed under
-- several documents (a package's `crate/` symbol under the root file of each of its
-- targets): scip_symbols keeps one row per listing.

CREATE OR REPLACE VIEW scip_index_occurrences AS SELECT * FROM read_scip(getvariable('scip_indexes'));

CREATE OR REPLACE VIEW scip_index_symbols AS SELECT * FROM read_scip_symbols(getvariable('scip_indexes'));

CREATE OR REPLACE VIEW scip_index_metadata AS SELECT * FROM read_scip_metadata(getvariable('scip_indexes'));

-- scip_index_roots(index_path, project_root, prefix): each index with its project root
-- as a repository-relative directory ('' for the repository root). An index whose
-- project root is not inside `repo` has a NULL prefix, and its documents match no file.
CREATE OR REPLACE VIEW scip_index_roots AS
WITH roots AS (
    SELECT index_path, project_root,
           rtrim(regexp_replace(project_root, '^file://', ''), '/') AS root
    FROM scip_index_metadata
)
SELECT index_path, project_root,
       CASE WHEN root = getvariable('repo') THEN ''
            WHEN starts_with(root, getvariable('repo') || '/') THEN substr(root, length(getvariable('repo')) + 2)
       END AS prefix
FROM roots;

-- scip_document_occurrences and scip_document_symbols: the rows of scip_occurrences
-- (without file_id) and scip_symbols, which refresh.sql copies and then replaces those two
-- views with views over its copies, never these.
CREATE OR REPLACE VIEW scip_document_occurrences AS
WITH roots AS MATERIALIZED (
    SELECT index_path, prefix FROM scip_index_roots
),
occurrences AS (
    SELECT (CASE WHEN r.prefix = '' THEN o.document ELSE r.prefix || '/' || o.document END)::VARCHAR AS document, o.*
           EXCLUDE (document, index_path)
    FROM scip_index_occurrences o JOIN roots r USING (index_path)
    WHERE o.symbol <> ''
)
SELECT document,
       (CASE WHEN starts_with(symbol, 'local ') THEN symbol || ' ' || document ELSE symbol END)::VARCHAR AS symbol,
       (start_line + 1)::INTEGER AS start_line, (start_char + 1)::INTEGER AS start_column,
       (end_line + 1)::INTEGER AS end_line, (end_char + 1)::INTEGER AS end_column,
       is_definition, NOT is_definition AS is_reference,
       (enclosing_start_line + 1)::INTEGER AS enclosing_start_line,
       (enclosing_start_char + 1)::INTEGER AS enclosing_start_column,
       (enclosing_end_line + 1)::INTEGER AS enclosing_end_line,
       (enclosing_end_char + 1)::INTEGER AS enclosing_end_column
FROM occurrences;

CREATE OR REPLACE VIEW scip_document_symbols AS
WITH roots AS MATERIALIZED (
    SELECT index_path, prefix FROM scip_index_roots
),
symbols AS (
    SELECT (CASE WHEN s.document IS NULL OR r.prefix = '' THEN s.document
                 ELSE r.prefix || '/' || s.document END)::VARCHAR AS document, s.* EXCLUDE (document, index_path)
    FROM scip_index_symbols s JOIN roots r USING (index_path)
)
SELECT document,
       (CASE WHEN starts_with(symbol, 'local ') THEN symbol || ' ' || document ELSE symbol END)::VARCHAR AS symbol,
       kind::VARCHAR AS kind, display_name::VARCHAR AS display_name,
       documentation::VARCHAR[] AS documentation, signature::VARCHAR AS signature
FROM symbols;

CREATE OR REPLACE VIEW scip_occurrences AS
SELECT f.file_id, o.*
FROM scip_document_occurrences o
LEFT JOIN files f ON f.path = o.document;

CREATE OR REPLACE VIEW scip_symbols AS SELECT * FROM scip_document_symbols;
