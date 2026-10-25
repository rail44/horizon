-- Load the fact sources and fill in the parameters every view reads.
--
-- Set `repo` (an absolute path to the repository root) before reading this file;
-- every other parameter has a default and can be set before or after it. Views read
-- the variables when they are scanned, so changing a variable changes the next query,
-- except `source_files`/`source_paths`, which this file computes once from
-- git_tracked_files: re-read this file (or refresh.sql) to pick up added or removed
-- files and new content ids. File contents are always read live.
--
-- The git_history path is relative to the current directory, which must be the root
-- of the Horizon checkout that built it (duckdb-ext/build.sh). LOAD accepts only a
-- literal, not getvariable(). The host must allow unsigned extensions.

INSTALL sitting_duck FROM community;
LOAD sitting_duck;
LOAD 'duckdb-ext/target/extension/v2.0.0/linux_amd64/git_history.duckdb_extension';

SET VARIABLE repo = CASE
    WHEN getvariable('repo') LIKE '/%' THEN rtrim(getvariable('repo'), '/')
    ELSE error('set the variable repo to the absolute path of the repository root before reading 00_load.sql')
END;

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

-- tracked_source_files(path, mode, blob_id): the source files now, computed at every
-- scan. Only regular files (mode 100644 or 100755) count; a symlink's blob is its
-- target path and a submodule's id is a commit, neither the content read_ast parses.
CREATE OR REPLACE VIEW tracked_source_files AS
SELECT path, mode, blob_id
FROM git_tracked_files(getvariable('repo'), include_untracked := getvariable('include_untracked'))
WHERE mode IN ('100644', '100755')
  AND getvariable('languages')[regexp_extract(path, '\.([^./]+)$', 1)] IS NOT NULL
  AND NOT coalesce(list_bool_or(list_transform(getvariable('exclude_patterns'), lambda p: path GLOB p)), false);

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

-- jscpd JSON reports (duckdb-ext/sql/jscpd.sh). A report's `source` label is the name
-- of the directory that holds it. The files must exist when clones.sql is read:
-- read_json reads them to type the view.
SET VARIABLE clone_reports = coalesce(getvariable('clone_reports'), [
    getvariable('repo') || '/target/code-db/jscpd/exact/jscpd-report.json',
    getvariable('repo') || '/target/code-db/jscpd/normalized/jscpd-report.json']);

-- Persistent cache (refresh.sql): the database file, created when missing.
SET VARIABLE cache_path = coalesce(getvariable('cache_path'),
    getvariable('repo') || '/target/code-db/cache.duckdb');
