-- Load the fact sources and fill in the parameters every view reads.
--
-- Set `repo` (an absolute path to the repository root) before reading this file;
-- every other parameter has a default and can be set before or after it. Views read
-- the variables when they are scanned, so changing a variable changes the next query,
-- except `source_files`/`source_paths`, which this file computes once from
-- git_tracked_files: re-read this file (or refresh.sql) to pick up added or removed
-- files and new content ids. File contents are always read live.
--
-- The git_history and scip_index paths are relative to the current directory, which must
-- be the root of the Horizon checkout that built them (duckdb-ext/build.sh). LOAD accepts
-- only a literal, not getvariable(). The host must allow unsigned extensions.

INSTALL sitting_duck FROM community;
LOAD sitting_duck;
LOAD 'duckdb-ext/target/extension/v2.0.0/linux_amd64/git_history.duckdb_extension';
LOAD 'duckdb-ext/target/extension/v2.0.0/linux_amd64/scip_index.duckdb_extension';

SET VARIABLE repo = CASE
    WHEN getvariable('repo') LIKE '/%' THEN rtrim(getvariable('repo'), '/')
    ELSE error('set the variable repo to the absolute path of the repository root before reading 00_load.sql')
END;

-- Limits (README.md, Limits): `code-db` derives memory_limit, threads,
-- max_temp_directory_size and temp_directory from the machine's state and passes them as
-- the variables memory_limit, threads, temp_limit and temp_directory (`code-db limits
-- REPO --json` prints them for other clients); this file only applies them, at every
-- read. A variable left unset leaves DuckDB's default. The last statement of this file
-- returns the variable limits (code-db's one-line summary) or, without it, the settings
-- in force.
RESET memory_limit;
RESET threads;
RESET max_temp_directory_size;
SET memory_limit = coalesce(CAST(getvariable('memory_limit') AS VARCHAR), current_setting('memory_limit'));
SET threads = coalesce(getvariable('threads'), current_setting('threads'));
SET max_temp_directory_size = coalesce(CAST(getvariable('temp_limit') AS VARCHAR),
                                       current_setting('max_temp_directory_size'));
SET temp_directory = coalesce(getvariable('temp_directory'), current_setting('temp_directory'));

-- Source files are the worktree's files as git lists them (git_tracked_files: the
-- tracked files with the content id `git add` would record now; ignored files and
-- nested repositories never appear), plus the untracked files that are not ignored
-- when `include_untracked` is true.
SET VARIABLE include_untracked = coalesce(getvariable('include_untracked'), false);
-- Optional repository-relative paths to drop, as GLOB operator patterns (`*` crosses `/`).
SET VARIABLE exclude_patterns = coalesce(getvariable('exclude_patterns'), []::VARCHAR[]);
-- File extension -> language. A file is a source file only when its extension is here;
-- hunks are restricted to the same extensions.
SET VARIABLE languages = coalesce(getvariable('languages'), MAP {'rs': 'rust'});

-- is_source_file(path, mode): whether a tree entry is a source file: a regular file
-- (mode 100644 or 100755; a symlink's blob is its target path and a submodule's id is a
-- commit, neither the content read_ast parses) with an extension in `languages` and no
-- match in `exclude_patterns`. The current files and an earlier commit's tree
-- (views/snapshot.sql) are scoped by this one rule.
CREATE OR REPLACE MACRO is_source_file(path, mode) AS
    mode IN ('100644', '100755')
    AND getvariable('languages')[regexp_extract(path, '\.([^./]+)$', 1)] IS NOT NULL
    AND NOT coalesce(list_bool_or(list_transform(getvariable('exclude_patterns'), lambda p: path GLOB p)), false);

-- tracked_source_files(path, mode, blob_id): the source files now, computed at every
-- scan.
CREATE OR REPLACE VIEW tracked_source_files AS
SELECT path, mode, blob_id
FROM git_tracked_files(getvariable('repo'), include_untracked := getvariable('include_untracked'))
WHERE is_source_file(path, mode);

-- The snapshot every view reads: a list of {path, mode, blob_id} sorted by path, and
-- the absolute paths for the table functions. refresh.sql repeats these two statements.
SET VARIABLE source_files = (
    SELECT coalesce(list({'path': path, 'mode': mode, 'blob_id': blob_id} ORDER BY path), [])
    FROM tracked_source_files
);
SET VARIABLE source_paths = list_transform(getvariable('source_files'),
                                           lambda f: getvariable('repo') || '/' || f.path);

-- History: `since` is any revision (history is since..HEAD); unset means all of HEAD.
-- `include_merges` also diffs merges against their first parent.
SET VARIABLE include_merges = coalesce(getvariable('include_merges'), false);
-- The blob layer (views/blobs.sql) diffs merges against their first parent unless
-- `blob_include_merges` is false, so that every blob of every tree in since..HEAD is in
-- it (a conflict resolution's content enters history only through the merge). The
-- history views keep following include_merges, and so does everything that joins blob
-- sides to hunks (unit_changes, the lineage).
SET VARIABLE blob_include_merges = coalesce(getvariable('blob_include_merges'), true);

-- jscpd JSON reports (duckdb-ext/sql/jscpd.sh). A report's `source` label is the name
-- of the directory that holds it. The files must exist when clones.sql is read:
-- read_json reads them to type the view.
SET VARIABLE clone_reports = coalesce(getvariable('clone_reports'), [
    getvariable('repo') || '/target/code-db/jscpd/exact/jscpd-report.json',
    getvariable('repo') || '/target/code-db/jscpd/normalized/jscpd-report.json']);

-- Historical blobs (views/blobs.sql): git_changed_blobs writes each changed file's old
-- and new content here, once per blob, as <blob_id>.<extension>.
SET VARIABLE blob_dir = coalesce(getvariable('blob_dir'), getvariable('repo') || '/target/code-db/blobs');

-- SCIP index (views/scip.sql, optional): must exist when views/scip.sql is read.
SET VARIABLE scip_index = coalesce(getvariable('scip_index'), getvariable('repo') || '/target/code-db/index.scip');

-- Persistent cache (refresh.sql): the database file, created when missing.
SET VARIABLE cache_path = coalesce(getvariable('cache_path'),
    getvariable('repo') || '/target/code-db/cache.duckdb');

-- The analysed repository's settings (README.md, Settings file), which code-db reads
-- from its `.code-db.toml`. Test files and test attribute names beyond the rules of
-- lang/rust.sql: repository-relative GLOB operator patterns (`*` crosses `/`) and
-- attribute names.
SET VARIABLE extra_test_paths = coalesce(getvariable('extra_test_paths'), []::VARCHAR[]);
SET VARIABLE extra_test_attributes = coalesce(getvariable('extra_test_attributes'), []::VARCHAR[]);
-- The intended layers (views/graph.sql, module_layers): each layer's module patterns
-- (GLOB operator patterns over modules.module) and the layers each may depend on. Empty
-- unless code-db filled them before this file was read.
CREATE TABLE IF NOT EXISTS config_layers (name VARCHAR, module_pattern VARCHAR);
CREATE TABLE IF NOT EXISTS config_layer_deps (name VARCHAR, may_depend_on VARCHAR);

-- The limits (above), as one line.
SELECT coalesce(getvariable('limits'),
                'limits: memory ' || current_setting('memory_limit') || ', temp '
                || current_setting('max_temp_directory_size') || ', threads ' || current_setting('threads')
                || ' (the settings in force; code-db limits derives them)') AS limits;
