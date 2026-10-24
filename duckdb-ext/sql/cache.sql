-- Session cache: materialize the expensive layers once and point their views at the
-- copies, so that every view and query keeps working unchanged and reads the copies.
--
-- Read after all view files. Each cached layer becomes a temporary table
-- `<view>_cache`, and the view of the same name is replaced by
-- `SELECT * FROM <view>_cache`. The views above a cached layer reference it by name, so
-- `units` and `tags` are built from `nodes_cache`.
--
-- Live mode again: re-read the view files (views/nodes.sql, lang/rust.sql,
-- views/units.sql, views/history.sql; reading all of them in order is simplest), which
-- replace the views with their live definitions. Refresh: re-read the view files, then
-- this file. Reading this file twice without the view files in between fails on purpose
-- (the first DROP removes the table the view still reads) instead of copying the copies.
-- The tables stay until the session ends or this file drops them on the next read.
--
-- Frozen at the time this file is read: the syntax and text of every source file (an
-- edit is not seen), `languages`, and the history for the `since` and `include_merges`
-- set then. Query parameters (min_shared, match_mode, ...) still apply at every query.
--
-- Why not temporary tables named like the views: DuckDB resolves `temp` before `main`,
-- so such a table would shadow the view, but re-reading a view file would then replace
-- the hidden view and leave the shadow in place. With a database file, the replaced
-- views persist pointing at temporary tables that the next session does not have: read
-- the view files in every session, as every client does anyway.

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
DROP TABLE IF EXISTS commits_cache;
CREATE TEMP TABLE commits_cache AS SELECT * FROM commits;
CREATE OR REPLACE VIEW commits AS SELECT * FROM commits_cache;

DROP TABLE IF EXISTS file_changes_cache;
CREATE TEMP TABLE file_changes_cache AS SELECT * FROM file_changes;
CREATE OR REPLACE VIEW file_changes AS SELECT * FROM file_changes_cache;

DROP TABLE IF EXISTS hunks_cache;
CREATE TEMP TABLE hunks_cache AS SELECT * FROM hunks;
CREATE OR REPLACE VIEW hunks AS SELECT * FROM hunks_cache;
