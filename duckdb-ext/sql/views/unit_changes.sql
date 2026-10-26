-- unit_changes(sha, path, unit_kind, unit_qualified_name, change_kind): which units each
-- commit touched, from its hunks and the units of the blobs on both sides of the
-- change (views/blobs.sql, blob_units). It is a projection of unit_change_sides (below),
-- which keeps every unit of both sides with its lines.
--
-- For every (sha, path) with hunks (hunks.path is the new path, or the old one for a
-- deletion; only extensions in `languages`):
--   - a unit of the new side's blob whose lines [start_line, end_line] intersect a
--     hunk's new range [new_start, new_start + new_lines - 1] is `modified` when a unit
--     with the same (kind, qualified_name) exists on the old side, else `added`;
--   - a unit of the old side's blob whose lines intersect a hunk's old range
--     [old_start, old_start + old_lines - 1], and that is not already classified, is
--     `modified` when it still exists on the new side (recorded under `path`), else
--     `removed` (recorded under old_path, or path when there is no rename).
-- A hunk with new_lines = 0 (old_lines = 0) intersects nothing on the new (old) side:
-- git's -U0 start is then the line before the deletion (insertion). Lines are 1-based
-- and inclusive on both sides. Unit identity is (kind, qualified_name) within a path:
-- two units with the same kind and name in one blob are one unit, and a rename starts a
-- new history under the new path. A pure rename or a mode change has no hunks and no
-- rows; merges follow `include_merges`, like the history views. One row per distinct
-- tuple. This is the spike's unit_changes (git show 475599d5:scripts/code-db/history.py)
-- on 1-based lines.

-- unit_change_sides(sha, path, old_path, side, blob_id, kind, qualified_name, start_line,
--     end_line, record_path, change_kind): every unit of either side of every (sha, path)
-- with hunks (one row per unit, so two units with one identity are two rows), with the
-- change unit_changes records for its identity (change_kind, under record_path), NULL
-- when the commit did not touch it. Both sides of a modified unit carry the change.
-- views/lineage.sql reads it, so that the units of the blobs are derived once per query.
CREATE OR REPLACE VIEW unit_change_sides AS
WITH hunk_files AS MATERIALIZED (
    SELECT DISTINCT sha, path FROM hunks
),
-- The units of each side of each changed file with hunks.
side_units AS MATERIALIZED (
    SELECT b.sha, b.path, b.old_path, b.side, b.blob_id, u.kind, u.qualified_name, u.start_line, u.end_line
    FROM blob_files b
    JOIN hunk_files h ON h.sha = b.sha AND h.path = b.path
    JOIN blob_units u ON u.blob_id = b.blob_id
),
touched AS MATERIALIZED (
    SELECT DISTINCT u.sha, u.path, u.old_path, u.side, u.kind, u.qualified_name
    FROM side_units u
    JOIN hunks h ON h.sha = u.sha AND h.path = u.path
    WHERE CASE u.side
        WHEN 'new' THEN h.new_lines > 0 AND h.new_start <= u.end_line
                        AND h.new_start + h.new_lines - 1 >= u.start_line
        ELSE h.old_lines > 0 AND h.old_start <= u.end_line
             AND h.old_start + h.old_lines - 1 >= u.start_line
    END
),
present AS MATERIALIZED (
    SELECT DISTINCT sha, path, side, kind, qualified_name FROM side_units
),
-- New-side units, then old-side units not already classified.
classified AS MATERIALIZED (
    SELECT t.sha, t.path, t.kind AS unit_kind, t.qualified_name AS unit_qualified_name,
           CASE WHEN p.kind IS NULL THEN 'added' ELSE 'modified' END AS change_kind
    FROM touched t
    LEFT JOIN present p ON p.sha = t.sha AND p.path = t.path AND p.side = 'old'
         AND p.kind = t.kind AND p.qualified_name = t.qualified_name
    WHERE t.side = 'new'
    UNION ALL
    SELECT t.sha,
           CASE WHEN p.kind IS NULL THEN coalesce(t.old_path, t.path) ELSE t.path END,
           t.kind, t.qualified_name,
           CASE WHEN p.kind IS NULL THEN 'removed' ELSE 'modified' END
    FROM touched t
    LEFT JOIN present p ON p.sha = t.sha AND p.path = t.path AND p.side = 'new'
         AND p.kind = t.kind AND p.qualified_name = t.qualified_name
    WHERE t.side = 'old'
      AND NOT EXISTS (SELECT 1 FROM touched n
                      WHERE n.sha = t.sha AND n.path = t.path AND n.side = 'new'
                        AND n.kind = t.kind AND n.qualified_name = t.qualified_name)
),
changes AS MATERIALIZED (
    SELECT DISTINCT * FROM classified
)
SELECT u.sha, u.path, u.old_path, u.side, u.blob_id, u.kind, u.qualified_name, u.start_line, u.end_line,
       c.path AS record_path, c.change_kind
FROM side_units u
LEFT JOIN changes c ON c.sha = u.sha AND c.unit_kind = u.kind AND c.unit_qualified_name = u.qualified_name
     AND c.path = CASE WHEN c.change_kind = 'removed' THEN coalesce(u.old_path, u.path) ELSE u.path END
     AND (c.change_kind = 'modified' OR (c.change_kind = 'added') = (u.side = 'new'));

CREATE OR REPLACE VIEW unit_changes AS
SELECT DISTINCT sha, record_path AS path, kind AS unit_kind, qualified_name AS unit_qualified_name, change_kind
FROM unit_change_sides
WHERE change_kind IS NOT NULL;
