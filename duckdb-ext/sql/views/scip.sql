-- SCIP reference facts: which symbol each occurrence in the source names, where, and in
-- which role, from a SCIP index of the repository (rust-analyzer: `rust-analyzer scip .
-- --output target/code-db/index.scip`).
--
-- Optional, read after the other view files: nothing else depends on it, and the index
-- file `scip_index` (default <repo>/target/code-db/index.scip, 00_load.sql) must exist
-- when this file is read. cache.sql and refresh.sql work whether or not it was read
-- (without it their SCIP views are empty). The index is a separate artifact: it
-- describes the tree it was generated from, which is not checked against `files`.
--
-- scip_index_occurrences, scip_index_symbols and scip_index_metadata are the index's rows
-- as SCIP encodes them (0-based lines and characters, `local N` symbols unqualified):
--   scip_index_occurrences(document, symbol, start_line, start_char, end_line, end_char,
--       roles, is_definition, is_import, is_write, is_read, is_generated, is_test,
--       is_forward_definition, enclosing_start_line, enclosing_start_char,
--       enclosing_end_line, enclosing_end_char)
--   scip_index_symbols(document, symbol, kind, display_name, enclosing_symbol,
--       documentation, relationship_count): `document` is NULL for an external symbol
--   scip_index_metadata(version, tool_name, tool_version, tool_arguments, project_root,
--       text_document_encoding, document_count, occurrence_count, external_symbol_count)
-- They are read_scip, read_scip_symbols and read_scip_metadata of the scip_index
-- extension: `kind` and `text_document_encoding` are enum names, and absent strings are
-- '' (display_name, enclosing_symbol), absent documentation an empty list.
--
-- The views the queries read follow this layer's conventions:
--   scip_occurrences(file_id, document, symbol, start_line, start_column, end_line,
--       end_column, roles, is_definition, is_reference, is_import, is_write, is_read,
--       is_generated, is_test, is_forward_definition, enclosing_start_line,
--       enclosing_start_column, enclosing_end_line, enclosing_end_column)
--   scip_symbols(document, symbol, kind, display_name, enclosing_symbol, documentation,
--       relationship_count)
-- Lines are 1-based and inclusive, columns 1-based (start_char + 1; the end is the
-- column after the last character, as nodes' end_column), counted in the unit of the
-- document's position encoding (rust-analyzer: UTF-8 bytes). `file_id` comes from
-- joining `document` to files.path and is NULL for a document that is not a source
-- file. SCIP local symbols (`local N`) are unique only within a document, so they are
-- qualified as `local N <document>`, in occurrences, symbols and enclosing_symbol.
-- is_reference is NOT is_definition (the spike's definition: every occurrence without
-- the Definition role, imports included). Occurrences without a symbol are dropped.
-- A non-local symbol can be listed under several documents (rust-analyzer lists a
-- package's `crate/` symbol under the root file of each of its targets): scip_symbols
-- keeps one row per listing.

-- The scip_index extension's functions (duckdb-ext/README.md, "scip_index"), loaded by
-- 00_load.sql. They open the file at bind time: a missing index fails this file.
CREATE OR REPLACE VIEW scip_index_occurrences AS SELECT * FROM read_scip(getvariable('scip_index'));

CREATE OR REPLACE VIEW scip_index_symbols AS SELECT * FROM read_scip_symbols(getvariable('scip_index'));

CREATE OR REPLACE VIEW scip_index_metadata AS SELECT * FROM read_scip_metadata(getvariable('scip_index'));

-- scip_document_occurrences and scip_document_symbols: the rows of scip_occurrences
-- (without file_id) and of scip_symbols, which cache.sql and refresh.sql copy; those
-- two files replace scip_occurrences and scip_symbols, never these. The types are fixed
-- here so that the copies do not depend on the source's. The qualification of local
-- symbols is written out rather than a macro: refresh.sql's derive_key covers every
-- macro, and a macro defined here would re-derive all units whenever this file is read.
CREATE OR REPLACE VIEW scip_document_occurrences AS
SELECT document::VARCHAR AS document,
       (CASE WHEN starts_with(symbol, 'local ') THEN symbol || ' ' || document ELSE symbol END)::VARCHAR AS symbol,
       (start_line + 1)::INTEGER AS start_line, (start_char + 1)::INTEGER AS start_column,
       (end_line + 1)::INTEGER AS end_line, (end_char + 1)::INTEGER AS end_column,
       roles::INTEGER AS roles, is_definition, NOT is_definition AS is_reference, is_import, is_write,
       is_read, is_generated, is_test, is_forward_definition,
       (enclosing_start_line + 1)::INTEGER AS enclosing_start_line,
       (enclosing_start_char + 1)::INTEGER AS enclosing_start_column,
       (enclosing_end_line + 1)::INTEGER AS enclosing_end_line,
       (enclosing_end_char + 1)::INTEGER AS enclosing_end_column
FROM scip_index_occurrences
WHERE symbol <> '';

CREATE OR REPLACE VIEW scip_document_symbols AS
SELECT document::VARCHAR AS document,
       (CASE WHEN starts_with(symbol, 'local ') THEN symbol || ' ' || document ELSE symbol END)::VARCHAR AS symbol,
       kind::VARCHAR AS kind, display_name::VARCHAR AS display_name,
       (CASE WHEN starts_with(enclosing_symbol, 'local ') THEN enclosing_symbol || ' ' || document
             ELSE enclosing_symbol END)::VARCHAR AS enclosing_symbol,
       documentation::VARCHAR[] AS documentation, relationship_count::INTEGER AS relationship_count
FROM scip_index_symbols;

CREATE OR REPLACE VIEW scip_occurrences AS
SELECT f.file_id, o.*
FROM scip_document_occurrences o
LEFT JOIN files f ON f.path = o.document;

CREATE OR REPLACE VIEW scip_symbols AS SELECT * FROM scip_document_symbols;
