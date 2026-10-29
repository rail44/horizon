-- Persistent cache: materialize the derived layers once per state of the inputs and point
-- their views at the copies, so that every view and query keeps its text and reads them.
-- Materialization is only a cache: the views compute the same rows live.
--
-- Read after all view files (00_load.sql through views/quality.sql), once per session;
-- live again: re-read the view files. Reading this file while the views already read a
-- cache is an error (re-read the view files first).
--
-- Key. The inputs every cached layer derives from: `repo`, the file snapshot
-- (source_files: paths and content ids, taken again here as 00_load.sql takes it), each
-- SCIP index's path and content hash, the test settings (extra_test_paths,
-- extra_test_attributes), and the text of every view and macro of the view files. The
-- cache of one key is the file <cache_dir>/<md5 of the key>.duckdb, attached as `cache`;
-- a file that exists already holds the layers (each is created whole or not at all, so
-- an interrupted build leaves only complete tables, and the next read creates the rest).
-- Files of other keys are left in place; `code-db` removes them after a read.
--
-- Layers, in the order they are built, each from the copies before it: scip_occurrences,
-- scip_symbols (views/scip.sql); units (views/units.sql); unit_occurrences, symbol_units,
-- unit_references, unit_edges, unit_ancestors, impls, impl_targets, unit_impl_type,
-- modules, unit_module, module_edges (views/graph.sql); unit_scope, unit_fan_in,
-- unit_fan_out, unit_reference_status, unit_debt_markers (views/quality.sql). Views that
-- read a query variable or the settings' tables (module_cycle_edges and what reads it,
-- the layers) and the clone views (jscpd's reports are not in the key) stay live, over the
-- copies.
--
-- cache.meta(key, repo, built_at, seconds, layers) records the build: the wall time and
-- the row count of each layer.

SELECT CASE
    WHEN NOT EXISTS (SELECT 1 FROM duckdb_views() WHERE view_name = 'derived_units')
        THEN error('refresh.sql: read the view files first')
    WHEN NOT EXISTS (SELECT 1 FROM duckdb_views() WHERE view_name = 'unit_quality')
        THEN error('refresh.sql: read views/quality.sql first')
    WHEN EXISTS (SELECT 1 FROM duckdb_views() WHERE view_name = 'units' AND regexp_matches(sql, '"?cache"?\.'))
        THEN error('refresh.sql: the views read a cache already; re-read the view files first')
END;

SET VARIABLE refresh_t_start = epoch(now());

-- File snapshot: the same two statements as in 00_load.sql.
SET VARIABLE source_files = (
    SELECT coalesce(list({'path': path, 'mode': mode, 'blob_id': blob_id} ORDER BY path), [])
    FROM tracked_source_files
);
SET VARIABLE source_paths = list_transform(getvariable('source_files'),
                                           lambda f: getvariable('repo') || '/' || f.path);



-- The stub report keeps read_blob's list non-empty when there is no index.
SET VARIABLE refresh_key = concat_ws(chr(10),
    'schema 1', getvariable('repo'), getvariable('source_files')::VARCHAR,
    (SELECT string_agg(filename || ' ' || hash(content)::VARCHAR, ' ' ORDER BY filename)
     FROM read_blob(getvariable('scip_indexes') || ['duckdb-ext/sql/empty-jscpd-report.json'])),
    getvariable('extra_test_paths')::VARCHAR, getvariable('extra_test_attributes')::VARCHAR,
    (SELECT string_agg(view_name || ' ' || sql, chr(10) ORDER BY view_name) FROM duckdb_views() WHERE NOT internal),
    (SELECT string_agg(function_name || ' ' || coalesce(macro_definition, ''), chr(10) ORDER BY function_name)
     FROM duckdb_functions() WHERE NOT internal AND function_type IN ('macro', 'table_macro')));
SET VARIABLE refresh_file = getvariable('cache_dir') || '/' || md5(getvariable('refresh_key')) || '.duckdb';

-- A cache an earlier read of this session attached (the views no longer read it: the
-- view files were read again) may be of another key.
DETACH DATABASE IF EXISTS cache;
ATTACH getvariable('refresh_file') AS cache;

CREATE TABLE IF NOT EXISTS cache.meta (key VARCHAR, repo VARCHAR, built_at TIMESTAMP,
    seconds DOUBLE, layers STRUCT(name VARCHAR, row_count BIGINT)[]);
SET VARIABLE refresh_built = (SELECT count(*) > 0 FROM cache.meta);

-- Each layer: copy it when the file does not hold it yet, then read the copy.
CREATE TABLE IF NOT EXISTS cache.scip_occurrences AS SELECT * FROM scip_occurrences;
CREATE OR REPLACE VIEW scip_occurrences AS SELECT * FROM cache.scip_occurrences;
CREATE TABLE IF NOT EXISTS cache.scip_symbols AS SELECT * FROM scip_symbols;
CREATE OR REPLACE VIEW scip_symbols AS SELECT * FROM cache.scip_symbols;
CREATE TABLE IF NOT EXISTS cache.units AS SELECT * FROM units;
CREATE OR REPLACE VIEW units AS SELECT * FROM cache.units;
CREATE TABLE IF NOT EXISTS cache.unit_occurrences AS SELECT * FROM unit_occurrences;
CREATE OR REPLACE VIEW unit_occurrences AS SELECT * FROM cache.unit_occurrences;
CREATE TABLE IF NOT EXISTS cache.symbol_units AS SELECT * FROM symbol_units;
CREATE OR REPLACE VIEW symbol_units AS SELECT * FROM cache.symbol_units;
CREATE TABLE IF NOT EXISTS cache.unit_references AS SELECT * FROM unit_references;
CREATE OR REPLACE VIEW unit_references AS SELECT * FROM cache.unit_references;
CREATE TABLE IF NOT EXISTS cache.unit_edges AS SELECT * FROM unit_edges;
CREATE OR REPLACE VIEW unit_edges AS SELECT * FROM cache.unit_edges;
CREATE TABLE IF NOT EXISTS cache.unit_ancestors AS SELECT * FROM unit_ancestors;
CREATE OR REPLACE VIEW unit_ancestors AS SELECT * FROM cache.unit_ancestors;
CREATE TABLE IF NOT EXISTS cache.impls AS SELECT * FROM impls;
CREATE OR REPLACE VIEW impls AS SELECT * FROM cache.impls;
CREATE TABLE IF NOT EXISTS cache.impl_targets AS SELECT * FROM impl_targets;
CREATE OR REPLACE VIEW impl_targets AS SELECT * FROM cache.impl_targets;
CREATE TABLE IF NOT EXISTS cache.unit_impl_type AS SELECT * FROM unit_impl_type;
CREATE OR REPLACE VIEW unit_impl_type AS SELECT * FROM cache.unit_impl_type;
CREATE TABLE IF NOT EXISTS cache.modules AS SELECT * FROM modules;
CREATE OR REPLACE VIEW modules AS SELECT * FROM cache.modules;
CREATE TABLE IF NOT EXISTS cache.unit_module AS SELECT * FROM unit_module;
CREATE OR REPLACE VIEW unit_module AS SELECT * FROM cache.unit_module;
CREATE TABLE IF NOT EXISTS cache.module_edges AS SELECT * FROM module_edges;
CREATE OR REPLACE VIEW module_edges AS SELECT * FROM cache.module_edges;
CREATE TABLE IF NOT EXISTS cache.unit_scope AS SELECT * FROM unit_scope;
CREATE OR REPLACE VIEW unit_scope AS SELECT * FROM cache.unit_scope;
CREATE TABLE IF NOT EXISTS cache.unit_fan_in AS SELECT * FROM unit_fan_in;
CREATE OR REPLACE VIEW unit_fan_in AS SELECT * FROM cache.unit_fan_in;
CREATE TABLE IF NOT EXISTS cache.unit_fan_out AS SELECT * FROM unit_fan_out;
CREATE OR REPLACE VIEW unit_fan_out AS SELECT * FROM cache.unit_fan_out;
CREATE TABLE IF NOT EXISTS cache.unit_reference_status AS SELECT * FROM unit_reference_status;
CREATE OR REPLACE VIEW unit_reference_status AS SELECT * FROM cache.unit_reference_status;
CREATE TABLE IF NOT EXISTS cache.unit_debt_markers AS SELECT * FROM unit_debt_markers;
CREATE OR REPLACE VIEW unit_debt_markers AS SELECT * FROM cache.unit_debt_markers;

INSERT INTO cache.meta
SELECT getvariable('refresh_key'), getvariable('repo'), now()::TIMESTAMP,
       round(epoch(now()) - getvariable('refresh_t_start'), 3),
       (SELECT list({'name': table_name, 'row_count': estimated_size} ORDER BY table_name)
        FROM duckdb_tables() WHERE database_name = 'cache' AND table_name <> 'meta')
WHERE NOT getvariable('refresh_built');
CHECKPOINT cache;

-- The cache this read used and whether it built it.
SELECT getvariable('refresh_file') AS cache_file,
       CASE WHEN getvariable('refresh_built') THEN 'kept' ELSE 'built' END AS cache,
       round(epoch(now()) - getvariable('refresh_t_start'), 3) AS seconds;
