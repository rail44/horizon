-- Load the fact sources and fill in the parameters every view reads.
--
-- Set `repo` (an absolute path to the repository root) before reading this file;
-- every other parameter has a default and can be set before or after it. Views read
-- the variables when they are scanned, so changing a variable changes the next query,
-- except `source_files`/`source_paths`, which this file computes from the globs once:
-- re-read this file to pick up added or removed files (file contents are always live).
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

-- Source files: DuckDB glob() patterns relative to `repo` (`**` crosses directories).
-- glob() follows no .gitignore, descends hidden directories and cannot prune, so the
-- default walks the whole tree; narrow it to the source roots in a checkout that holds
-- large build trees or nested worktrees.
SET VARIABLE include_globs = coalesce(getvariable('include_globs'), ['**/*.rs']);
-- Repository-relative paths to drop, as GLOB operator patterns (`*` crosses `/`):
-- build directories and anything under a hidden directory.
SET VARIABLE exclude_patterns = coalesce(getvariable('exclude_patterns'),
    ['target/*', '*/target/*', '.*', '*/.*']);
-- File extension -> language. A file is a source file only when its extension is here;
-- hunks are restricted to the same extensions.
SET VARIABLE languages = coalesce(getvariable('languages'), MAP {'rs': 'rust'});

SET VARIABLE source_files = (
    SELECT coalesce(list(path ORDER BY path), []::VARCHAR[])
    FROM (
        SELECT DISTINCT substr(file, length(getvariable('repo')) + 2) AS path
        FROM glob(list_transform(getvariable('include_globs'),
                                 lambda g: getvariable('repo') || '/' || g))
    )
    WHERE NOT list_bool_or(list_transform(getvariable('exclude_patterns'), lambda p: path GLOB p))
      AND getvariable('languages')[regexp_extract(path, '\.([^./]+)$', 1)] IS NOT NULL
);
SET VARIABLE source_paths = list_transform(getvariable('source_files'),
                                           lambda p: getvariable('repo') || '/' || p);

-- History: `since` is any revision (history is since..HEAD); unset means all of HEAD.
-- `include_merges` also diffs merges against their first parent.
SET VARIABLE include_merges = coalesce(getvariable('include_merges'), false);

-- jscpd JSON reports (duckdb-ext/sql/jscpd.sh). A report's `source` label is the name
-- of the directory that holds it. The files must exist when clones.sql is read:
-- read_json reads them to type the view.
SET VARIABLE clone_reports = coalesce(getvariable('clone_reports'), [
    getvariable('repo') || '/target/code-db/jscpd/exact/jscpd-report.json',
    getvariable('repo') || '/target/code-db/jscpd/normalized/jscpd-report.json']);
