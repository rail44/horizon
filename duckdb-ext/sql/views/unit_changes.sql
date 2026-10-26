-- unit_changes(sha, path, unit_kind, unit_qualified_name, change_kind): which units each
-- commit touched, from its hunks and the units of the blobs on both sides of the
-- change (views/blobs.sql, blob_units). It is a projection of unit_change_sides (below),
-- which keeps every unit of both sides with its lines.
--
-- For every (sha, path) with hunks (hunks.path is the new path, or the old one for a
-- deletion; only extensions in `languages`):
--   - a unit of the new side's blob whose credited lines intersect a hunk's new range
--     [new_start, new_start + new_lines - 1] is `modified` when a unit with the same
--     (kind, qualified_name) exists on the old side, else `added`;
--   - a unit of the old side's blob whose credited lines intersect a hunk's old range
--     [old_start, old_start + old_lines - 1], and that is not already classified, is
--     `modified` when it still exists on the new side (recorded under `path`), else
--     `removed` (recorded under old_path, or path when there is no rename).
-- A unit's credited lines follow `attribution`:
--   innermost (default)  its lines [lead_start_line, end_line] (its leading doc comments
--                        and attributes included, lang/rust.sql) minus the same lines of
--                        the units nested in it (blob_units.parent_unit_ord), and minus the
--                        lines that are blank in that side's text: a changed line credits
--                        only the innermost unit containing it, so an impl, module or type
--                        is credited for its own doc comments and attributes, its header,
--                        its closing line and the non-blank lines between its inner units
--                        (plain comments, fields, other items), not for a change inside a
--                        method or to a method's doc comments; a hunk of blank lines
--                        credits no unit;
--   all                  every line [start_line, end_line]: an enclosing unit is credited
--                        with every change inside it, blank lines included, and leading
--                        doc comments and attributes credit no unit but an enclosing one.
-- A hunk with new_lines = 0 (old_lines = 0) intersects nothing on the new (old) side:
-- git's -U0 start is then the line before the deletion (insertion). Lines are 1-based
-- and inclusive on both sides. Unit identity is (kind, qualified_name) within a path:
-- two units with the same kind and name in one blob are one unit, and a rename starts a
-- new history under the new path. A pure rename or a mode change has no hunks and no
-- rows; merges follow `include_merges`, like the history views. One row per distinct
-- tuple. With attribution 'all' this is the spike's unit_changes (git show
-- 475599d5:scripts/code-db/history.py) on 1-based lines.

-- unit_change_sides(sha, path, old_path, side, blob_id, kind, qualified_name, start_line,
--     end_line, record_path, change_kind, record_path_all, change_kind_all): every unit of
-- either side of every (sha, path) with hunks (one row per unit, so two units with one
-- identity are two rows), with the change unit_changes records for its identity under
-- `attribution` (change_kind, under record_path) and under attribution 'all'
-- (change_kind_all, under record_path_all); NULL when the commit did not touch it. Both
-- sides of a modified unit carry the change. views/lineage.sql reads it (the `_all`
-- columns, whatever `attribution` is), so that the units of the blobs are derived once
-- per query.
-- Blank lines are those of blob_blank_lines (views/blobs.sql) for that side's blob.
-- Parameters (DuckDB variables; default when unset): attribution 'innermost' (or 'all').
CREATE OR REPLACE VIEW unit_change_sides AS
WITH params AS MATERIALIZED (
    SELECT CASE WHEN coalesce(getvariable('attribution'), 'innermost') IN ('innermost', 'all')
                THEN coalesce(getvariable('attribution'), 'innermost')
                ELSE error('unit_change_sides: attribution is innermost or all') END AS attribution
),
hunk_files AS MATERIALIZED (
    SELECT DISTINCT sha, path FROM hunks
),
-- The units of each side of each changed file with hunks.
side_units AS MATERIALIZED (
    SELECT b.sha, b.path, b.old_path, b.side, b.blob_id, u.unit_ord, u.parent_unit_ord, u.kind,
           u.qualified_name, u.start_line, u.end_line, u.lead_start_line
    FROM blob_files b
    JOIN hunk_files h ON h.sha = b.sha AND h.path = b.path
    JOIN blob_units u ON u.blob_id = b.blob_id
),
-- The lines each unit of those blobs owns under innermost attribution, as spans [s, e]:
-- its range [lead_start_line, end_line] minus the same ranges of its direct inner units
-- (before each inner unit, after the last one, or the whole range when it has none).
-- prev_end is the furthest end among the inner units before this one in line order, so
-- inner units sharing a line leave no gap. Blank lines are dropped below.
blob_unit_rows AS MATERIALIZED (
    SELECT DISTINCT blob_id, unit_ord, parent_unit_ord, lead_start_line AS start_line, end_line
    FROM side_units
),
inner_units AS MATERIALIZED (
    SELECT blob_id, parent_unit_ord, start_line, end_line,
           max(end_line) OVER (PARTITION BY blob_id, parent_unit_ord ORDER BY start_line, unit_ord
                               ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS prev_end
    FROM blob_unit_rows
    WHERE parent_unit_ord IS NOT NULL
),
own_spans AS MATERIALIZED (
    SELECT * FROM (
        SELECT p.blob_id, p.unit_ord, greatest(coalesce(c.prev_end + 1, p.start_line), p.start_line) AS s,
               c.start_line - 1 AS e
        FROM inner_units c
        JOIN blob_unit_rows p ON p.blob_id = c.blob_id AND p.unit_ord = c.parent_unit_ord
        UNION ALL
        SELECT p.blob_id, p.unit_ord, greatest(coalesce(max(c.end_line) + 1, p.start_line), p.start_line),
               p.end_line
        FROM blob_unit_rows p
        LEFT JOIN inner_units c ON c.blob_id = p.blob_id AND c.parent_unit_ord = p.unit_ord
        GROUP BY p.blob_id, p.unit_ord, p.start_line, p.end_line
    )
    WHERE s <= e
),
-- Under innermost attribution, the lines [lo, hi] each hunk changes in each own span; a
-- hit credits its unit unless every one of those lines is blank. The spans and the
-- hunks' sides are materialized first, so that the hits are one join on (sha, path,
-- side): live, a plan that joined own_spans to the hunks on the line ranges alone
-- peaked above 30 GB on this repository.
side_spans AS MATERIALIZED (
    SELECT u.sha, u.path, u.old_path, u.side, u.blob_id, u.kind, u.qualified_name, o.s, o.e
    FROM side_units u
    JOIN own_spans o ON o.blob_id = u.blob_id AND o.unit_ord = u.unit_ord
),
side_hunks AS MATERIALIZED (
    SELECT sha, path, 'new' AS side, new_start AS hs, new_start + new_lines - 1 AS he
    FROM hunks WHERE new_lines > 0
    UNION ALL
    SELECT sha, path, 'old', old_start, old_start + old_lines - 1
    FROM hunks WHERE old_lines > 0
),
own_hits AS MATERIALIZED (
    SELECT DISTINCT u.sha, u.path, u.old_path, u.side, u.blob_id, u.kind, u.qualified_name,
           greatest(u.s, h.hs) AS lo, least(u.e, h.he) AS hi
    FROM side_spans u
    JOIN side_hunks h ON h.sha = u.sha AND h.path = u.path AND h.side = u.side AND h.hs <= u.e AND h.he >= u.s
),
own_credited AS MATERIALIZED (
    SELECT i.sha, i.path, i.old_path, i.side, i.kind, i.qualified_name
    FROM own_hits i
    LEFT JOIN blob_blank_lines b ON b.blob_id = i.blob_id AND b.line BETWEEN i.lo AND i.hi
    GROUP BY i.sha, i.path, i.old_path, i.side, i.kind, i.qualified_name, i.blob_id, i.lo, i.hi
    HAVING count(b.line) < i.hi - i.lo + 1
),
-- The identities each hunk touches, under both attributions (`mode`).
touched AS MATERIALIZED (
    SELECT DISTINCT 'all' AS mode, u.sha, u.path, u.old_path, u.side, u.kind, u.qualified_name
    FROM side_units u
    JOIN hunks h ON h.sha = u.sha AND h.path = u.path
    WHERE CASE u.side
        WHEN 'new' THEN h.new_lines > 0 AND h.new_start <= u.end_line
                        AND h.new_start + h.new_lines - 1 >= u.start_line
        ELSE h.old_lines > 0 AND h.old_start <= u.end_line
             AND h.old_start + h.old_lines - 1 >= u.start_line
    END
    UNION ALL
    SELECT DISTINCT 'innermost', sha, path, old_path, side, kind, qualified_name FROM own_credited
),
present AS MATERIALIZED (
    SELECT DISTINCT sha, path, side, kind, qualified_name FROM side_units
),
-- New-side units, then old-side units not already classified, per mode.
classified AS MATERIALIZED (
    SELECT t.mode, t.sha, t.path, t.kind AS unit_kind, t.qualified_name AS unit_qualified_name,
           CASE WHEN p.kind IS NULL THEN 'added' ELSE 'modified' END AS change_kind
    FROM touched t
    LEFT JOIN present p ON p.sha = t.sha AND p.path = t.path AND p.side = 'old'
         AND p.kind = t.kind AND p.qualified_name = t.qualified_name
    WHERE t.side = 'new'
    UNION ALL
    SELECT t.mode, t.sha,
           CASE WHEN p.kind IS NULL THEN coalesce(t.old_path, t.path) ELSE t.path END,
           t.kind, t.qualified_name,
           CASE WHEN p.kind IS NULL THEN 'removed' ELSE 'modified' END
    FROM touched t
    LEFT JOIN present p ON p.sha = t.sha AND p.path = t.path AND p.side = 'new'
         AND p.kind = t.kind AND p.qualified_name = t.qualified_name
    WHERE t.side = 'old'
      AND NOT EXISTS (SELECT 1 FROM touched n
                      WHERE n.mode = t.mode AND n.sha = t.sha AND n.path = t.path AND n.side = 'new'
                        AND n.kind = t.kind AND n.qualified_name = t.qualified_name)
),
changes AS MATERIALIZED (
    SELECT DISTINCT * FROM classified
)
SELECT u.sha, u.path, u.old_path, u.side, u.blob_id, u.kind, u.qualified_name, u.start_line, u.end_line,
       c.path AS record_path, c.change_kind, a.path AS record_path_all, a.change_kind AS change_kind_all
FROM side_units u
CROSS JOIN params
LEFT JOIN changes c ON c.mode = params.attribution
     AND c.sha = u.sha AND c.unit_kind = u.kind AND c.unit_qualified_name = u.qualified_name
     AND c.path = CASE WHEN c.change_kind = 'removed' THEN coalesce(u.old_path, u.path) ELSE u.path END
     AND (c.change_kind = 'modified' OR (c.change_kind = 'added') = (u.side = 'new'))
LEFT JOIN changes a ON a.mode = 'all'
     AND a.sha = u.sha AND a.unit_kind = u.kind AND a.unit_qualified_name = u.qualified_name
     AND a.path = CASE WHEN a.change_kind = 'removed' THEN coalesce(u.old_path, u.path) ELSE u.path END
     AND (a.change_kind = 'modified' OR (a.change_kind = 'added') = (u.side = 'new'));

CREATE OR REPLACE VIEW unit_changes AS
SELECT DISTINCT sha, record_path AS path, kind AS unit_kind, qualified_name AS unit_qualified_name, change_kind
FROM unit_change_sides
WHERE change_kind IS NOT NULL;
