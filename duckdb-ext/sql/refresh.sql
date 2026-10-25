-- Persistent cache: bring the cache file up to date with the repository, then point
-- nodes, units, tags, commits, file_changes, hunks, blob_files, blob_nodes, blob_units,
-- scip_occurrences and scip_symbols at it (unit_changes reads them).
--
-- Read after all view files, in any session and as often as wanted: each read takes a
-- new file snapshot (as 00_load.sql does), re-parses only the files whose content id
-- changed, and appends the commits made since the cached head. The file is
-- `cache_path` (default <repo>/target/code-db/cache.duckdb; its directory must exist),
-- attached as the database `cache`. Live mode again: re-read the view files.
--
-- Keys. Syntax and the rows derived from it are cached per file, under the file's
-- repository-relative path and its git blob id: `cache.nodes_files` holds one row per
-- cached file, `cache.nodes`, `cache.units` and `cache.tags` carry the path. A file
-- whose (path, blob_id, language) is not in nodes_files is re-parsed and its units and
-- tags recomputed; a cached path that is no longer a source file is dropped. file_id
-- and unit_id are positions in the current file list, so the views assign them at query
-- time, joining on the path and the blob id.
--
-- Full rebuilds. The syntax layers are rebuilt when the cache has no meta row, when
-- the previous refresh did not finish (meta.state is not 'ok'), when `repo` or the
-- sitting_duck version differs; units and tags alone are recomputed for every file
-- when the definitions of the derived views (lang/*.sql, views/units.sql, the text
-- views of views/files.sql) differ. History is rebuilt when `since`, `include_merges`
-- or `languages` differ or the cached head is not among the commits since..HEAD (a
-- rewritten or reset branch); otherwise the commits cached_head..HEAD are appended.
--
-- Blobs. A blob's content never changes, so its parse (`cache.blob_nodes`) and units
-- (`cache.blob_units`) are keyed by blob_id alone and only ever added: a blob is parsed
-- when it is in the cached blob_files and not in `cache.blob_parsed`, and its units are
-- derived when it is not in `cache.blob_derived`. `cache.blob_files` follows the
-- history: appended with the commits cached_head..HEAD, rebuilt with it (keeping the
-- parsed blobs) and also when the units of every blob are re-derived, since that reads
-- every blob file again and the rebuild exports the missing ones. Blobs that leave the
-- range (an amended commit) stay in the cache and are not shown.
--
-- SCIP. views/scip.sql is optional. When it was read, `cache.scip_occurrences` and
-- `cache.scip_symbols` hold its rows (without file_id, which the view joins at query
-- time), rebuilt whole when the key changes: the index file's path and content hash
-- (hash() over read_blob, about 20 ms for 30 MB) and the text of the views that shape
-- the rows. When it was not read, the tables are emptied and scip_occurrences and
-- scip_symbols are empty views.
-- meta records what happened. A different cache schema version is an error: delete
-- the file.
--
-- Not to be mixed with cache.sql (the session-only copy): each refuses to run while
-- the views read the other's tables. Statements run one by one (no transaction), so
-- the per-layer times can be taken from now(); an interrupted refresh leaves
-- meta.state = 'refreshing' and the next one rebuilds. With the DuckDB CLI, use -bail
-- (or .bail on) so that a failing check stops the file.

-- Preconditions.
SELECT CASE
    WHEN NOT EXISTS (SELECT 1 FROM duckdb_views() WHERE view_name = 'derived_units')
        THEN error('refresh.sql: read the view files first')
    WHEN EXISTS (SELECT 1 FROM duckdb_views() WHERE view_name = 'nodes' AND sql LIKE '%nodes\_cache%' ESCAPE '\')
        THEN error('refresh.sql: the views read the session copies of cache.sql; re-read the view files first')
END;

SET VARIABLE refresh_t_start = epoch(now());

-- File snapshot: the same two statements as in 00_load.sql.
SET VARIABLE source_files = (
    SELECT coalesce(list({'path': path, 'mode': mode, 'blob_id': blob_id} ORDER BY path), [])
    FROM tracked_source_files
);
SET VARIABLE source_paths = list_transform(getvariable('source_files'),
                                           lambda f: getvariable('repo') || '/' || f.path);

ATTACH IF NOT EXISTS getvariable('cache_path') AS cache;
SELECT CASE WHEN getvariable('cache_path') LIKE '/%'
             AND (SELECT path FROM duckdb_databases() WHERE database_name = 'cache') <> getvariable('cache_path')
    THEN error('refresh.sql: the database cache is attached from another file; DETACH cache first')
END;

-- Layout (schema version 3).
CREATE TABLE IF NOT EXISTS cache.meta (
    schema_version INTEGER,
    repo VARCHAR,
    state VARCHAR,              -- 'ok', or 'refreshing' while a refresh runs
    built_at TIMESTAMP,         -- the last full rebuild of the syntax layers
    refreshed_at TIMESTAMP,
    syntax_key VARCHAR,         -- sitting_duck version
    derive_key VARCHAR,         -- md5 of the derived views' definitions
    history_key VARCHAR,        -- since, include_merges, languages, blob_dir
    head VARCHAR,               -- HEAD when the history was cached; NULL for no commits
    -- the last refresh
    syntax VARCHAR,             -- 'rebuilt' or 'incremental'
    derived VARCHAR,            -- 'rebuilt' or 'incremental'
    history VARCHAR,            -- 'rebuilt' or 'appended'
    files_added INTEGER,        -- paths not cached before
    files_changed INTEGER,      -- cached paths whose blob id (or language) changed
    files_removed INTEGER,      -- cached paths that are no longer source files
    files_unchanged INTEGER,
    nodes_parsed BIGINT,        -- node rows inserted (read_ast over the changed files)
    units_inserted BIGINT,
    tags_inserted BIGINT,
    commits_added BIGINT,
    blob_history VARCHAR,       -- 'rebuilt' or 'appended' (cache.blob_files)
    blob_rows_added BIGINT,     -- rows inserted into cache.blob_files
    blobs_parsed BIGINT,        -- blobs read by read_ast in this refresh
    blob_bytes_parsed BIGINT,   -- their size in bytes
    blob_nodes_parsed BIGINT,
    blobs_derived BIGINT,       -- blobs whose units were derived
    blob_units_inserted BIGINT,
    blobs_cached BIGINT,        -- all parsed blobs in the file, in the range or not
    scip_key VARCHAR,           -- scip_index, its content hash and the shaping views' md5; NULL without views/scip.sql
    scip_cache VARCHAR,         -- 'rebuilt' or 'kept'
    scip_occurrences_inserted BIGINT,
    scip_symbols_inserted BIGINT,
    seconds STRUCT(files DOUBLE, nodes DOUBLE, units DOUBLE, tags DOUBLE, commits DOUBLE,
                   file_changes DOUBLE, hunks DOUBLE, blob_files DOUBLE, blob_nodes DOUBLE,
                   blob_units DOUBLE, scip DOUBLE, checkpoint DOUBLE, total DOUBLE)
);
CREATE TABLE IF NOT EXISTS cache.nodes_files (path VARCHAR, blob_id VARCHAR, language VARCHAR);
-- read_ast's own columns, from a read of a path that does not exist (no file is read).
CREATE TABLE IF NOT EXISTS cache.nodes AS
SELECT ''::VARCHAR AS path, *
FROM read_ast([getvariable('cache_path') || '.no-such-file.rs'], source := 'full', peek := 'none',
              context := 'none', ignore_errors := true);
-- The columns of units and tags (views/units.sql), keyed by path; unit_id and
-- parent_unit_id are kept as positions within the file (see below).
CREATE TABLE IF NOT EXISTS cache.units (path VARCHAR, unit_ord BIGINT, parent_unit_ord BIGINT,
    node_id BIGINT, kind VARCHAR, name VARCHAR, qualified_name VARCHAR, start_line BIGINT,
    end_line BIGINT, loc HUGEINT);
CREATE TABLE IF NOT EXISTS cache.tags (path VARCHAR, node_id BIGINT, tag VARCHAR, start_line BIGINT,
    end_line BIGINT);
CREATE TABLE IF NOT EXISTS cache.commits (sha VARCHAR, author_time TIMESTAMP, author_name VARCHAR,
    subject VARCHAR, parent_count INTEGER, is_merge BOOLEAN);
CREATE TABLE IF NOT EXISTS cache.file_changes (sha VARCHAR, path VARCHAR, old_path VARCHAR,
    status VARCHAR, added INTEGER, deleted INTEGER);
CREATE TABLE IF NOT EXISTS cache.hunks (sha VARCHAR, path VARCHAR, old_start INTEGER,
    old_lines INTEGER, new_start INTEGER, new_lines INTEGER);
CREATE TABLE IF NOT EXISTS cache.blob_files (sha VARCHAR, path VARCHAR, old_path VARCHAR, side VARCHAR,
    blob_id VARCHAR, mode VARCHAR, file VARCHAR);
CREATE TABLE IF NOT EXISTS cache.blob_parsed (blob_id VARCHAR, bytes BIGINT);
CREATE TABLE IF NOT EXISTS cache.blob_derived (blob_id VARCHAR);
-- blob_id plus read_ast's columns, as cache.nodes.
CREATE TABLE IF NOT EXISTS cache.blob_nodes AS
SELECT ''::VARCHAR AS blob_id, *
FROM read_ast([getvariable('cache_path') || '.no-such-file.rs'], source := 'full', peek := 'none',
              context := 'none', ignore_errors := true);
-- The columns of blob_units (views/units.sql).
CREATE TABLE IF NOT EXISTS cache.blob_units (blob_id VARCHAR, unit_ord BIGINT, parent_unit_ord BIGINT,
    node_id BIGINT, kind VARCHAR, name VARCHAR, qualified_name VARCHAR, start_line BIGINT,
    end_line BIGINT, loc HUGEINT);
-- The columns of scip_document_occurrences and scip_document_symbols (views/scip.sql).
CREATE TABLE IF NOT EXISTS cache.scip_occurrences (document VARCHAR, symbol VARCHAR, start_line INTEGER,
    start_column INTEGER, end_line INTEGER, end_column INTEGER, roles INTEGER, is_definition BOOLEAN,
    is_reference BOOLEAN, is_import BOOLEAN, is_write BOOLEAN, is_read BOOLEAN, is_generated BOOLEAN,
    is_test BOOLEAN, is_forward_definition BOOLEAN, enclosing_start_line INTEGER,
    enclosing_start_column INTEGER, enclosing_end_line INTEGER, enclosing_end_column INTEGER);
CREATE TABLE IF NOT EXISTS cache.scip_symbols (document VARCHAR, symbol VARCHAR, kind VARCHAR,
    display_name VARCHAR, enclosing_symbol VARCHAR, documentation VARCHAR[], relationship_count INTEGER);

SET VARIABLE refresh_prior = (SELECT m FROM cache.meta m);
SELECT CASE WHEN getvariable('refresh_prior').schema_version <> 3
    THEN error('refresh.sql: ' || getvariable('cache_path') || ' has another schema version; delete it')
END;

SET VARIABLE refresh_syntax_key = (SELECT extension_version FROM duckdb_extensions()
                                   WHERE extension_name = 'sitting_duck');
-- The text of the views and macros units, tags and blob units are derived through. The
-- unit rules are a table macro (lang/*.sql), so every macro that is not internal is
-- included (sitting_duck's own among them; their text changes only with its version):
-- reading duckdb_functions() costs about 18 ms of every refresh (sitting_duck registers
-- thousands of functions). blob_units is excluded because this file replaces it.
SET VARIABLE refresh_derive_key = md5(concat_ws(chr(10),
    (SELECT string_agg(view_name || ':' || sql, chr(10) ORDER BY view_name)
     FROM duckdb_views()
     WHERE NOT internal AND view_name <> 'blob_units'
       AND (view_name IN ('scope_files', 'sources', 'source_bytes', 'source_lines', 'scope_nodes',
                          'blob_scope_nodes', 'blob_sources', 'blob_source_bytes', 'blob_source_lines')
            OR view_name LIKE '%\_units' ESCAPE '\' OR view_name LIKE '%\_tags' ESCAPE '\')),
    (SELECT string_agg(function_name || ':' || macro_definition, chr(10) ORDER BY function_name, macro_definition)
     FROM duckdb_functions()
     WHERE NOT internal AND function_type IN ('macro', 'table_macro'))));
SET VARIABLE refresh_history_key = concat_ws(' ', 'since=' || coalesce(getvariable('since'), ''),
    'include_merges=' || getvariable('include_merges'), 'languages=' || getvariable('languages')::VARCHAR,
    'blob_dir=' || getvariable('blob_dir'));

SET VARIABLE refresh_syntax_reset = getvariable('refresh_prior') IS NULL
    OR getvariable('refresh_prior').state IS DISTINCT FROM 'ok'
    OR getvariable('refresh_prior').repo IS DISTINCT FROM getvariable('repo')
    OR getvariable('refresh_prior').syntax_key IS DISTINCT FROM getvariable('refresh_syntax_key');
SET VARIABLE refresh_derive_reset = getvariable('refresh_syntax_reset')
    OR getvariable('refresh_prior').derive_key IS DISTINCT FROM getvariable('refresh_derive_key');

-- SCIP: whether views/scip.sql was read (scip_index_occurrences exists only then), and
-- the key. Without it, empty views of the cached columns stand in for the two views the
-- rows are copied from (they persist until views/scip.sql replaces them).
SET VARIABLE refresh_scip_loaded = EXISTS (SELECT 1 FROM duckdb_views()
                                           WHERE view_name = 'scip_index_occurrences' AND NOT internal);
-- The same two statements as in cache.sql.
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
SET VARIABLE refresh_scip_hash = (SELECT hash(content)::VARCHAR FROM read_blob(getvariable('scip_index')));
SELECT CASE WHEN getvariable('refresh_scip_loaded') AND getvariable('refresh_scip_hash') IS NULL
    THEN error('refresh.sql: views/scip.sql was read but the index ' || getvariable('scip_index') || ' does not exist')
END;
SET VARIABLE refresh_scip_key = CASE WHEN getvariable('refresh_scip_loaded') THEN concat_ws(' ',
    getvariable('scip_index'), getvariable('refresh_scip_hash'),
    (SELECT md5(string_agg(view_name || ':' || sql, chr(10) ORDER BY view_name)) FROM duckdb_views()
     WHERE NOT internal AND view_name IN ('scip_index_occurrences', 'scip_index_symbols',
                                          'scip_document_occurrences', 'scip_document_symbols'))) END;

UPDATE cache.meta SET state = 'refreshing';

-- Syntax: which files to drop and to parse.
DELETE FROM cache.nodes_files WHERE getvariable('refresh_syntax_reset');
DELETE FROM cache.nodes WHERE getvariable('refresh_syntax_reset');
SET VARIABLE refresh_changed = (
    SELECT coalesce(list(f.path ORDER BY f.path), [])
    FROM files f
    WHERE NOT EXISTS (SELECT 1 FROM cache.nodes_files c
                      WHERE c.path = f.path AND c.blob_id = f.blob_id
                        AND c.language IS NOT DISTINCT FROM f.language)
);
SET VARIABLE refresh_removed = (
    SELECT coalesce(list(path ORDER BY path), [])
    FROM cache.nodes_files WHERE path NOT IN (SELECT path FROM files)
);
SET VARIABLE refresh_added = (
    SELECT count(*)::INTEGER FROM unnest(getvariable('refresh_changed')) u(path)
    WHERE path NOT IN (SELECT path FROM cache.nodes_files)
);
SET VARIABLE refresh_drop = list_concat(getvariable('refresh_changed'), getvariable('refresh_removed'));
SET VARIABLE refresh_t_files = epoch(now());

DELETE FROM cache.nodes WHERE list_contains(getvariable('refresh_drop'), path);
DELETE FROM cache.nodes_files WHERE list_contains(getvariable('refresh_drop'), path);
-- read_ast refuses an empty list (when the list is bound, so no CASE around the call
-- can avoid it). With nothing to parse, the call gets one path that does not exist and
-- ignore_errors, reads no file and returns no rows; the WHERE keeps only the changed
-- paths in any case. ignore_errors is off whenever there are files to parse.
SET VARIABLE refresh_nothing_to_parse = len(getvariable('refresh_changed')) = 0;
SET VARIABLE refresh_parse_paths = CASE
    WHEN getvariable('refresh_nothing_to_parse') THEN [getvariable('cache_path') || '.no-such-file.rs']
    ELSE list_transform(getvariable('refresh_changed'), lambda p: getvariable('repo') || '/' || p)
END;
SET VARIABLE refresh_nodes_before = (SELECT count(*) FROM cache.nodes);
-- The same read_ast parameters as views/nodes.sql.
INSERT INTO cache.nodes
SELECT substr(a.file_path, length(getvariable('repo')) + 2) AS path, a.*
FROM read_ast(getvariable('refresh_parse_paths'), source := 'full', peek := 'none', context := 'none',
              ignore_errors := getvariable('refresh_nothing_to_parse')) a
WHERE list_contains(getvariable('refresh_changed'), substr(a.file_path, length(getvariable('repo')) + 2));
INSERT INTO cache.nodes_files
SELECT path, blob_id, language FROM files WHERE list_contains(getvariable('refresh_changed'), path);
SET VARIABLE refresh_nodes_parsed = (SELECT count(*) FROM cache.nodes) - getvariable('refresh_nodes_before');

-- nodes from the cache: rows of the files whose cached blob id is the current one.
CREATE OR REPLACE VIEW nodes AS
SELECT f.file_id, n.* EXCLUDE (path)
FROM cache.nodes n
JOIN cache.nodes_files c ON c.path = n.path
JOIN files f ON f.path = n.path AND f.blob_id = c.blob_id;
SET VARIABLE refresh_t_nodes = epoch(now());

-- Units and tags for the changed files only (all files after a definition change),
-- computed by derived_units/derived_tags over the cached nodes of `scope_paths`.
-- unit_id is a position in the scope and a file's units are consecutive, so the cache
-- keeps each unit's position within its file (unit_ord) and its parent's
-- (parent_unit_ord).
DELETE FROM cache.units WHERE getvariable('refresh_derive_reset') OR list_contains(getvariable('refresh_drop'), path);
DELETE FROM cache.tags WHERE getvariable('refresh_derive_reset') OR list_contains(getvariable('refresh_drop'), path);
SET VARIABLE scope_paths = CASE WHEN getvariable('refresh_derive_reset')
    THEN (SELECT coalesce(list(path), []) FROM files)
    ELSE getvariable('refresh_changed') END;
SET VARIABLE refresh_units_before = (SELECT count(*) FROM cache.units);
INSERT INTO cache.units BY NAME
SELECT f.path,
       u.unit_id - min(u.unit_id) OVER (PARTITION BY u.file_id) AS unit_ord,
       u.parent_unit_id - min(u.unit_id) OVER (PARTITION BY u.file_id) AS parent_unit_ord,
       u.* EXCLUDE (unit_id, file_id, parent_unit_id)
FROM derived_units u JOIN files f ON f.file_id = u.file_id;
SET VARIABLE refresh_units_inserted = (SELECT count(*) FROM cache.units) - getvariable('refresh_units_before');
SET VARIABLE refresh_t_units = epoch(now());
SET VARIABLE refresh_tags_before = (SELECT count(*) FROM cache.tags);
INSERT INTO cache.tags BY NAME
SELECT f.path, t.* EXCLUDE (file_id)
FROM derived_tags t JOIN files f ON f.file_id = t.file_id;
SET VARIABLE refresh_tags_inserted = (SELECT count(*) FROM cache.tags) - getvariable('refresh_tags_before');
SET VARIABLE scope_paths = NULL;
SET VARIABLE refresh_t_tags = epoch(now());

CREATE OR REPLACE VIEW units AS
SELECT unit_id, file_id, node_id, unit_id - unit_ord + parent_unit_ord AS parent_unit_id,
       kind, name, qualified_name, start_line, end_line, loc
FROM (
    SELECT row_number() OVER (ORDER BY f.file_id, u.unit_ord) - 1 AS unit_id, f.file_id, u.*
    FROM cache.units u
    JOIN cache.nodes_files c ON c.path = u.path
    JOIN files f ON f.path = u.path AND f.blob_id = c.blob_id
);
CREATE OR REPLACE VIEW tags AS
SELECT f.file_id, t.* EXCLUDE (path)
FROM cache.tags t
JOIN cache.nodes_files c ON c.path = t.path
JOIN files f ON f.path = t.path AND f.blob_id = c.blob_id;

-- History: append cached_head..HEAD, or rebuild since..HEAD. git_commits lists HEAD
-- first (git log's order, emitted from one thread), which gives the new head.
SET VARIABLE refresh_history_reset = getvariable('refresh_prior') IS NULL
    OR getvariable('refresh_prior').state IS DISTINCT FROM 'ok'
    OR getvariable('refresh_prior').repo IS DISTINCT FROM getvariable('repo')
    OR getvariable('refresh_prior').history_key IS DISTINCT FROM getvariable('refresh_history_key')
    OR getvariable('refresh_prior').head IS NULL
    OR NOT EXISTS (SELECT 1 FROM git_commits(getvariable('repo'), since := getvariable('since'),
                                             include_merges := getvariable('include_merges'))
                   WHERE sha = getvariable('refresh_prior').head);
DELETE FROM cache.commits WHERE getvariable('refresh_history_reset');
DELETE FROM cache.file_changes WHERE getvariable('refresh_history_reset');
DELETE FROM cache.hunks WHERE getvariable('refresh_history_reset');
SET VARIABLE refresh_history_since = CASE WHEN getvariable('refresh_history_reset')
    THEN getvariable('since') ELSE getvariable('refresh_prior').head END;
CREATE OR REPLACE TEMP TABLE refresh_commits AS
SELECT * FROM git_commits(getvariable('repo'), since := getvariable('refresh_history_since'),
                          include_merges := getvariable('include_merges'));
INSERT INTO cache.commits SELECT * FROM refresh_commits;
SET VARIABLE refresh_head = coalesce((SELECT sha FROM refresh_commits LIMIT 1),
    CASE WHEN NOT getvariable('refresh_history_reset') THEN getvariable('refresh_prior').head END);
SET VARIABLE refresh_commits_added = (SELECT count(*) FROM refresh_commits);
DROP TABLE refresh_commits;
SET VARIABLE refresh_t_commits = epoch(now());
INSERT INTO cache.file_changes
SELECT * FROM git_file_changes(getvariable('repo'), since := getvariable('refresh_history_since'),
                               include_merges := getvariable('include_merges'));
SET VARIABLE refresh_t_file_changes = epoch(now());
-- As views/history.sql: only files whose extension is in `languages`.
INSERT INTO cache.hunks
SELECT * FROM git_hunks(getvariable('repo'), since := getvariable('refresh_history_since'),
                        include_merges := getvariable('include_merges'))
WHERE getvariable('languages')[regexp_extract(path, '\.([^./]+)$', 1)] IS NOT NULL;
SET VARIABLE refresh_t_hunks = epoch(now());

CREATE OR REPLACE VIEW commits AS SELECT * FROM cache.commits;
CREATE OR REPLACE VIEW file_changes AS SELECT * FROM cache.file_changes;
CREATE OR REPLACE VIEW hunks AS SELECT * FROM cache.hunks;

-- Blob files: rebuilt with the history, or when every blob's units are re-derived (that
-- reads every blob file, and the rebuild exports the missing ones); else appended with
-- the same commits as the history. git_changed_blobs exports what it lists.
SET VARIABLE refresh_blob_files_reset = getvariable('refresh_history_reset') OR getvariable('refresh_derive_reset');
DELETE FROM cache.blob_files WHERE getvariable('refresh_blob_files_reset');
SET VARIABLE refresh_blob_since = CASE WHEN getvariable('refresh_blob_files_reset')
    THEN getvariable('since') ELSE getvariable('refresh_prior').head END;
SET VARIABLE refresh_blob_rows_before = (SELECT count(*) FROM cache.blob_files);
-- As views/blobs.sql.
INSERT INTO cache.blob_files
SELECT * FROM git_changed_blobs(getvariable('repo'), getvariable('blob_dir'), since := getvariable('refresh_blob_since'),
                                include_merges := getvariable('include_merges'),
                                extensions := map_keys(getvariable('languages')));
SET VARIABLE refresh_blob_rows_added = (SELECT count(*) FROM cache.blob_files) - getvariable('refresh_blob_rows_before');
SET VARIABLE refresh_t_blob_files = epoch(now());

-- Blob syntax: parse the blobs of the range that are not cached, through the live
-- blob_nodes of views/blobs.sql with its snapshot narrowed to them (the same sentinel
-- as views/blobs.sql when there are none).
DELETE FROM cache.blob_parsed WHERE getvariable('refresh_syntax_reset');
DELETE FROM cache.blob_nodes WHERE getvariable('refresh_syntax_reset');
DELETE FROM cache.blob_derived WHERE getvariable('refresh_derive_reset');
DELETE FROM cache.blob_units WHERE getvariable('refresh_derive_reset');
SET VARIABLE blob_snapshot = (
    SELECT coalesce(list({'blob_id': blob_id, 'file': file} ORDER BY blob_id), [])
    FROM (SELECT blob_id, min(file) AS file FROM cache.blob_files
          WHERE blob_id NOT IN (SELECT blob_id FROM cache.blob_parsed) GROUP BY blob_id)
);
SET VARIABLE blob_paths_empty = len(getvariable('blob_snapshot')) = 0;
SET VARIABLE blob_paths = CASE WHEN getvariable('blob_paths_empty')
    THEN [getvariable('blob_dir') || '/no-such-blob.rs']
    ELSE list_transform(getvariable('blob_snapshot'), lambda b: b.file) END;
CREATE OR REPLACE VIEW blob_nodes AS
SELECT l.blob_id, a.*
FROM read_ast(getvariable('blob_paths'), source := 'full', peek := 'none', context := 'none',
              ignore_errors := getvariable('blob_paths_empty')) a
JOIN blob_list l ON l.file = a.file_path;
SET VARIABLE refresh_blob_nodes_before = (SELECT count(*) FROM cache.blob_nodes);
INSERT INTO cache.blob_nodes SELECT * FROM blob_nodes;
-- read_text reads only the sizes here (projection pushdown).
INSERT INTO cache.blob_parsed
SELECT l.blob_id, t.size FROM read_text(getvariable('blob_paths')) t JOIN blob_list l ON l.file = t.filename;
SET VARIABLE refresh_blobs_parsed = len(getvariable('blob_snapshot'));
SET VARIABLE refresh_blob_bytes_parsed = (SELECT coalesce(sum(bytes), 0) FROM cache.blob_parsed
                                          WHERE blob_id IN (SELECT blob_id FROM blob_list));
SET VARIABLE refresh_blob_nodes_parsed = (SELECT count(*) FROM cache.blob_nodes) - getvariable('refresh_blob_nodes_before');
SET VARIABLE refresh_t_blob_nodes = epoch(now());

-- Blob units: derived from the cached nodes of the blobs of the range that have none
-- yet, by derived_blob_units over the snapshot narrowed to them; blob_nodes reads the
-- cache, restricted to the snapshot.
SET VARIABLE blob_snapshot = (
    SELECT coalesce(list({'blob_id': blob_id, 'file': file} ORDER BY blob_id), [])
    FROM (SELECT blob_id, min(file) AS file FROM cache.blob_files
          WHERE blob_id NOT IN (SELECT blob_id FROM cache.blob_derived) GROUP BY blob_id)
);
SET VARIABLE blob_paths_empty = len(getvariable('blob_snapshot')) = 0;
SET VARIABLE blob_paths = CASE WHEN getvariable('blob_paths_empty')
    THEN [getvariable('blob_dir') || '/no-such-blob.rs']
    ELSE list_transform(getvariable('blob_snapshot'), lambda b: b.file) END;
CREATE OR REPLACE VIEW blob_nodes AS
SELECT n.* FROM cache.blob_nodes n WHERE n.blob_id IN (SELECT blob_id FROM blob_list);
SET VARIABLE refresh_blob_units_before = (SELECT count(*) FROM cache.blob_units);
INSERT INTO cache.blob_units BY NAME SELECT * FROM derived_blob_units;
INSERT INTO cache.blob_derived SELECT blob_id FROM blob_list;
SET VARIABLE refresh_blobs_derived = len(getvariable('blob_snapshot'));
SET VARIABLE refresh_blob_units_inserted = (SELECT count(*) FROM cache.blob_units) - getvariable('refresh_blob_units_before');

-- The snapshot of views/blobs.sql again, over the cached blob files: every blob of the
-- range. The views show only those blobs.
SET VARIABLE blob_snapshot = (
    SELECT coalesce(list({'blob_id': blob_id, 'file': file} ORDER BY blob_id), [])
    FROM (SELECT blob_id, min(file) AS file FROM cache.blob_files GROUP BY blob_id)
);
SET VARIABLE blob_paths_empty = len(getvariable('blob_snapshot')) = 0;
SET VARIABLE blob_paths = CASE WHEN getvariable('blob_paths_empty')
    THEN [getvariable('blob_dir') || '/no-such-blob.rs']
    ELSE list_transform(getvariable('blob_snapshot'), lambda b: b.file) END;
CREATE OR REPLACE VIEW blob_files AS SELECT * FROM cache.blob_files;
CREATE OR REPLACE VIEW blob_units AS
SELECT u.* FROM cache.blob_units u WHERE u.blob_id IN (SELECT blob_id FROM blob_list);
SET VARIABLE refresh_t_blob_units = epoch(now());

-- SCIP: rebuilt whole when the key changed (or after an interrupted refresh), from the
-- views of views/scip.sql (or the empty stand-ins).
SET VARIABLE refresh_scip_reset = getvariable('refresh_prior') IS NULL
    OR getvariable('refresh_prior').state IS DISTINCT FROM 'ok'
    OR getvariable('refresh_prior').scip_key IS DISTINCT FROM getvariable('refresh_scip_key');
DELETE FROM cache.scip_occurrences WHERE getvariable('refresh_scip_reset');
DELETE FROM cache.scip_symbols WHERE getvariable('refresh_scip_reset');
-- A false WHERE folds to an empty result: the index is not read when the key is unchanged.
INSERT INTO cache.scip_occurrences SELECT * FROM scip_document_occurrences WHERE getvariable('refresh_scip_reset');
SET VARIABLE refresh_scip_occurrences_inserted = CASE WHEN getvariable('refresh_scip_reset')
    THEN (SELECT count(*) FROM cache.scip_occurrences) ELSE 0 END;
INSERT INTO cache.scip_symbols SELECT * FROM scip_document_symbols WHERE getvariable('refresh_scip_reset');
SET VARIABLE refresh_scip_symbols_inserted = CASE WHEN getvariable('refresh_scip_reset')
    THEN (SELECT count(*) FROM cache.scip_symbols) ELSE 0 END;
CREATE OR REPLACE VIEW scip_occurrences AS
SELECT f.file_id, o.*
FROM cache.scip_occurrences o
LEFT JOIN files f ON f.path = o.document;
CREATE OR REPLACE VIEW scip_symbols AS SELECT * FROM cache.scip_symbols;
SET VARIABLE refresh_t_scip = epoch(now());

CHECKPOINT cache;
SET VARIABLE refresh_t_end = epoch(now());

DELETE FROM cache.meta;
INSERT INTO cache.meta
SELECT 3, getvariable('repo'), 'ok',
       CASE WHEN getvariable('refresh_syntax_reset') THEN now()::TIMESTAMP ELSE getvariable('refresh_prior').built_at END,
       now()::TIMESTAMP,
       getvariable('refresh_syntax_key'), getvariable('refresh_derive_key'), getvariable('refresh_history_key'),
       getvariable('refresh_head'),
       CASE WHEN getvariable('refresh_syntax_reset') THEN 'rebuilt' ELSE 'incremental' END,
       CASE WHEN getvariable('refresh_derive_reset') THEN 'rebuilt' ELSE 'incremental' END,
       CASE WHEN getvariable('refresh_history_reset') THEN 'rebuilt' ELSE 'appended' END,
       getvariable('refresh_added'),
       len(getvariable('refresh_changed')) - getvariable('refresh_added'),
       len(getvariable('refresh_removed')),
       len(getvariable('source_files')) - len(getvariable('refresh_changed')),
       getvariable('refresh_nodes_parsed'),
       getvariable('refresh_units_inserted'),
       getvariable('refresh_tags_inserted'),
       getvariable('refresh_commits_added'),
       CASE WHEN getvariable('refresh_blob_files_reset') THEN 'rebuilt' ELSE 'appended' END,
       getvariable('refresh_blob_rows_added'),
       getvariable('refresh_blobs_parsed'),
       getvariable('refresh_blob_bytes_parsed'),
       getvariable('refresh_blob_nodes_parsed'),
       getvariable('refresh_blobs_derived'),
       getvariable('refresh_blob_units_inserted'),
       (SELECT count(*) FROM cache.blob_parsed),
       getvariable('refresh_scip_key'),
       CASE WHEN getvariable('refresh_scip_reset') THEN 'rebuilt' ELSE 'kept' END,
       getvariable('refresh_scip_occurrences_inserted'),
       getvariable('refresh_scip_symbols_inserted'),
       {'files': getvariable('refresh_t_files') - getvariable('refresh_t_start'),
        'nodes': getvariable('refresh_t_nodes') - getvariable('refresh_t_files'),
        'units': getvariable('refresh_t_units') - getvariable('refresh_t_nodes'),
        'tags': getvariable('refresh_t_tags') - getvariable('refresh_t_units'),
        'commits': getvariable('refresh_t_commits') - getvariable('refresh_t_tags'),
        'file_changes': getvariable('refresh_t_file_changes') - getvariable('refresh_t_commits'),
        'hunks': getvariable('refresh_t_hunks') - getvariable('refresh_t_file_changes'),
        'blob_files': getvariable('refresh_t_blob_files') - getvariable('refresh_t_hunks'),
        'blob_nodes': getvariable('refresh_t_blob_nodes') - getvariable('refresh_t_blob_files'),
        'blob_units': getvariable('refresh_t_blob_units') - getvariable('refresh_t_blob_nodes'),
        'scip': getvariable('refresh_t_scip') - getvariable('refresh_t_blob_units'),
        'checkpoint': getvariable('refresh_t_end') - getvariable('refresh_t_scip'),
        'total': getvariable('refresh_t_end') - getvariable('refresh_t_start')};
