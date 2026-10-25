-- Question: how often, and by how many authors, was each current unit changed?
-- Rows: every unit of the current snapshot, joined to unit_changes by
-- (path, kind, qualified_name); units never seen in history get 0 commits.
-- Enclosing units (impl, module) count every commit that touched any part of them.
-- History under a previous path is not followed (a rename starts a new history).
-- Evidence: process (history) metrics outperform static metrics for defect
-- prediction (Majumder et al. 2022, one research group). Ranking by
-- churn x size ("hotspots") has no independent validation; the only studies
-- found are by the vendor of the method.
-- Parameters (DuckDB variables; default when unset):
--   since_time NULL (all commits; or a timestamp, compared with author_time),
--   kind NULL (all kinds; or a unit kind such as 'function');
--   include_merges (00_load.sql, false) also counts merges against their first parent.
WITH params AS (
    SELECT CAST(getvariable('since_time') AS TIMESTAMP) AS since_time,
           CAST(getvariable('kind') AS VARCHAR) AS kind
),
changes AS (
    SELECT DISTINCT uc.sha, uc.path, uc.unit_kind, uc.unit_qualified_name, c.author_name, c.author_time
    FROM unit_changes uc
    JOIN commits c USING (sha)
    CROSS JOIN params
    WHERE (NOT c.is_merge OR getvariable('include_merges'))
      AND (params.since_time IS NULL OR c.author_time >= params.since_time)
),
per_unit AS (
    SELECT path, unit_kind, unit_qualified_name,
           count(DISTINCT sha) AS commits,
           count(DISTINCT author_name) AS authors,
           min(author_time) AS first_change,
           max(author_time) AS last_change
    FROM changes
    GROUP BY ALL
)
SELECT f.path, f.language, u.kind, u.qualified_name, u.start_line, u.loc,
       coalesce(p.commits, 0) AS commits, coalesce(p.authors, 0) AS authors,
       p.first_change, p.last_change
FROM units u
JOIN files f USING (file_id)
CROSS JOIN params
LEFT JOIN per_unit p
  ON p.path = f.path AND p.unit_kind = u.kind AND p.unit_qualified_name = u.qualified_name
WHERE params.kind IS NULL OR u.kind = params.kind
ORDER BY commits DESC, u.loc DESC, f.path, u.start_line, u.unit_id;
