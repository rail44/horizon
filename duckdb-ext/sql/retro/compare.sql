-- Retrospective check of the before/after comparison (queries/compare_units.sql,
-- docs/research/code-db-compare-2026-10-28.md). Run on a checkout of the range's end
-- with the whole history cached (no `since`), after changes.sql and lineage.sql wrote
-- their files for the same range:
--   duckdb-ext/code-db/target/release/code-db sql END duckdb-ext/sql/retro/compare.sql compare_before=BASE \
--       scip_before=/abs/index-of-BASE.scip retro_changes=/abs/changes.parquet \
--       retro_lineage_preserves=/abs/lineage-preserves.parquet
-- Prints:
--   1. compare_units' rows by status and ambiguous, and snapshot_missing;
--   2. the correspondence of unit_at_before against lineage.sql's walk (the successions
--      of BASE..HEAD only, the identities reaching BASE with no restart, kept when the
--      before tree has them), started from every current identity: pairs in both, only
--      in one, and a sample of each kind of difference;
--   3. compare_units against the ground truth: the identities changes.sql records as
--      added in the range that exist at HEAD (expected `added`), those it records as
--      removed that exist at BASE and not at HEAD (expected `removed`), and the HEAD
--      identities the preserve sources name (expected kept, renamed or moved; with how
--      many have commits >= 1); per category, the statuses found.

SET VARIABLE cmp_query = (SELECT rtrim(trim(content), ';') FROM read_text('duckdb-ext/sql/queries/compare_units.sql'));
CREATE OR REPLACE TEMP TABLE cmp_rows AS FROM query(getvariable('cmp_query'));

.print
.print ## 1. compare_units by status
SELECT status, ambiguous, count(*) AS rows_, count(*) FILTER (WHERE commits >= 1) AS with_commits,
       any_value(snapshot_missing) AS snapshot_missing
FROM cmp_rows GROUP BY status, ambiguous ORDER BY list_position(['kept', 'renamed', 'moved', 'added', 'removed', 'missing'], status), ambiguous;

-- 2. The walk of lineage.sql from every current identity.
SET VARIABLE cmp_range = (SELECT list(sha) FROM compare_range);
CREATE OR REPLACE TEMP TABLE cmp_ord AS SELECT sha, ord FROM commit_order;
CREATE OR REPLACE TEMP TABLE cmp_successions AS
SELECT s.*, o.ord
FROM unit_successions s JOIN cmp_ord o USING (sha)
WHERE s.link <> 'same_name' AND list_contains(getvariable('cmp_range'), s.sha);
CREATE OR REPLACE TEMP TABLE cmp_restarts AS
SELECT DISTINCT s.ord, coalesce(f.path, s.pred_path) AS path, s.pred_kind AS kind, s.pred_qualified_name AS qualified_name
FROM cmp_successions s
LEFT JOIN file_changes f ON f.sha = s.sha AND f.old_path = s.pred_path AND f.path <> f.old_path
WHERE s.link = 'body_move';
CREATE OR REPLACE TEMP TABLE cmp_links AS
SELECT s.*,
       coalesce((SELECT max(r.ord) FROM cmp_restarts r
                 WHERE r.path = s.pred_path AND r.kind = s.pred_kind
                   AND r.qualified_name = s.pred_qualified_name AND r.ord < s.ord), 0) AS pred_from
FROM cmp_successions s;
CREATE OR REPLACE TEMP TABLE cmp_starts AS
SELECT row_number() OVER () AS start_id, *, (SELECT max(ord) FROM cmp_ord) AS until_ord
FROM (SELECT DISTINCT f.path, u.kind, u.qualified_name FROM units u JOIN files f USING (file_id));
CREATE OR REPLACE TEMP TABLE cmp_before_ids AS
SELECT DISTINCT f.path, u.kind, u.qualified_name FROM snapshot_units u JOIN snapshot_files f USING (file_id);
CREATE OR REPLACE TEMP TABLE cmp_walk AS
WITH RECURSIVE walk(start_id, path, kind, qualified_name, from_ord, until_ord, depth) AS (
    SELECT s.start_id, s.path, s.kind, s.qualified_name,
           coalesce((SELECT max(r.ord) FROM cmp_restarts r
                     WHERE r.path = s.path AND r.kind = s.kind AND r.qualified_name = s.qualified_name
                       AND r.ord <= s.until_ord), 0),
           s.until_ord, 0
    FROM cmp_starts s
    UNION
    SELECT w.start_id, l.pred_path, l.pred_kind, l.pred_qualified_name, l.pred_from, l.ord - 1, w.depth + 1
    FROM walk w
    JOIN cmp_links l ON l.succ_path = w.path AND l.succ_kind = w.kind AND l.succ_qualified_name = w.qualified_name
     AND l.ord > w.from_ord AND l.ord <= w.until_ord
)
SELECT DISTINCT s.path AS path_after, s.kind AS kind_after, s.qualified_name AS name_after,
       w.path AS path_before, w.kind AS kind_before, w.qualified_name AS name_before
FROM walk w JOIN cmp_starts s USING (start_id)
JOIN cmp_before_ids b ON b.path = w.path AND b.kind = w.kind AND b.qualified_name = w.qualified_name
WHERE w.from_ord = 0;
CREATE OR REPLACE TEMP TABLE cmp_segments_pairs AS
SELECT DISTINCT f.path AS path_after, u.kind AS kind_after, u.qualified_name AS name_after,
       b.path AS path_before, b.kind AS kind_before, b.qualified_name AS name_before, b.depth, b.link, b.sha
FROM unit_at_before b JOIN units u USING (unit_id) JOIN files f USING (file_id);

.print
.print ## 2. unit_at_before against the walk of lineage.sql (pairs of identities)
SELECT (SELECT count(*) FROM (SELECT path_after, kind_after, name_after, path_before, kind_before, name_before
                              FROM cmp_segments_pairs INTERSECT SELECT * FROM cmp_walk)) AS both_,
       (SELECT count(*) FROM (SELECT path_after, kind_after, name_after, path_before, kind_before, name_before
                              FROM cmp_segments_pairs EXCEPT SELECT * FROM cmp_walk)) AS only_segments,
       (SELECT count(*) FROM (SELECT * FROM cmp_walk EXCEPT SELECT path_after, kind_after, name_after, path_before,
                                                               kind_before, name_before FROM cmp_segments_pairs)) AS only_walk;
.print only in unit_at_before (up to 20)
SELECT p.* FROM cmp_segments_pairs p
WHERE NOT EXISTS (SELECT 1 FROM cmp_walk w WHERE w.path_after = p.path_after AND w.kind_after = p.kind_after
                  AND w.name_after = p.name_after AND w.path_before = p.path_before AND w.kind_before = p.kind_before
                  AND w.name_before = p.name_before)
ORDER BY ALL LIMIT 20;
.print only in the walk (up to 20)
SELECT * FROM (FROM cmp_walk EXCEPT SELECT path_after, kind_after, name_after, path_before, kind_before, name_before
               FROM cmp_segments_pairs) ORDER BY ALL LIMIT 20;

-- 3. The ground truth.
CREATE OR REPLACE TEMP TABLE cmp_after_ids AS
SELECT DISTINCT f.path, u.kind, u.qualified_name FROM units u JOIN files f USING (file_id);
CREATE OR REPLACE TEMP TABLE cmp_truth AS
SELECT 'added' AS category, path, unit_kind AS kind, unit_qualified_name AS qualified_name, 'after' AS side
FROM (SELECT DISTINCT path, unit_kind, unit_qualified_name FROM read_parquet(getvariable('retro_changes'))
      WHERE change_kind = 'added') c
WHERE (path, unit_kind, unit_qualified_name) IN (SELECT (path, kind, qualified_name) FROM cmp_after_ids)
UNION ALL
SELECT 'removed', path, unit_kind, unit_qualified_name, 'before'
FROM (SELECT DISTINCT path, unit_kind, unit_qualified_name FROM read_parquet(getvariable('retro_changes'))
      WHERE change_kind = 'removed') c
WHERE (path, unit_kind, unit_qualified_name) IN (SELECT (path, kind, qualified_name) FROM cmp_before_ids)
  AND (path, unit_kind, unit_qualified_name) NOT IN (SELECT (path, kind, qualified_name) FROM cmp_after_ids)
UNION ALL
SELECT DISTINCT 'preserve', named_path, named_kind, named_qualified_name, 'after'
FROM read_parquet(getvariable('retro_lineage_preserves'));
-- Each truth identity with the statuses of its compare_units rows on its side.
CREATE OR REPLACE TEMP TABLE cmp_truth_status AS
SELECT t.category, t.path, t.kind, t.qualified_name,
       list(DISTINCT r.status ORDER BY r.status) AS statuses, bool_or(r.ambiguous) AS ambiguous,
       max(r.commits) AS commits,
       list(DISTINCT {'path': coalesce(r.path_after, r.path_before), 'name': coalesce(r.qualified_name_after, '-')
                      || ' <- ' || coalesce(r.qualified_name_before, '-')}) AS rows_
FROM cmp_truth t
LEFT JOIN cmp_rows r
  ON (t.side = 'after' AND r.path_after = t.path AND r.kind_after = t.kind AND r.qualified_name_after = t.qualified_name)
  OR (t.side = 'before' AND r.path_before = t.path AND r.kind_before = t.kind AND r.qualified_name_before = t.qualified_name)
GROUP BY ALL;

.print
.print ## 3. compare_units against the ground truth (statuses found per truth identity)
SELECT category, statuses, ambiguous, count(*) AS identities, count(*) FILTER (WHERE commits >= 1) AS with_commits
FROM cmp_truth_status GROUP BY ALL ORDER BY category, identities DESC;
.print disagreements (up to 40)
SELECT category, path, kind, qualified_name, statuses, ambiguous, commits, rows_
FROM cmp_truth_status
WHERE NOT ((category = 'added' AND statuses = ['added'])
           OR (category = 'removed' AND statuses = ['removed'])
           OR (category = 'preserve' AND list_has_all(['kept', 'moved', 'renamed'], statuses) AND len(statuses) > 0))
ORDER BY category, path, qualified_name LIMIT 40;
