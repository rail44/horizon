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

-- Limits (README.md, Limits): memory_limit, threads, temp_directory and
-- max_temp_directory_size, computed from the machine's state at every read of this file
-- unless the variables memory_limit, threads or temp_limit override them. The last
-- statement of this file returns the chosen values as one line (the variable limits).
-- setting_bytes(s): a size as current_setting() prints it or SET accepts it
-- ('49.6 GiB', '4GB', '512 bytes') in bytes; NULL for anything else.
CREATE OR REPLACE MACRO setting_bytes(s) AS
    TRY_CAST(regexp_extract(trim(s), '^([0-9.]+)', 1) AS DOUBLE)
    * CASE upper(regexp_extract(trim(s), '([A-Za-z]+)$', 1))
        WHEN 'BYTES' THEN 1 WHEN 'B' THEN 1
        WHEN 'KB' THEN 1e3 WHEN 'MB' THEN 1e6 WHEN 'GB' THEN 1e9 WHEN 'TB' THEN 1e12
        WHEN 'KIB' THEN 1024 WHEN 'MIB' THEN 1024 ^ 2 WHEN 'GIB' THEN 1024 ^ 3 WHEN 'TIB' THEN 1024 ^ 4
    END;
-- DuckDB's own defaults: memory_limit is 80 % of physical memory, threads the CPU count.
RESET memory_limit;
RESET threads;
RESET max_temp_directory_size;
SET VARIABLE limits_default_memory = setting_bytes(current_setting('memory_limit'));
SET VARIABLE limits_cpus = current_setting('threads');
-- What the OS can give now: MemAvailable plus what this DuckDB instance already holds
-- (a re-read in a session with large temporary tables counts them as its own). NULL when
-- /proc/meminfo is missing or has no MemAvailable line.
SET VARIABLE limits_meminfo = coalesce((SELECT any_value(file) FROM glob('/proc/meminfo')), '/dev/null');
SET VARIABLE limits_available = (
    SELECT TRY_CAST(regexp_extract(content, '(?m)^MemAvailable:\s+([0-9]+) kB', 1) AS BIGINT) * 1024
           + (SELECT coalesce(sum(memory_usage_bytes), 0) FROM duckdb_memory())
    FROM read_text(getvariable('limits_meminfo'))
);
-- 60 % of it (of DuckDB's default without /proc/meminfo), at most DuckDB's default, at
-- least 2 GB. The other 40 % is for allocations outside DuckDB's accounting.
SET VARIABLE limits_derived_memory = CASE
    WHEN getvariable('memory_limit') IS NOT NULL THEN NULL
    WHEN least(0.6 * coalesce(getvariable('limits_available'), getvariable('limits_default_memory')),
               getvariable('limits_default_memory')) >= 2e9
        THEN least(0.6 * coalesce(getvariable('limits_available'), getvariable('limits_default_memory')),
                   getvariable('limits_default_memory'))
    ELSE error('only ' || formatReadableDecimalSize(CAST(coalesce(getvariable('limits_available'),
                   getvariable('limits_default_memory')) AS BIGINT))
               || ' of memory is available, and 60% of it is below the 2 GB floor: free memory, '
               || 'or set the variable memory_limit (README.md, Limits)')
END;
SET memory_limit = coalesce(CAST(getvariable('memory_limit') AS VARCHAR),
                            CAST(round(getvariable('limits_derived_memory')) AS BIGINT) || ' bytes');
SET VARIABLE limits_memory = coalesce(getvariable('limits_derived_memory'),
                                      setting_bytes(CAST(getvariable('memory_limit') AS VARCHAR)),
                                      setting_bytes(current_setting('memory_limit')));
-- One thread per GB of memory_limit, at most the CPU count: each thread's operator state
-- then has a share of the limit to work in.
SET threads = coalesce(getvariable('threads'),
                       least(getvariable('limits_cpus'), greatest(1, floor(getvariable('limits_memory') / 1e9))));
-- Spill files go under the repository's target/code-db; their total is capped at half
-- of the free space of that file system when temp_free_bytes (run.sh, check.py) says
-- how much that is, else DuckDB's default stays.
SET temp_directory = getvariable('repo') || '/target/code-db/tmp';
SET max_temp_directory_size = coalesce(
    CAST(getvariable('temp_limit') AS VARCHAR),
    CAST(round(0.5 * getvariable('temp_free_bytes')) AS BIGINT) || ' bytes',
    current_setting('max_temp_directory_size'));
SET VARIABLE limits = 'limits: memory '
    || formatReadableDecimalSize(CAST(getvariable('limits_memory') AS BIGINT)) || ' ('
    || CASE
        WHEN getvariable('memory_limit') IS NOT NULL THEN 'memory_limit'
        WHEN getvariable('limits_available') IS NULL
            THEN '60% of DuckDB''s default '
                 || formatReadableDecimalSize(CAST(getvariable('limits_default_memory') AS BIGINT))
                 || '; /proc/meminfo unreadable'
        WHEN getvariable('limits_derived_memory') >= getvariable('limits_default_memory')
            THEN 'DuckDB''s default, below 60% of '
                 || formatReadableDecimalSize(CAST(getvariable('limits_available') AS BIGINT)) || ' available'
        ELSE '60% of ' || formatReadableDecimalSize(CAST(getvariable('limits_available') AS BIGINT))
             || ' available, /proc/meminfo'
    END || '), temp '
    || CASE
        WHEN getvariable('temp_limit') IS NOT NULL
            THEN coalesce(formatReadableDecimalSize(CAST(setting_bytes(CAST(getvariable('temp_limit') AS VARCHAR)) AS BIGINT)),
                          current_setting('max_temp_directory_size')) || ' (temp_limit)'
        WHEN getvariable('temp_free_bytes') IS NOT NULL
            THEN formatReadableDecimalSize(CAST(round(0.5 * getvariable('temp_free_bytes')) AS BIGINT))
                 || ' (50% of ' || formatReadableDecimalSize(CAST(getvariable('temp_free_bytes') AS BIGINT)) || ' free)'
        ELSE 'DuckDB''s default (' || current_setting('max_temp_directory_size') || '; temp_free_bytes unset)'
    END || ', threads ' || current_setting('threads') || ' ('
    || CASE
        WHEN getvariable('threads') IS NOT NULL THEN 'threads'
        WHEN current_setting('threads') < getvariable('limits_cpus')
            THEN '1 per GB of memory; ' || getvariable('limits_cpus') || ' CPUs'
        ELSE 'CPUs'
    END || ')';

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

-- The chosen limits (above), as one line.
SELECT getvariable('limits') AS limits;
