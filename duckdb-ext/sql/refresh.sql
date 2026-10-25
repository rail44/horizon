-- Persistent cache: bring the cache file up to date with the repository, then point
-- nodes, units, tags, commits, file_changes and hunks at it.
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

-- Layout (schema version 1).
CREATE TABLE IF NOT EXISTS cache.meta (
    schema_version INTEGER,
    repo VARCHAR,
    state VARCHAR,              -- 'ok', or 'refreshing' while a refresh runs
    built_at TIMESTAMP,         -- the last full rebuild of the syntax layers
    refreshed_at TIMESTAMP,
    syntax_key VARCHAR,         -- sitting_duck version
    derive_key VARCHAR,         -- md5 of the derived views' definitions
    history_key VARCHAR,        -- since, include_merges, languages
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
    seconds STRUCT(files DOUBLE, nodes DOUBLE, units DOUBLE, tags DOUBLE, commits DOUBLE,
                   file_changes DOUBLE, hunks DOUBLE, checkpoint DOUBLE, total DOUBLE)
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

SET VARIABLE refresh_prior = (SELECT m FROM cache.meta m);
SELECT CASE WHEN getvariable('refresh_prior').schema_version <> 1
    THEN error('refresh.sql: ' || getvariable('cache_path') || ' has another schema version; delete it')
END;

SET VARIABLE refresh_syntax_key = (SELECT extension_version FROM duckdb_extensions()
                                   WHERE extension_name = 'sitting_duck');
-- The text of the views units and tags are derived through. The macros byte_slice and
-- normalize_ws are not covered: reading duckdb_functions() costs about 18 ms (sitting_duck
-- registers thousands of functions), a fifth of a refresh with nothing to do.
SET VARIABLE refresh_derive_key = (
    SELECT md5(string_agg(view_name || ':' || sql, chr(10) ORDER BY view_name))
    FROM duckdb_views()
    WHERE NOT internal AND (view_name IN ('scope_files', 'sources', 'source_bytes', 'source_lines')
                            OR view_name LIKE '%\_units' ESCAPE '\' OR view_name LIKE '%\_tags' ESCAPE '\')
);
SET VARIABLE refresh_history_key = concat_ws(' ', 'since=' || coalesce(getvariable('since'), ''),
    'include_merges=' || getvariable('include_merges'), 'languages=' || getvariable('languages')::VARCHAR);

SET VARIABLE refresh_syntax_reset = getvariable('refresh_prior') IS NULL
    OR getvariable('refresh_prior').state IS DISTINCT FROM 'ok'
    OR getvariable('refresh_prior').repo IS DISTINCT FROM getvariable('repo')
    OR getvariable('refresh_prior').syntax_key IS DISTINCT FROM getvariable('refresh_syntax_key');
SET VARIABLE refresh_derive_reset = getvariable('refresh_syntax_reset')
    OR getvariable('refresh_prior').derive_key IS DISTINCT FROM getvariable('refresh_derive_key');

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

CHECKPOINT cache;
SET VARIABLE refresh_t_end = epoch(now());

DELETE FROM cache.meta;
INSERT INTO cache.meta
SELECT 1, getvariable('repo'), 'ok',
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
       {'files': getvariable('refresh_t_files') - getvariable('refresh_t_start'),
        'nodes': getvariable('refresh_t_nodes') - getvariable('refresh_t_files'),
        'units': getvariable('refresh_t_units') - getvariable('refresh_t_nodes'),
        'tags': getvariable('refresh_t_tags') - getvariable('refresh_t_units'),
        'commits': getvariable('refresh_t_commits') - getvariable('refresh_t_tags'),
        'file_changes': getvariable('refresh_t_file_changes') - getvariable('refresh_t_commits'),
        'hunks': getvariable('refresh_t_hunks') - getvariable('refresh_t_file_changes'),
        'checkpoint': getvariable('refresh_t_end') - getvariable('refresh_t_hunks'),
        'total': getvariable('refresh_t_end') - getvariable('refresh_t_start')};
