-- Unit lineage: which earlier unit identities each current unit continues, across file
-- renames, unit renames and moves, and bodies that moved under a kept name, so that the
-- history-based queries count the commits of the code rather than of the name.
-- Read after views/unit_changes.sql (and views/blobs.sql, views/history.sql).
--
-- An identity is (path, kind, qualified_name), as in unit_changes. A succession links a
-- predecessor identity (the pre-image side of a commit) to a successor identity (the
-- post-image side of the same commit). Links are found within one commit only:
--   same_name     a unit modified in place: its identity continues (informational; the
--                 identity's own commits are its history anyway).
--   file_rename   the file was renamed (file_changes.old_path -> path, gix's rename
--                 detection): every unit with the same kind and qualified name on both
--                 sides continues under the new path. For a pure rename (no hunks), the
--                 units are those of the new side of the old path's latest earlier change
--                 with hunks in the range.
--   content_match a removed unit and an added unit of the commit whose texts are similar.
--   body_move     a modified unit whose pre-image is dissimilar to its post-image while an
--                 added unit of the commit is similar to the pre-image: the pre-image
--                 links to the added unit, and the unit that kept the name (the stub)
--                 gets no predecessor at that commit: its lineage restarts there.
-- Similarity is the Jaccard index of the two texts' sets of lines, each line with its
-- whitespace runs collapsed (normalize_ws) and blank lines dropped; a unit's text is the
-- lines [start_line, end_line] of every unit with its identity in that blob. content_match
-- and body_move compare units of compatible kinds only (the same kind, or function and
-- method, which a move into or out of an impl turns into each other) and link every
-- candidate at or above `lineage_threshold` (default 0.5), so a split links one
-- predecessor to several successors and a merge several predecessors to one.
-- `ambiguous` marks a content_match or body_move link whose predecessor or successor
-- has several links at the commit, or whose two best candidates on either side are
-- within 0.1 of each other (a modified unit's own post-image counts as one of its
-- pre-image's candidates).
--
-- History follows the body, not the name, and is the union over every ancestor:
-- unit_lineage walks the successions back from each current unit; an ancestor reached
-- through a link at commit c contributes its commits before c, and an identity whose
-- lineage restarted at a body_move contributes only its commits from that commit on.
-- unit_history counts each commit once (count(DISTINCT sha)), however many ancestors it
-- touched; commit order is commit_order's.
--
-- Parameters (DuckDB variables; default when unset): lineage_threshold 0.5. refresh.sql
-- sets lineage_scope (the commits to derive successions for; NULL = all) and narrows the
-- text snapshot below; it keeps the successions per commit (see refresh.sql).

SET VARIABLE lineage_threshold = coalesce(getvariable('lineage_threshold'), 0.5);

-- commit_order(sha, ord): the position of each commit of `commits` in git log's order
-- reversed, 1 = the oldest. git_commits emits the commits in git log's order from one
-- thread; refresh.sql and cache.sql replace this view with a copy taken in that order.
CREATE OR REPLACE VIEW commit_order AS
SELECT sha, count(*) OVER () - row_number() OVER () + 1 AS ord
FROM git_commits(getvariable('repo'), since := getvariable('since'),
                 include_merges := getvariable('include_merges'));

-- The blob texts the successions read: a snapshot like views/blobs.sql's, of every blob
-- in it here; refresh.sql narrows it to the blobs of the commits it derives.
SET VARIABLE lineage_text_snapshot = getvariable('blob_snapshot');
SET VARIABLE lineage_text_paths = CASE WHEN len(getvariable('lineage_text_snapshot')) = 0
    THEN [getvariable('blob_dir') || '/no-such-blob.rs']
    ELSE list_transform(getvariable('lineage_text_snapshot'), lambda b: b.file) END;

-- lineage_sources(blob_id, content): read_text over the text snapshot (a missing file
-- returns no row).
CREATE OR REPLACE VIEW lineage_sources AS
SELECT l.blob_id, t.content
FROM read_text(getvariable('lineage_text_paths')) t
JOIN (SELECT b.blob_id, b.file FROM (SELECT unnest(getvariable('lineage_text_snapshot')) AS b)) l
  ON l.file = t.filename;

-- derived_unit_successions: the successions of the commits of `lineage_scope` (all when
-- NULL), computed from unit_change_sides (views/unit_changes.sql), file_changes,
-- commit_order and the blob texts. unit_successions reads it live; refresh.sql stores
-- its rows per commit. Each source is read once (a view is expanded at every reference,
-- and live every expansion of the blob units parses every blob again).
-- The successions read the changes under attribution 'all' (unit_change_sides'
-- change_kind_all), whatever `attribution` is: a unit's movement is judged on its whole
-- text, inner units included (an impl whose methods moved out is a modified unit whose
-- pre-image may have moved elsewhere), and the successions, which both caches store, then
-- do not depend on the attribution the counting queries use.
CREATE OR REPLACE VIEW derived_unit_successions AS
WITH sides AS MATERIALIZED (
    SELECT sha, path, old_path, side, blob_id, kind, qualified_name, start_line, end_line,
           record_path_all AS record_path, change_kind_all AS change_kind
    FROM unit_change_sides
),
scope AS MATERIALIZED (
    SELECT sha FROM commits
    WHERE getvariable('lineage_scope') IS NULL OR list_contains(getvariable('lineage_scope'), sha)
),
orders AS MATERIALIZED (
    SELECT sha, ord FROM commit_order
),
-- One row per unit image: the post-image (side new) of added and modified units, the
-- pre-image (side old) of removed and modified ones. node_path is the path of the
-- identity on that side (a renamed file's old path for its pre-images), record_path the
-- path unit_changes records the change under.
images AS MATERIALIZED (
    SELECT DISTINCT sha, change_kind, side, record_path,
           CASE side WHEN 'old' THEN coalesce(old_path, path) ELSE path END AS node_path,
           kind, qualified_name, blob_id
    FROM sides
    WHERE change_kind IS NOT NULL AND sha IN (SELECT sha FROM scope)
),
image_keys AS MATERIALIZED (
    SELECT row_number() OVER (ORDER BY sha, side, record_path, node_path, kind, qualified_name, change_kind, blob_id) AS img, *
    FROM images
),
-- The lines of every unit with an image's identity in its blob.
spans AS MATERIALIZED (
    SELECT DISTINCT blob_id, kind, qualified_name, start_line, end_line
    FROM sides
    WHERE change_kind IS NOT NULL AND sha IN (SELECT sha FROM scope)
),
-- Renamed files (gix's rename detection in file_changes).
renames AS MATERIALIZED (
    SELECT DISTINCT f.sha, f.old_path, f.path
    FROM file_changes f
    WHERE f.old_path IS NOT NULL AND f.old_path <> f.path AND f.sha IN (SELECT sha FROM scope)
      AND getvariable('languages')[regexp_extract(f.path, '\.([^./]+)$', 1)] IS NOT NULL
),
-- A pure rename has no hunks, so no sides: its units are those of the new side of the
-- old path's latest earlier change in the range.
pure_renames AS MATERIALIZED (
    SELECT r.sha, r.old_path, r.path, arg_max(u.sha, o.ord) AS earlier_sha
    FROM renames r
    JOIN orders ro ON ro.sha = r.sha
    JOIN sides u ON u.path = r.old_path AND u.side = 'new'
    JOIN orders o ON o.sha = u.sha AND o.ord < ro.ord
    WHERE NOT EXISTS (SELECT 1 FROM sides x WHERE x.sha = r.sha AND x.path = r.path)
    GROUP BY ALL
),
rename_units AS MATERIALIZED (
    SELECT DISTINCT r.sha, r.old_path, r.path, o.kind, o.qualified_name
    FROM renames r
    JOIN sides o ON o.sha = r.sha AND o.path = r.path AND o.side = 'old'
    JOIN sides n ON n.sha = r.sha AND n.path = r.path AND n.side = 'new'
     AND n.kind = o.kind AND n.qualified_name = o.qualified_name
    UNION
    SELECT DISTINCT r.sha, r.old_path, r.path, u.kind, u.qualified_name
    FROM pure_renames r
    JOIN sides u ON u.sha = r.earlier_sha AND u.path = r.old_path AND u.side = 'new'
),
-- Each non-blank line of the blobs, as the hash of its whitespace-normalized text.
blob_lines AS MATERIALIZED (
    SELECT blob_id, line, hash(text) AS h
    FROM (SELECT blob_id, line, normalize_ws(raw) AS text
          FROM (SELECT s.blob_id, unnest(string_split(s.content, chr(10))) AS raw,
                       generate_subscripts(string_split(s.content, chr(10)), 1) AS line
                FROM lineage_sources s WHERE s.blob_id IN (SELECT blob_id FROM spans)))
    WHERE text <> ''
),
-- The line set of each (blob, identity).
unit_lines AS MATERIALIZED (
    SELECT DISTINCT s.blob_id, s.kind, s.qualified_name, l.h
    FROM spans s JOIN blob_lines l ON l.blob_id = s.blob_id AND l.line BETWEEN s.start_line AND s.end_line
),
image_lines AS MATERIALIZED (
    SELECT i.img, i.sha, l.h FROM image_keys i JOIN unit_lines l USING (blob_id, kind, qualified_name)
),
image_size AS MATERIALIZED (
    SELECT img, count(*) AS lines FROM image_lines GROUP BY img
),
-- Candidate pairs: pre-images (removed, modified) x post-images of added units, same
-- commit, compatible kinds, sharing at least one line.
pairs AS MATERIALIZED (
    SELECT p.img AS pred_img, s.img AS succ_img, count(*) AS shared
    FROM image_keys p
    JOIN image_keys s ON s.sha = p.sha AND s.change_kind = 'added'
     AND (s.kind = p.kind OR (s.kind IN ('function', 'method') AND p.kind IN ('function', 'method')))
    JOIN image_lines a ON a.img = p.img
    JOIN image_lines b ON b.img = s.img AND b.h = a.h
    WHERE p.side = 'old'
    GROUP BY ALL
),
-- A modified unit's pre-image against its post-image.
own_pairs AS MATERIALIZED (
    SELECT p.img AS pred_img, s.img AS succ_img
    FROM image_keys p
    JOIN image_keys s ON s.sha = p.sha AND s.change_kind = 'modified' AND s.side = 'new'
     AND s.record_path = p.record_path AND s.kind = p.kind AND s.qualified_name = p.qualified_name
    WHERE p.change_kind = 'modified' AND p.side = 'old'
),
own AS MATERIALIZED (
    SELECT o.pred_img, o.succ_img, count(b.h) AS shared
    FROM own_pairs o
    LEFT JOIN image_lines a ON a.img = o.pred_img
    LEFT JOIN image_lines b ON b.img = o.succ_img AND b.h = a.h
    GROUP BY ALL
),
scored AS MATERIALIZED (
    SELECT x.pred_img, x.succ_img, x.own,
           x.shared / (coalesce(sp.lines, 0) + coalesce(ss.lines, 0) - x.shared) AS similarity
    FROM (SELECT pred_img, succ_img, shared, false AS own FROM pairs
          UNION ALL SELECT pred_img, succ_img, shared, true FROM own) x
    LEFT JOIN image_size sp ON sp.img = x.pred_img
    LEFT JOIN image_size ss ON ss.img = x.succ_img
    WHERE coalesce(sp.lines, 0) + coalesce(ss.lines, 0) - x.shared > 0
),
-- Pre-images of modified units that moved: dissimilar to their post-image (or with no
-- line in common with it) and similar to an added unit.
moved AS MATERIALIZED (
    SELECT p.img
    FROM image_keys p
    WHERE p.change_kind = 'modified' AND p.side = 'old'
      AND coalesce((SELECT max(similarity) FROM scored s WHERE s.pred_img = p.img AND s.own), 0)
          < getvariable('lineage_threshold')
      AND EXISTS (SELECT 1 FROM scored s WHERE s.pred_img = p.img AND NOT s.own
                  AND s.similarity >= getvariable('lineage_threshold'))
),
matched AS MATERIALIZED (
    SELECT s.pred_img, s.succ_img, s.similarity,
           CASE WHEN p.change_kind = 'removed' THEN 'content_match' ELSE 'body_move' END AS link
    FROM scored s JOIN image_keys p ON p.img = s.pred_img
    WHERE NOT s.own AND s.similarity >= getvariable('lineage_threshold')
      AND (p.change_kind = 'removed' OR p.img IN (SELECT img FROM moved))
),
-- The best two candidates of each image, on either side (every candidate pair, linked
-- or not; a pre-image's own post-image included).
ranked AS (
    SELECT img, similarity, row_number() OVER (PARTITION BY img ORDER BY similarity DESC) AS rn
    FROM (SELECT pred_img AS img, similarity FROM scored
          UNION ALL SELECT succ_img, similarity FROM scored WHERE NOT own)
),
close_calls AS MATERIALIZED (
    SELECT img FROM ranked GROUP BY img
    HAVING count(*) FILTER (WHERE rn <= 2) = 2
       AND max(similarity) FILTER (WHERE rn = 1) - max(similarity) FILTER (WHERE rn = 2) < 0.1
),
matched_links AS (
    SELECT p.sha, p.node_path AS pred_path, p.kind AS pred_kind, p.qualified_name AS pred_qualified_name,
           s.node_path AS succ_path, s.kind AS succ_kind, s.qualified_name AS succ_qualified_name,
           m.link, m.similarity,
           count(*) OVER (PARTITION BY m.pred_img) > 1 OR count(*) OVER (PARTITION BY m.succ_img) > 1
               OR m.pred_img IN (SELECT img FROM close_calls) OR m.succ_img IN (SELECT img FROM close_calls) AS ambiguous
    FROM matched m
    JOIN image_keys p ON p.img = m.pred_img
    JOIN image_keys s ON s.img = m.succ_img
),
-- Modified units in place (not renamed, not moved).
same_name_links AS (
    SELECT p.sha, p.node_path, p.kind, p.qualified_name, s.node_path, s.kind, s.qualified_name,
           'same_name', coalesce(o.similarity, 0), false
    FROM own x
    JOIN image_keys p ON p.img = x.pred_img
    JOIN image_keys s ON s.img = x.succ_img
    LEFT JOIN scored o ON o.pred_img = x.pred_img AND o.succ_img = x.succ_img AND o.own
    WHERE p.node_path = s.node_path AND p.img NOT IN (SELECT img FROM moved)
),
-- Every unit of a renamed file that keeps its kind and name, unless its body moved.
file_rename_links AS (
    SELECT r.sha, r.old_path, r.kind, r.qualified_name, r.path, r.kind, r.qualified_name,
           'file_rename', coalesce(o.similarity, 1.0), false
    FROM rename_units r
    LEFT JOIN image_keys p ON p.sha = r.sha AND p.side = 'old' AND p.change_kind = 'modified'
         AND p.node_path = r.old_path AND p.kind = r.kind AND p.qualified_name = r.qualified_name
    LEFT JOIN scored o ON o.pred_img = p.img AND o.own
    WHERE p.img IS NULL OR p.img NOT IN (SELECT img FROM moved)
)
SELECT * FROM matched_links
UNION ALL SELECT * FROM same_name_links
UNION ALL SELECT * FROM file_rename_links;

-- unit_successions(sha, pred_path, pred_kind, pred_qualified_name, succ_path, succ_kind,
--     succ_qualified_name, link, similarity, ambiguous): every succession of the range.
-- refresh.sql and cache.sql replace it with their copies.
CREATE OR REPLACE VIEW unit_successions AS SELECT * FROM derived_unit_successions;

-- unit_lineage_segments: for each current unit, the identities whose commits are its
-- history, each with the commit range it contributes: from from_ord up to until_ord
-- (both inclusive), in commit_order positions. sha is the link commit through which the
-- identity was reached (NULL for the unit itself), link its kind, depth the number of
-- links from the unit, successors the number of successor identities that link's
-- predecessor has at that commit (more than 1: a split), is_self whether the identity
-- is the unit's own.
CREATE OR REPLACE VIEW unit_lineage_segments AS
WITH RECURSIVE ord AS MATERIALIZED (
    SELECT sha, ord FROM commit_order
),
successions AS MATERIALIZED (
    SELECT s.*, o.ord,
           count(*) OVER (PARTITION BY s.sha, s.pred_path, s.pred_kind, s.pred_qualified_name) AS successors
    FROM unit_successions s JOIN ord o USING (sha) WHERE s.link <> 'same_name'
),
-- Where a lineage restarts: the stub of a body_move, the unit that kept the name (its
-- post-image, under the file's new path when the file was renamed in that commit).
restarts AS MATERIALIZED (
    SELECT DISTINCT s.ord, coalesce(f.path, s.pred_path) AS path, s.pred_kind AS kind,
           s.pred_qualified_name AS qualified_name
    FROM successions s
    LEFT JOIN file_changes f ON f.sha = s.sha AND f.old_path = s.pred_path AND f.path <> f.old_path
    WHERE s.link = 'body_move'
),
links AS MATERIALIZED (
    SELECT s.*,
           coalesce((SELECT max(r.ord) FROM restarts r
                     WHERE r.path = s.pred_path AND r.kind = s.pred_kind
                       AND r.qualified_name = s.pred_qualified_name AND r.ord < s.ord), 0) AS pred_from
    FROM successions s
),
current_units AS MATERIALIZED (
    SELECT u.unit_id, f.path, u.kind, u.qualified_name,
           coalesce((SELECT max(r.ord) FROM restarts r
                     WHERE r.path = f.path AND r.kind = u.kind AND r.qualified_name = u.qualified_name), 0) AS from_ord
    FROM units u JOIN files f USING (file_id)
),
walk(unit_id, path, kind, qualified_name, from_ord, until_ord, depth, link, sha, successors,
     self_path, self_kind, self_qualified_name) AS (
    SELECT unit_id, path, kind, qualified_name, from_ord, 9223372036854775807::BIGINT, 0, NULL::VARCHAR,
           NULL::VARCHAR, 1::BIGINT, path, kind, qualified_name
    FROM current_units
    UNION
    SELECT w.unit_id, l.pred_path, l.pred_kind, l.pred_qualified_name, l.pred_from, l.ord - 1, w.depth + 1, l.link,
           l.sha, l.successors, w.self_path, w.self_kind, w.self_qualified_name
    FROM walk w
    JOIN links l ON l.succ_path = w.path AND l.succ_kind = w.kind AND l.succ_qualified_name = w.qualified_name
     AND l.ord > w.from_ord AND l.ord <= w.until_ord
)
SELECT unit_id, path, kind, qualified_name, from_ord, until_ord, depth, link, sha, successors,
       path = self_path AND kind = self_kind AND qualified_name = self_qualified_name AS is_self
FROM walk;

-- unit_lineage(unit_id, sha, path, kind, qualified_name, depth, link, from_sha): every
-- identity in each current unit's lineage, newest first: depth 0 is the unit itself
-- (sha and link NULL), depth n an identity n links back, reached through `link` at
-- commit `sha` (it contributes its commits before sha). from_sha is the body_move
-- commit where that identity's own lineage restarted (its commits from there on), NULL
-- when it reaches back to the start of the range. An identity reached along several
-- paths is listed once per (sha, link), at its least depth.
CREATE OR REPLACE VIEW unit_lineage AS
SELECT w.unit_id, w.sha, w.path, w.kind, w.qualified_name, min(w.depth) AS depth, w.link, o.sha AS from_sha
FROM unit_lineage_segments w
LEFT JOIN commit_order o ON o.ord = w.from_ord
GROUP BY w.unit_id, w.sha, w.path, w.kind, w.qualified_name, w.link, o.sha;

-- unit_history(unit_id, sha, change_kind, depth, via_link): the distinct commits of
-- unit_changes that touched any identity of each current unit's lineage within the range
-- it contributes; change_kind and depth are those of the nearest such identity. via_link
-- marks a commit at which the unit's lineage links one of its identities to its
-- predecessor (a file_rename, content_match or body_move of unit_lineage's `sha`): the
-- commit that moved, renamed, split or merged the code, counted here because it touched
-- the successor. The co-change views leave these commits out (views/graph.sql,
-- unit_commits); unit_churn counts them and reports how many (move_commits).
CREATE OR REPLACE VIEW unit_history AS
WITH segments AS MATERIALIZED (
    SELECT * FROM unit_lineage_segments
),
link_commits AS MATERIALIZED (
    SELECT DISTINCT unit_id, sha FROM segments WHERE sha IS NOT NULL
)
SELECT w.unit_id, uc.sha, (list(uc.change_kind ORDER BY w.depth, uc.change_kind))[1] AS change_kind,
       min(w.depth) AS depth, bool_or(l.sha IS NOT NULL) AS via_link
FROM segments w
JOIN unit_changes uc ON uc.path = w.path AND uc.unit_kind = w.kind AND uc.unit_qualified_name = w.qualified_name
JOIN commit_order o ON o.sha = uc.sha AND o.ord >= w.from_ord AND o.ord <= w.until_ord
LEFT JOIN link_commits l ON l.unit_id = w.unit_id AND l.sha = uc.sha
GROUP BY w.unit_id, uc.sha;

-- unit_lineage_counts(unit_id, ancestors, successors_at_split): for each current unit,
-- the number of distinct identities other than its own in its lineage, and the largest
-- number of successors any link of its lineage gave its predecessor at that commit (1:
-- no split; a unit whose history it shares with others has more).
CREATE OR REPLACE VIEW unit_lineage_counts AS
SELECT unit_id,
       count(DISTINCT (path, kind, qualified_name)) FILTER (WHERE NOT is_self) AS ancestors,
       max(successors) AS successors_at_split
FROM unit_lineage_segments
GROUP BY unit_id;
