-- Question: how often, and by how many authors, was each current unit changed?
-- Rows: every unit of the current snapshot with the commits of its lineage
-- (unit_history, views/lineage.sql): its own changes and those of the units it continues
-- across file renames, renames, moves and moved bodies, each commit once; a unit that
-- kept its name after its body moved away counts from that commit on. Units never seen
-- in history get 0 commits. Which changes credit an enclosing unit (impl, module, type)
-- follows `attribution` (views/unit_changes.sql): by default only those to its own lines,
-- not those inside its methods or inner modules.
-- commits_by_name: the commits of the unit's own (path, kind, qualified_name) in
-- unit_changes, the count before lineage; ancestors: the other identities its history
-- comes from; successors_at_split: 1, or the most successors a link of its lineage gave
-- its predecessor (a split shares that history among them); move_commits: how many of
-- `commits` are commits at which its lineage links an identity (unit_history's
-- via_link: the move, rename, split or merge itself), which the co-change views leave
-- out.
-- Evidence: process (history) metrics outperform static metrics for defect
-- prediction (Majumder et al. 2022, one research group). Ranking by
-- churn x size ("hotspots") has no independent validation; the only studies
-- found are by the vendor of the method.
-- Parameters (DuckDB variables; default when unset):
--   since_time NULL (all commits; or a timestamp, compared with author_time),
--   kind NULL (all kinds; or a unit kind such as 'function');
--   include_merges (00_load.sql, false) also counts merges against their first parent;
--   lineage_threshold (views/lineage.sql, 0.5); attribution (views/unit_changes.sql,
--   'innermost').
WITH params AS (
    SELECT CAST(getvariable('since_time') AS TIMESTAMP) AS since_time,
           CAST(getvariable('kind') AS VARCHAR) AS kind
),
-- unit_history and unit_lineage_counts (views/lineage.sql) restated over one read of the
-- lineage segments and of unit_changes: each view reads both again, and live every
-- read derives the blob units once more.
segments AS MATERIALIZED (
    SELECT * FROM unit_lineage_segments
),
changes AS MATERIALIZED (
    SELECT uc.*, o.ord FROM unit_changes uc JOIN commit_order o USING (sha)
),
history AS (
    SELECT DISTINCT s.unit_id, c.sha
    FROM segments s
    JOIN changes c ON c.path = s.path AND c.unit_kind = s.kind AND c.unit_qualified_name = s.qualified_name
     AND c.ord >= s.from_ord AND c.ord <= s.until_ord
),
-- unit_history's via_link: the commits at which the unit's lineage links an identity.
link_commits AS (
    SELECT DISTINCT unit_id, sha FROM segments WHERE sha IS NOT NULL
),
lineage_counts AS (
    SELECT unit_id,
           count(DISTINCT (path, kind, qualified_name)) FILTER (WHERE NOT is_self) AS ancestors,
           max(successors) AS successors_at_split
    FROM segments
    GROUP BY unit_id
),
eligible AS MATERIALIZED (
    SELECT c.sha, c.author_name, c.author_time
    FROM commits c CROSS JOIN params
    WHERE (NOT c.is_merge OR getvariable('include_merges'))
      AND (params.since_time IS NULL OR c.author_time >= params.since_time)
),
per_unit AS (
    SELECT h.unit_id,
           count(DISTINCT h.sha) AS commits,
           count(DISTINCT e.author_name) AS authors,
           min(e.author_time) AS first_change,
           max(e.author_time) AS last_change,
           count(DISTINCT k.sha) AS move_commits
    FROM history h
    JOIN eligible e USING (sha)
    LEFT JOIN link_commits k USING (unit_id, sha)
    GROUP BY h.unit_id
),
by_name AS (
    SELECT uc.path, uc.unit_kind, uc.unit_qualified_name, count(DISTINCT uc.sha) AS commits
    FROM changes uc
    JOIN eligible e USING (sha)
    GROUP BY ALL
)
SELECT f.path, f.language, u.kind, u.qualified_name, u.start_line, u.loc,
       coalesce(p.commits, 0) AS commits, coalesce(p.authors, 0) AS authors,
       p.first_change, p.last_change,
       coalesce(n.commits, 0) AS commits_by_name, l.ancestors, l.successors_at_split,
       coalesce(p.move_commits, 0) AS move_commits
FROM units u
JOIN files f USING (file_id)
JOIN lineage_counts l USING (unit_id)
CROSS JOIN params
LEFT JOIN per_unit p USING (unit_id)
LEFT JOIN by_name n
  ON n.path = f.path AND n.unit_kind = u.kind AND n.unit_qualified_name = u.qualified_name
WHERE params.kind IS NULL OR u.kind = params.kind
ORDER BY commits DESC, u.loc DESC, f.path, u.start_line, u.unit_id;
