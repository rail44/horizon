-- Session cache: materialize the expensive layers once and point their views at the
-- copies, so that every view and query keeps working unchanged and reads the copies.
-- The layers are nodes, units, tags, the three history views, blob_files, blob_units,
-- blob_blank_lines, commit_order and unit_successions (views/lineage.sql) and, from views/scip.sql
-- (optional), scip_occurrences and scip_symbols.
--
-- Read after all view files. Each cached layer becomes a temporary table
-- `<view>_cache`, and the view of the same name is replaced by
-- `SELECT * FROM <view>_cache`. The views above a cached layer reference it by name, so
-- `units` and `tags` are built from `nodes_cache`.
--
-- Live mode again: re-read the view files (views/nodes.sql, views/blobs.sql, lang/rust.sql,
-- views/units.sql, views/history.sql; reading all of them in order is simplest), which
-- replace the views with their live definitions. Refresh: re-read the view files, then
-- this file. Reading this file twice without the view files in between fails on purpose
-- (the first DROP removes the table the view still reads) instead of copying the copies.
-- The tables stay until the session ends or this file drops them on the next read.
--
-- Frozen at the time this file is read: the syntax and text of every source file (an
-- edit is not seen), `languages`, and the history and blob layer for the `since` and
-- `include_merges` set then. Query parameters (min_shared, match_mode, ...) still apply at every query.
--
-- Why not temporary tables named like the views: DuckDB resolves `temp` before `main`,
-- so such a table would shadow the view, but re-reading a view file would then replace
-- the hidden view and leave the shadow in place. With a database file, the replaced
-- views persist pointing at temporary tables that the next session does not have: read
-- the view files in every session, as every client does anyway.
--
-- refresh.sql is the persistent, incremental counterpart. The two are not mixed: this
-- file refuses to run while the views read the persistent cache (re-read the view
-- files first), and refresh.sql refuses while they read these copies.

-- (duckdb_views() returns the view text normalized, the database name quoted.)
SELECT CASE WHEN EXISTS (SELECT 1 FROM duckdb_views()
                         WHERE view_name = 'nodes' AND regexp_matches(sql, '"?cache"?\.nodes\b'))
    THEN error('cache.sql: the views read the persistent cache of refresh.sql; re-read the view files first')
END;

DROP TABLE IF EXISTS nodes_cache;
CREATE TEMP TABLE nodes_cache AS SELECT * FROM nodes;
CREATE OR REPLACE VIEW nodes AS SELECT * FROM nodes_cache;

DROP TABLE IF EXISTS units_cache;
CREATE TEMP TABLE units_cache AS SELECT * FROM units;
CREATE OR REPLACE VIEW units AS SELECT * FROM units_cache;

DROP TABLE IF EXISTS tags_cache;
CREATE TEMP TABLE tags_cache AS SELECT * FROM tags;
CREATE OR REPLACE VIEW tags AS SELECT * FROM tags_cache;

-- The three history views together, so that all of them describe the same `since` and
-- `include_merges` (no query reads hunks yet).
-- commit_order numbers the commits in the order git_commits emits them, so it is copied
-- from the live view, before commits reads its copy.
DROP TABLE IF EXISTS commit_order_cache;
CREATE TEMP TABLE commit_order_cache AS SELECT * FROM commit_order;
CREATE OR REPLACE VIEW commit_order AS SELECT * FROM commit_order_cache;

DROP TABLE IF EXISTS commits_cache;
CREATE TEMP TABLE commits_cache AS SELECT * FROM commits;
CREATE OR REPLACE VIEW commits AS SELECT * FROM commits_cache;

DROP TABLE IF EXISTS file_changes_cache;
CREATE TEMP TABLE file_changes_cache AS SELECT * FROM file_changes;
CREATE OR REPLACE VIEW file_changes AS SELECT * FROM file_changes_cache;

DROP TABLE IF EXISTS hunks_cache;
CREATE TEMP TABLE hunks_cache AS SELECT * FROM hunks;
CREATE OR REPLACE VIEW hunks AS SELECT * FROM hunks_cache;

-- The blob layer: blob_files (for the same `since` and `include_merges` as the history
-- copies), blob_units, which parses every blob once (the live blob_nodes) to build the
-- copy, and blob_blank_lines, which reads every blob's text once. blob_nodes itself is not copied (about 31 million rows on this repository)
-- and stays live; unit_changes reads the copies.
DROP TABLE IF EXISTS blob_files_cache;
CREATE TEMP TABLE blob_files_cache AS SELECT * FROM blob_files;
CREATE OR REPLACE VIEW blob_files AS SELECT * FROM blob_files_cache;

DROP TABLE IF EXISTS blob_units_cache;
CREATE TEMP TABLE blob_units_cache AS SELECT * FROM blob_units;
CREATE OR REPLACE VIEW blob_units AS SELECT * FROM blob_units_cache;

DROP TABLE IF EXISTS blob_blank_lines_cache;
CREATE TEMP TABLE blob_blank_lines_cache AS SELECT * FROM blob_blank_lines;
CREATE OR REPLACE VIEW blob_blank_lines AS SELECT * FROM blob_blank_lines_cache;

-- The successions (views/lineage.sql), derived over the copies above and the blob texts
-- (read once, here). unit_lineage, unit_history and the history-based queries read the
-- copy.
DROP TABLE IF EXISTS unit_successions_cache;
CREATE TEMP TABLE unit_successions_cache AS SELECT * FROM unit_successions;
CREATE OR REPLACE VIEW unit_successions AS SELECT * FROM unit_successions_cache;

-- SCIP (views/scip.sql, optional): the rows of scip_document_occurrences and
-- scip_document_symbols, which views/scip.sql defines and nothing replaces; scip_occurrences
-- joins file_id at query time, as in views/scip.sql. When views/scip.sql was not read,
-- empty views of the same columns stand in (they persist until it replaces them), so
-- that the copies, and the two views, exist and are empty.
DROP TABLE IF EXISTS scip_occurrences_cache;
-- The same two statements as in refresh.sql.
CREATE VIEW IF NOT EXISTS scip_document_occurrences AS
SELECT NULL::VARCHAR AS document, NULL::VARCHAR AS symbol, NULL::INTEGER AS start_line,
       NULL::INTEGER AS start_column, NULL::INTEGER AS end_line, NULL::INTEGER AS end_column,
       NULL::INTEGER AS roles, NULL::BOOLEAN AS is_definition, NULL::BOOLEAN AS is_reference,
       NULL::BOOLEAN AS is_import, NULL::BOOLEAN AS is_write, NULL::BOOLEAN AS is_read,
       NULL::BOOLEAN AS is_generated, NULL::BOOLEAN AS is_test, NULL::BOOLEAN AS is_forward_definition,
       NULL::INTEGER AS enclosing_start_line, NULL::INTEGER AS enclosing_start_column,
       NULL::INTEGER AS enclosing_end_line, NULL::INTEGER AS enclosing_end_column
WHERE false;
CREATE VIEW IF NOT EXISTS scip_document_symbols AS
SELECT NULL::VARCHAR AS document, NULL::VARCHAR AS symbol, NULL::VARCHAR AS kind,
       NULL::VARCHAR AS display_name, NULL::VARCHAR AS enclosing_symbol, NULL::VARCHAR[] AS documentation,
       NULL::INTEGER AS relationship_count
WHERE false;
CREATE TEMP TABLE scip_occurrences_cache AS SELECT * FROM scip_document_occurrences;
CREATE OR REPLACE VIEW scip_occurrences AS
SELECT f.file_id, o.*
FROM scip_occurrences_cache o
LEFT JOIN files f ON f.path = o.document;

DROP TABLE IF EXISTS scip_symbols_cache;
CREATE TEMP TABLE scip_symbols_cache AS SELECT * FROM scip_document_symbols;
CREATE OR REPLACE VIEW scip_symbols AS SELECT * FROM scip_symbols_cache;
