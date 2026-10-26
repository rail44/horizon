-- Retrospective, step 1b (optional): the ground truth through lineage. Run on a checkout
-- of the range's end commit with the whole history (no `since`), so that every link the
-- lineage views find inside the range is there (a pure file rename links only units
-- that history has seen), and with retro_since set to the range's base:
--   duckdb-ext/sql/run.sh sql END duckdb-ext/sql/retro/lineage.sql retro_since=BASE \
--       retro_reviews=/abs/reviews.json \
--       retro_record_preserves=/abs/duckdb-ext/sql/retro/september-2026-record-preserves.csv \
--       retro_lineage_changes=/abs/lineage-changes.parquet \
--       retro_lineage_preserves=/abs/lineage-preserves.parquet
-- signals.sql reads the two files (variables of the same names) on the baseline checkout.
--
-- The walk is unit_lineage_segments' (views/lineage.sql), started from a given identity
-- at a given commit instead of from each current unit, and restricted to the range:
-- only successions (other than same_name) of commits in BASE..HEAD are followed, and only
-- body_move restarts of those commits cut an identity's history. An identity reached
-- with no restart before the link it was reached through (from_ord 0) reaches back to
-- the start of the range, BASE: it is a candidate baseline identity, which signals.sql
-- keeps only when the baseline checkout has a unit with that (path, kind,
-- qualified_name). The start identity itself is the walk's depth 0; its own restart
-- counts when it is at or before the start commit (as a current unit's does).
--
-- retro_lineage_changes: one row per (unit change of the range, modified or removed,
-- merges following include_merges) x (identity of its walk that reaches BASE), the
-- change's own identity at depth 0 included when it does. Columns: sha, change_path,
-- change_kind_of_unit, change_qualified_name, change_kind, path, kind, qualified_name,
-- depth, link, link_sha.
-- retro_lineage_preserves: the same for each preserve source (signals.sql's: the ledger's
-- reviews and the record rows, same naming rules), started from the units of HEAD it
-- names, at the review's commit when that commit is in the range (else at HEAD). Columns:
-- source, source_path, owner, name, variant, named_path, named_kind,
-- named_qualified_name, path, kind, qualified_name, depth, link, link_sha.

SET VARIABLE retro_sweep = (
    SELECT list(sha) FROM git_commits(getvariable('repo'), since := getvariable('retro_since'),
                                      include_merges := true));

CREATE OR REPLACE TEMP TABLE retro_ord AS SELECT sha, ord FROM commit_order;

CREATE OR REPLACE TEMP TABLE retro_successions AS
SELECT s.*, o.ord,
       count(*) OVER (PARTITION BY s.sha, s.pred_path, s.pred_kind, s.pred_qualified_name) AS successors
FROM unit_successions s JOIN retro_ord o USING (sha)
WHERE s.link <> 'same_name' AND list_contains(getvariable('retro_sweep'), s.sha);

-- As in unit_lineage_segments: the stub of a body_move restarts at that commit.
CREATE OR REPLACE TEMP TABLE retro_restarts AS
SELECT DISTINCT s.ord, coalesce(f.path, s.pred_path) AS path, s.pred_kind AS kind,
       s.pred_qualified_name AS qualified_name
FROM retro_successions s
LEFT JOIN file_changes f ON f.sha = s.sha AND f.old_path = s.pred_path AND f.path <> f.old_path
WHERE s.link = 'body_move';

CREATE OR REPLACE TEMP TABLE retro_links AS
SELECT s.*,
       coalesce((SELECT max(r.ord) FROM retro_restarts r
                 WHERE r.path = s.pred_path AND r.kind = s.pred_kind
                   AND r.qualified_name = s.pred_qualified_name AND r.ord < s.ord), 0) AS pred_from
FROM retro_successions s;

-- Walk starts: the range's unit changes, and the units of HEAD the preserve sources name.
CREATE OR REPLACE TEMP TABLE retro_change_starts AS
SELECT DISTINCT uc.sha, uc.path, uc.unit_kind AS kind, uc.unit_qualified_name AS qualified_name,
       uc.change_kind, o.ord AS until_ord
FROM unit_changes uc
JOIN commits c USING (sha)
JOIN retro_ord o USING (sha)
WHERE list_contains(getvariable('retro_sweep'), uc.sha)
  AND (NOT c.is_merge OR getvariable('include_merges'))
  AND uc.change_kind IN ('modified', 'removed');

CREATE OR REPLACE TEMP TABLE retro_preserve_sources AS
SELECT 'reviews.json ' || substr(r.commit, 1, 8) AS source, r.target.file AS path, r.target.owner AS owner,
       r.target.name AS name, r.target.variant AS variant, r.commit AS review_commit
FROM (SELECT unnest(reviews) AS r FROM read_json(getvariable('retro_reviews'), maximum_object_size := 100000000))
WHERE r.decision = 'preserve'
UNION ALL
SELECT record, path, '<any>', name, NULL, NULL FROM read_csv(getvariable('retro_record_preserves'), header := true);

-- The naming rules of signals.sql's retro_preserve_map, over HEAD's units.
CREATE OR REPLACE TEMP TABLE retro_preserve_starts AS
SELECT s.source, s.path AS source_path, s.owner, s.name, s.variant, u.path, u.kind, u.qualified_name,
       coalesce(o.ord, (SELECT max(ord) FROM retro_ord)) AS until_ord
FROM retro_preserve_sources s
JOIN (SELECT DISTINCT f.path, u.kind, u.qualified_name FROM units u JOIN files f USING (file_id)) u
  ON u.path = s.path
 AND CASE WHEN s.owner = '<free>'
          THEN (u.qualified_name = s.name AND u.kind IN ('function', 'type', 'module', 'macro'))
               OR (u.kind = 'function' AND u.qualified_name LIKE '%.' || s.name)
          WHEN s.owner = '<any>'
          THEN u.qualified_name = s.name OR (u.kind IN ('function', 'method') AND u.qualified_name LIKE '%.' || s.name)
          ELSE u.kind = 'method' AND (u.qualified_name = s.owner || '.' || s.name
                                      OR u.qualified_name LIKE '%.' || s.owner || '.' || s.name)
     END
LEFT JOIN retro_ord o ON o.sha = s.review_commit AND list_contains(getvariable('retro_sweep'), s.review_commit);

CREATE OR REPLACE TEMP TABLE retro_starts AS
SELECT row_number() OVER () AS start_id, *
FROM (SELECT path, kind, qualified_name, until_ord FROM retro_change_starts
      UNION SELECT path, kind, qualified_name, until_ord FROM retro_preserve_starts);

CREATE OR REPLACE TEMP TABLE retro_walk AS
WITH RECURSIVE walk(start_id, path, kind, qualified_name, from_ord, until_ord, depth, link, link_sha) AS (
    SELECT s.start_id, s.path, s.kind, s.qualified_name,
           coalesce((SELECT max(r.ord) FROM retro_restarts r
                     WHERE r.path = s.path AND r.kind = s.kind AND r.qualified_name = s.qualified_name
                       AND r.ord <= s.until_ord), 0),
           s.until_ord, 0, NULL::VARCHAR, NULL::VARCHAR
    FROM retro_starts s
    UNION
    SELECT w.start_id, l.pred_path, l.pred_kind, l.pred_qualified_name, l.pred_from, l.ord - 1, w.depth + 1,
           l.link, l.sha
    FROM walk w
    JOIN retro_links l ON l.succ_path = w.path AND l.succ_kind = w.kind AND l.succ_qualified_name = w.qualified_name
     AND l.ord > w.from_ord AND l.ord <= w.until_ord
)
SELECT start_id, path, kind, qualified_name, min(depth) AS depth,
       arg_min(link, depth) AS link, arg_min(link_sha, depth) AS link_sha
FROM walk
WHERE from_ord = 0
GROUP BY start_id, path, kind, qualified_name;

COPY (
    SELECT c.sha, c.path AS change_path, c.kind AS change_kind_of_unit, c.qualified_name AS change_qualified_name,
           c.change_kind, w.path, w.kind, w.qualified_name, w.depth, w.link, w.link_sha
    FROM retro_change_starts c
    JOIN retro_starts s USING (path, kind, qualified_name, until_ord)
    JOIN retro_walk w USING (start_id)
    ORDER BY c.sha, c.path, c.kind, c.qualified_name, c.change_kind, w.depth, w.path, w.kind, w.qualified_name
) TO (getvariable('retro_lineage_changes')) (FORMAT parquet);

COPY (
    SELECT p.source, p.source_path, p.owner, p.name, p.variant, p.path AS named_path, p.kind AS named_kind,
           p.qualified_name AS named_qualified_name, w.path, w.kind, w.qualified_name, w.depth, w.link, w.link_sha
    FROM retro_preserve_starts p
    JOIN retro_starts s USING (path, kind, qualified_name, until_ord)
    JOIN retro_walk w USING (start_id)
    ORDER BY p.source, p.source_path, p.name, p.variant, w.depth, w.path, w.kind, w.qualified_name
) TO (getvariable('retro_lineage_preserves')) (FORMAT parquet);

.print
.print ## Range, links and walk starts
SELECT len(getvariable('retro_sweep')) AS range_commits,
       (SELECT count(*) FROM retro_successions) AS links,
       (SELECT count(*) FROM retro_restarts) AS restarts,
       (SELECT count(*) FROM retro_change_starts) AS change_starts,
       (SELECT count(*) FROM retro_preserve_starts) AS preserve_starts;

.print
.print ## Unit changes and the identities they reach at the range's start
SELECT count(*) AS rows_,
       count(DISTINCT (sha, change_path, change_kind_of_unit, change_qualified_name, change_kind)) AS changes,
       count(DISTINCT (path, kind, qualified_name)) AS reached_identities,
       count(DISTINCT (path, kind, qualified_name)) FILTER (WHERE depth > 0) AS reached_through_links
FROM read_parquet(getvariable('retro_lineage_changes'));

.print
.print ## Preserve sources and the identities they reach at the range's start
SELECT * FROM read_parquet(getvariable('retro_lineage_preserves'));
