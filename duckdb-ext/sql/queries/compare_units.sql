-- Question: between an earlier commit and HEAD, which units were kept, renamed, moved,
-- added and removed, and how much did each change?
-- Rows: one per pair of a unit identity at HEAD (path, kind, qualified_name; the units
-- sharing one, such as two `impl Foo` blocks of a file, are one identity with their loc
-- summed) and the identity it continues at compare_before (views/snapshot.sql,
-- unit_at_before: its lineage's identity at that commit, when the before tree has a unit
-- with it); one per identity at HEAD that continues none, one per identity at
-- compare_before that no current unit continues, and one per before file whose blob
-- is not in the blob layer.
--   status   kept     the same identity on both sides
--            renamed  linked through the lineage, same path, another kind or name
--            moved    linked through the lineage, another path (a file rename, or a move
--                     to another file, with or without a new name)
--            added    a unit at HEAD with no identity at compare_before (the stub that
--                     kept a name after its body moved away counts as added: history
--                     follows the body)
--            removed  a unit at compare_before no current unit continues
--            missing  a file of the before tree whose units are unknown (snapshot_missing);
--                     path_before is the file, the other columns NULL
--   ambiguous  the pair is one of several for its HEAD identity (a merge) or its before
--            identity (a split), or a link of the HEAD unit's lineage inside the range is
--            ambiguous (unit_successions); never for added, removed or missing
-- commits: the distinct commits of compare_before..HEAD in the HEAD units' lineage
-- (unit_history; merges only with include_merges), move_commits how many of them link an
-- identity (as unit_churn reports them); fan_in_before/fan_in_after: unit_fan_in on each
-- side (the most among the identity's units; fan_in_before NULL without scip_before);
-- module_before/module_after: the module each side's unit belongs to (unit_module), the
-- before one as named in the before tree. snapshot_missing: the number of missing files
-- (on every row).
-- Ordering: status in the order above, then commits, then |loc_after - loc_before|
-- (both descending), then the path.
-- Needs views/graph.sql (module and fan-in), and the persistent cache: live, the before
-- side parses every blob.
-- Parameters (DuckDB variables; default when unset):
--   compare_before (required: a revision, an ancestor of HEAD within since..HEAD),
--   scip_before NULL (the path of a SCIP index of compare_before's tree);
--   include_merges (00_load.sql, false); lineage_threshold (views/lineage.sql, 0.5);
--   attribution (views/unit_changes.sql, 'innermost').
WITH before AS MATERIALIZED (
    SELECT sha, ord FROM compare_commit
),
range_commits AS MATERIALIZED (
    SELECT r.sha FROM compare_range r JOIN commits c USING (sha)
    WHERE NOT c.is_merge OR getvariable('include_merges')
),
missing AS MATERIALIZED (
    SELECT path, blob_id FROM snapshot_missing
),
after_units AS MATERIALIZED (
    SELECT u.unit_id, f.path, u.kind, u.qualified_name, u.loc, m.module, fi.fan_in_files
    FROM units u JOIN files f USING (file_id)
    LEFT JOIN unit_module m USING (unit_id)
    LEFT JOIN unit_fan_in fi USING (unit_id)
),
before_units AS MATERIALIZED (
    SELECT u.unit_id, f.path, u.kind, u.qualified_name, u.loc, m.module, fi.fan_in_files
    FROM snapshot_units u JOIN snapshot_files f USING (file_id)
    LEFT JOIN snapshot_unit_module m USING (unit_id)
    LEFT JOIN snapshot_unit_fan_in fi USING (unit_id)
),
after_ids AS MATERIALIZED (
    SELECT path, kind, qualified_name, sum(loc) AS loc, any_value(module) AS module,
           coalesce(max(fan_in_files), 0) AS fan_in
    FROM after_units GROUP BY ALL
),
before_ids AS MATERIALIZED (
    SELECT path, kind, qualified_name, sum(loc) AS loc, any_value(module) AS module,
           CASE WHEN getvariable('scip_before') IS NOT NULL THEN coalesce(max(fan_in_files), 0) END AS fan_in
    FROM before_units GROUP BY ALL
),
links AS MATERIALIZED (
    SELECT DISTINCT a.path AS path_after, a.kind AS kind_after, a.qualified_name AS name_after,
           b.path AS path_before, b.kind AS kind_before, b.qualified_name AS name_before
    FROM unit_at_before b JOIN after_units a USING (unit_id)
),
-- HEAD identities whose lineage passes an ambiguous link inside the range.
ambiguous_lineage AS MATERIALIZED (
    SELECT DISTINCT a.path, a.kind, a.qualified_name
    FROM unit_lineage_segments s
    JOIN after_units a USING (unit_id)
    JOIN range_commits r ON r.sha = s.sha
    JOIN unit_successions x ON x.sha = s.sha AND x.link = s.link AND x.pred_path = s.path
         AND x.pred_kind = s.kind AND x.pred_qualified_name = s.qualified_name
    WHERE x.ambiguous
),
history AS MATERIALIZED (
    SELECT a.path, a.kind, a.qualified_name, count(DISTINCT h.sha) AS commits,
           count(DISTINCT h.sha) FILTER (WHERE h.via_link) AS move_commits
    FROM unit_history h
    JOIN after_units a USING (unit_id)
    JOIN range_commits r USING (sha)
    GROUP BY ALL
),
pairs AS (
    SELECT l.*,
           count(*) OVER (PARTITION BY l.path_after, l.kind_after, l.name_after) > 1
           OR count(*) OVER (PARTITION BY l.path_before, l.kind_before, l.name_before) > 1 AS fanned
    FROM links l
    UNION ALL
    SELECT a.path, a.kind, a.qualified_name, NULL, NULL, NULL, false
    FROM after_ids a
    WHERE NOT EXISTS (SELECT 1 FROM links l WHERE l.path_after = a.path AND l.kind_after = a.kind
                                              AND l.name_after = a.qualified_name)
    UNION ALL
    SELECT NULL, NULL, NULL, b.path, b.kind, b.qualified_name, false
    FROM before_ids b
    WHERE NOT EXISTS (SELECT 1 FROM links l WHERE l.path_before = b.path AND l.kind_before = b.kind
                                              AND l.name_before = b.qualified_name)
),
rows_ AS (
    SELECT CASE
               WHEN p.path_before IS NULL THEN 'added'
               WHEN p.path_after IS NULL THEN 'removed'
               WHEN p.path_after <> p.path_before THEN 'moved'
               WHEN p.kind_after <> p.kind_before OR p.name_after <> p.name_before THEN 'renamed'
               ELSE 'kept'
           END AS status,
           p.path_after IS NOT NULL AND p.path_before IS NOT NULL
           AND (p.fanned OR al.path IS NOT NULL) AS ambiguous,
           p.path_before, p.kind_before, p.name_before AS qualified_name_before,
           p.path_after, p.kind_after, p.name_after AS qualified_name_after,
           bi.loc AS loc_before, ai.loc AS loc_after,
           CASE WHEN p.path_after IS NOT NULL THEN coalesce(h.commits, 0) END AS commits,
           CASE WHEN p.path_after IS NOT NULL THEN coalesce(h.move_commits, 0) END AS move_commits,
           bi.fan_in AS fan_in_before, ai.fan_in AS fan_in_after,
           bi.module AS module_before, ai.module AS module_after
    FROM pairs p
    LEFT JOIN after_ids ai ON ai.path = p.path_after AND ai.kind = p.kind_after AND ai.qualified_name = p.name_after
    LEFT JOIN before_ids bi ON bi.path = p.path_before AND bi.kind = p.kind_before
         AND bi.qualified_name = p.name_before
    LEFT JOIN history h ON h.path = p.path_after AND h.kind = p.kind_after AND h.qualified_name = p.name_after
    LEFT JOIN ambiguous_lineage al ON al.path = p.path_after AND al.kind = p.kind_after
         AND al.qualified_name = p.name_after
    UNION ALL
    SELECT 'missing', false, path, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL
    FROM missing
)
SELECT r.*, (SELECT count(*) FROM missing) AS snapshot_missing
FROM rows_ r
CROSS JOIN before
ORDER BY list_position(['kept', 'renamed', 'moved', 'added', 'removed', 'missing'], r.status),
         r.commits DESC NULLS LAST, abs(coalesce(r.loc_after, 0) - coalesce(r.loc_before, 0)) DESC,
         coalesce(r.path_after, r.path_before), r.qualified_name_after, r.qualified_name_before,
         r.kind_after, r.kind_before;
