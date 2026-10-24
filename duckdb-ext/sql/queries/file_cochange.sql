-- Question: which pairs of current files change together in the same commits?
-- Counting: commits touching more than max_commit_files files (all files, not
-- only selected languages) are dropped; each file's count is over the remaining
-- commits; only current paths of selected-language files are paired (history
-- under a previous path is not followed). basis = max divides shared commits by
-- the larger count; basis = average divides by the mean of both counts, which is
-- Code Maat's coupling "degree".
-- Evidence: co-change is among the best-supported clone-harm predictors (Mondal,
-- Juergens), but coupling results depend strongly on the threshold (MDPI
-- Software 2024); see file_cochange_thresholds. No threshold is validated.
-- Parameters (DuckDB variables; default when unset):
--   min_shared 3, max_commit_files 50, min_coupling_pct 30, basis 'max';
--   include_merges (00_load.sql, false) also counts merges against their first parent.
WITH params AS (
    SELECT coalesce(getvariable('min_shared'), 3) AS min_shared,
           coalesce(getvariable('max_commit_files'), 50) AS max_commit_files,
           coalesce(getvariable('min_coupling_pct'), 30) AS min_coupling_pct,
           coalesce(getvariable('basis'), 'max') AS basis
),
eligible AS (
    SELECT fc.sha
    FROM file_changes fc
    JOIN commits c USING (sha)
    WHERE NOT c.is_merge OR getvariable('include_merges')
    GROUP BY fc.sha
    HAVING count(DISTINCT fc.path) <= (SELECT max_commit_files FROM params)
),
touches AS (
    SELECT DISTINCT fc.sha, fc.path
    FROM file_changes fc
    JOIN eligible USING (sha)
    JOIN files f ON f.path = fc.path
),
revs AS (SELECT path, count(*) AS n FROM touches GROUP BY path),
pairs AS (
    SELECT a.path AS path_a, b.path AS path_b, count(*) AS shared
    FROM touches a JOIN touches b ON a.sha = b.sha AND a.path < b.path
    GROUP BY ALL
),
scored AS (
    SELECT p.path_a, p.path_b, p.shared, ra.n AS commits_a, rb.n AS commits_b,
           100.0 * p.shared / greatest(ra.n, rb.n) AS pct_max,
           100.0 * p.shared / ((ra.n + rb.n) / 2.0) AS pct_average
    FROM pairs p
    JOIN revs ra ON ra.path = p.path_a
    JOIN revs rb ON rb.path = p.path_b
)
SELECT path_a, path_b, shared, commits_a, commits_b,
       round(pct_max, 1) AS coupling_pct_max, round(pct_average, 1) AS coupling_pct_average
FROM scored, params
WHERE shared >= params.min_shared
  AND (CASE WHEN params.basis = 'average' THEN pct_average ELSE pct_max END) >= params.min_coupling_pct
ORDER BY (CASE WHEN params.basis = 'average' THEN pct_average ELSE pct_max END) DESC, shared DESC, path_a, path_b;
