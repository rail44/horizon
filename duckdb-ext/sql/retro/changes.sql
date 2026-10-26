-- Retrospective, step 1. Run on a checkout of the range's end commit with `since` set
-- to the range's base:
--   duckdb-ext/sql/run.sh sql END duckdb-ext/sql/retro/changes.sql since=BASE retro_changes=/abs/changes.parquet
-- Writes the unit changes of since..HEAD to `retro_changes` (Parquet): one row per
-- (sha, path, unit_kind, unit_qualified_name, change_kind) of unit_changes, with the
-- commit's author time, subject and merge flag (merges follow include_merges).
COPY (
    SELECT uc.sha, uc.path, uc.unit_kind, uc.unit_qualified_name, uc.change_kind,
           c.is_merge, c.author_time, c.subject
    FROM unit_changes uc
    JOIN commits c USING (sha)
    WHERE NOT c.is_merge OR getvariable('include_merges')
    ORDER BY c.author_time, uc.sha, uc.path, uc.unit_kind, uc.unit_qualified_name, uc.change_kind
) TO (getvariable('retro_changes')) (FORMAT parquet);

SELECT count(*) AS unit_change_rows, count(DISTINCT sha) AS commits,
       count(DISTINCT (path, unit_kind, unit_qualified_name)) AS units
FROM read_parquet(getvariable('retro_changes'));
