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
-- They are the column sets of the scip extension's read_scip, read_scip_symbols and
-- read_scip_metadata. Until it is available they are computed from a stand-in (below).
--
-- The views the queries read follow this layer's conventions:
--   scip_occurrences(file_id, document, symbol, start_line, start_column, end_line,
--       end_column, roles, is_definition, is_reference, is_import, is_write, is_read,
--       is_generated, is_test, is_forward_definition, enclosing_start_line,
--       enclosing_start_column, enclosing_end_line, enclosing_end_column)
--   scip_symbols(document, symbol, kind, display_name, enclosing_symbol, documentation,
--       relationship_count)
-- Lines are 1-based and inclusive, columns 1-based (start_char + 1; the end is the
-- column after the last character, as nodes' end_column), counted in the index's
-- text_document_encoding units (rust-analyzer: UTF-8 bytes). `file_id` comes from
-- joining `document` to files.path and is NULL for a document that is not a source
-- file. SCIP local symbols (`local N`) are unique only within a document, so they are
-- qualified as `local N <document>`, in occurrences, symbols and enclosing_symbol.
-- is_reference is NOT is_definition (the spike's definition: every occurrence without
-- the Definition role, imports included). Occurrences without a symbol are dropped.
-- A non-local symbol can be listed under several documents (rust-analyzer gives each
-- binary target's `crate/` symbol to each of its documents): scip_symbols keeps one row
-- per listing.

-- Stand-in until read_scip is available: the JSON that the Sourcegraph scip CLI prints
-- for the index (`scip print --json`, v0.10.0; Go's encoding of the protobuf structs:
-- snake_case names, enums as numbers, empty fields omitted), read with read_json.
-- `scip_json` (default `scip_index` || '.json') must exist when this file is read.
SET VARIABLE scip_json = coalesce(getvariable('scip_json'), getvariable('scip_index') || '.json');

CREATE OR REPLACE VIEW scip_json_index AS
SELECT * FROM read_json(getvariable('scip_json'), format := 'unstructured', maximum_object_size := 2147483647,
    columns := {
        metadata: 'STRUCT(version INTEGER, tool_info STRUCT(name VARCHAR, version VARCHAR, arguments VARCHAR[]), project_root VARCHAR, text_document_encoding INTEGER)',
        documents: 'STRUCT(relative_path VARCHAR, occurrences STRUCT("range" INTEGER[], symbol VARCHAR, symbol_roles INTEGER, enclosing_range INTEGER[])[], symbols STRUCT(symbol VARCHAR, kind INTEGER, display_name VARCHAR, enclosing_symbol VARCHAR, documentation VARCHAR[], relationships JSON[])[])[]',
        external_symbols: 'STRUCT(symbol VARCHAR, kind INTEGER, display_name VARCHAR, enclosing_symbol VARCHAR, documentation VARCHAR[], relationships JSON[])[]'
    });

-- A SCIP range is [line, start, end] on one line or [start_line, start, end_line, end].
CREATE OR REPLACE VIEW scip_index_occurrences AS
WITH docs AS (
    SELECT unnest(documents) AS d FROM scip_json_index
),
occ AS (
    SELECT d.relative_path AS document, unnest(d.occurrences) AS o FROM docs
)
SELECT document, o.symbol,
       o.range[1] AS start_line, o.range[2] AS start_char,
       CASE WHEN len(o.range) = 3 THEN o.range[1] ELSE o.range[3] END AS end_line,
       o.range[len(o.range)] AS end_char,
       coalesce(o.symbol_roles, 0) AS roles,
       (coalesce(o.symbol_roles, 0) & 1) <> 0 AS is_definition,
       (coalesce(o.symbol_roles, 0) & 2) <> 0 AS is_import,
       (coalesce(o.symbol_roles, 0) & 4) <> 0 AS is_write,
       (coalesce(o.symbol_roles, 0) & 8) <> 0 AS is_read,
       (coalesce(o.symbol_roles, 0) & 16) <> 0 AS is_generated,
       (coalesce(o.symbol_roles, 0) & 32) <> 0 AS is_test,
       (coalesce(o.symbol_roles, 0) & 64) <> 0 AS is_forward_definition,
       o.enclosing_range[1] AS enclosing_start_line, o.enclosing_range[2] AS enclosing_start_char,
       CASE WHEN len(o.enclosing_range) = 3 THEN o.enclosing_range[1] ELSE o.enclosing_range[3] END AS enclosing_end_line,
       o.enclosing_range[len(o.enclosing_range)] AS enclosing_end_char
FROM occ;

CREATE OR REPLACE VIEW scip_index_symbols AS
WITH idx AS MATERIALIZED (
    SELECT documents, external_symbols FROM scip_json_index
),
docs AS (
    SELECT unnest(documents) AS d FROM idx
),
listed AS (
    SELECT d.relative_path AS document, unnest(d.symbols) AS s FROM docs
    UNION ALL
    SELECT NULL, unnest(external_symbols) FROM idx
)
SELECT document, s.symbol, s.kind, s.display_name, s.enclosing_symbol, s.documentation,
       coalesce(len(s.relationships), 0) AS relationship_count
FROM listed;

CREATE OR REPLACE VIEW scip_index_metadata AS
SELECT metadata.version, metadata.tool_info.name AS tool_name, metadata.tool_info.version AS tool_version,
       metadata.tool_info.arguments AS tool_arguments, metadata.project_root,
       metadata.text_document_encoding,
       len(documents) AS document_count,
       list_sum(list_transform(documents, lambda d: coalesce(len(d.occurrences), 0))) AS occurrence_count,
       coalesce(len(external_symbols), 0) AS external_symbol_count
FROM scip_json_index;
-- End of the stand-in.

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
       kind::INTEGER AS kind, display_name::VARCHAR AS display_name,
       (CASE WHEN starts_with(enclosing_symbol, 'local ') THEN enclosing_symbol || ' ' || document
             ELSE enclosing_symbol END)::VARCHAR AS enclosing_symbol,
       documentation::VARCHAR[] AS documentation, relationship_count::INTEGER AS relationship_count
FROM scip_index_symbols;

CREATE OR REPLACE VIEW scip_occurrences AS
SELECT f.file_id, o.*
FROM scip_document_occurrences o
LEFT JOIN files f ON f.path = o.document;

CREATE OR REPLACE VIEW scip_symbols AS SELECT * FROM scip_document_symbols;
