-- Question: for each clone pair, how often did history change one side's units
-- without the other's, and how often both together?
-- Sides follow `clone_sides`:
--   overlap (default)  a side's units are the innermost units whose own lines overlap
--                      the fragment's lines. A unit's own lines are [lead_start_line,
--                      end_line] (its leading doc comments and attributes included,
--                      lang/rust.sql) minus the same lines of its direct inner units, the
--                      spans innermost attribution credits (views/unit_changes.sql), blank
--                      lines kept. A fragment spanning three methods maps to those three,
--                      plus their impl only when it overlaps the impl's own lines (its
--                      header, its closing line, the lines between its methods).
--   contain            the smallest current unit whose line range contains the whole
--                      fragment (none when the fragment spans units), as before this rule.
-- A fragment outside every unit maps to none; such a side gets no commits. Commits: the
-- commits of each unit's lineage (unit_history, views/lineage.sql). A commit is only_a
-- when it touched any of side a's units and none of side b's, only_b symmetrically, and
-- both_sides when it touched both sets (a unit in both sets counts for both).
-- Columns: kind_a/unit_a (kind_b/unit_b) name the side's unit with the most own lines in
-- the fragment (ties: the smaller unit, then the later unit_id; under contain, its one
-- unit); units_a and units_b count each side's units and shared_units the units in both
-- sets; side_units_a and side_units_b list them as {kind, qualified_name}, the
-- representative first. same_unit marks pairs whose two sets are identical (under
-- contain: one unit), where every commit counts as "both"; it is false when side a has
-- no unit and NULL when only side b has none.
-- Changes are attributed to the units, not to the fragment itself.
-- Counted from when both sides exist: only the commits at or after since_sha, the latest
-- first commit in its lineage (the first commit of unit_history) of any unit of either
-- side, count as one-sided or both; earlier commits changed a side whose partner, or a
-- part of either, did not exist yet. A unit with no commit in the range sets no bound
-- (since_sha NULL when no unit of either side has one: every commit counts).
-- Evidence: inconsistent change of clones is the clone-harm signal with
-- published support (Juergens et al. ICSE 2009: about half of unintentional
-- inconsistencies were faults; Mondal et al. on late propagation); clone
-- length is not a supported predictor (Wagner et al. 2016). Not studied in Rust.
-- Parameters (DuckDB variables; default when unset):
--   clone_sides 'overlap' ('contain': the sides as before this rule);
--   clone_source NULL (all reports; or a source label such as 'exact');
--   include_merges (00_load.sql, false) also counts merges against their first parent;
--   lineage_threshold (views/lineage.sql, 0.5); attribution (views/unit_changes.sql,
--   'innermost');
--   clone_before_both false (true: also count the commits before since_sha, as before
--   that rule; since_sha is then still reported).
WITH params AS MATERIALIZED (
    SELECT CAST(getvariable('clone_source') AS VARCHAR) AS clone_source,
           coalesce(CAST(getvariable('clone_before_both') AS BOOLEAN), false) AS before_both,
           CASE WHEN coalesce(getvariable('clone_sides'), 'overlap') IN ('overlap', 'contain')
                THEN coalesce(getvariable('clone_sides'), 'overlap')
                ELSE error('clone_change_consistency: clone_sides is overlap or contain') END AS clone_sides
),
sides AS MATERIALIZED (
    SELECT s.pair_id, s.side, f.file_id, s.s, s.e
    FROM (
        SELECT pair_id, 'a' AS side, file_a AS path, start_line_a AS s, end_line_a AS e FROM clone_pairs
        UNION ALL
        SELECT pair_id, 'b', file_b, start_line_b, end_line_b FROM clone_pairs
    ) s
    JOIN files f ON f.path = s.path
),
-- The units of the sides' files, read once.
side_file_units AS MATERIALIZED (
    SELECT unit_id, file_id, parent_unit_id, kind, qualified_name, start_line, end_line, lead_start_line
    FROM units
    WHERE file_id IN (SELECT file_id FROM sides)
),
-- contain: the smallest unit containing the whole fragment.
contained AS (
    SELECT s.pair_id, s.side, u.unit_id, u.end_line - u.start_line AS size, 0 AS own_lines
    FROM sides s
    JOIN side_file_units u ON u.file_id = s.file_id AND u.start_line <= s.s AND u.end_line >= s.e
    WHERE (SELECT clone_sides FROM params) = 'contain'
    QUALIFY row_number() OVER (PARTITION BY s.pair_id, s.side
                               ORDER BY u.end_line - u.start_line, u.unit_id DESC) = 1
),
-- overlap: the own lines of the units of the sides' files, as spans [s, e]: a unit's
-- range minus the ranges of its direct inner units (prev_end is the furthest end among
-- the inner units before this one in line order, so inner units sharing a line leave no
-- gap), as views/unit_changes.sql's own_spans.
unit_rows AS MATERIALIZED (
    SELECT unit_id, file_id, parent_unit_id, lead_start_line AS start_line, end_line,
           end_line - start_line AS size
    FROM side_file_units
    WHERE (SELECT clone_sides FROM params) = 'overlap'
),
inner_units AS MATERIALIZED (
    SELECT parent_unit_id, start_line, end_line,
           max(end_line) OVER (PARTITION BY parent_unit_id ORDER BY start_line, unit_id
                               ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS prev_end
    FROM unit_rows
    WHERE parent_unit_id IS NOT NULL
),
own_spans AS MATERIALIZED (
    SELECT * FROM (
        SELECT p.file_id, p.unit_id, p.size, greatest(coalesce(c.prev_end + 1, p.start_line), p.start_line) AS s,
               c.start_line - 1 AS e
        FROM inner_units c
        JOIN unit_rows p ON p.unit_id = c.parent_unit_id
        UNION ALL
        SELECT p.file_id, p.unit_id, p.size, greatest(coalesce(max(c.end_line) + 1, p.start_line), p.start_line),
               p.end_line
        FROM unit_rows p
        LEFT JOIN inner_units c ON c.parent_unit_id = p.unit_id
        GROUP BY p.file_id, p.unit_id, p.size, p.start_line, p.end_line
    )
    WHERE s <= e
),
overlapping AS (
    SELECT s.pair_id, s.side, o.unit_id, any_value(o.size) AS size,
           sum(least(o.e, s.e) - greatest(o.s, s.s) + 1) AS own_lines
    FROM sides s
    JOIN own_spans o ON o.file_id = s.file_id AND o.s <= s.e AND o.e >= s.s
    GROUP BY s.pair_id, s.side, o.unit_id
),
-- Every unit of every side; rn 1 is the side's representative.
resolved AS MATERIALIZED (
    SELECT x.pair_id, x.side, x.unit_id, u.kind, u.qualified_name,
           row_number() OVER (PARTITION BY x.pair_id, x.side
                              ORDER BY x.own_lines DESC, x.size, x.unit_id DESC) AS rn
    FROM (SELECT * FROM contained UNION ALL SELECT * FROM overlapping) x
    JOIN side_file_units u ON u.unit_id = x.unit_id
),
side_sets AS (
    SELECT pair_id, side, count(*) AS units,
           list({'kind': kind, 'qualified_name': qualified_name} ORDER BY rn) AS side_units,
           list(unit_id ORDER BY unit_id) AS unit_ids,
           any_value(kind) FILTER (WHERE rn = 1) AS kind,
           any_value(qualified_name) FILTER (WHERE rn = 1) AS qualified_name
    FROM resolved
    GROUP BY pair_id, side
),
shared AS (
    SELECT a.pair_id, count(*) AS shared_units
    FROM resolved a
    JOIN resolved b ON b.pair_id = a.pair_id AND b.unit_id = a.unit_id
    WHERE a.side = 'a' AND b.side = 'b'
    GROUP BY a.pair_id
),
history AS MATERIALIZED (
    SELECT h.unit_id, h.sha, o.ord
    FROM unit_history h
    JOIN commit_order o USING (sha)
),
changes AS (
    SELECT h.unit_id, h.sha, h.ord
    FROM history h
    JOIN commits c USING (sha)
    WHERE NOT c.is_merge OR getvariable('include_merges')
),
-- Each pair's bound: the latest first commit of any unit of its sides.
first_seen AS (
    SELECT unit_id, min(ord) AS first_ord FROM history GROUP BY unit_id
),
since AS (
    SELECT r.pair_id, max(f.first_ord) AS since_ord
    FROM resolved r
    JOIN first_seen f USING (unit_id)
    GROUP BY r.pair_id
),
per_commit AS (
    SELECT r.pair_id, ch.sha, bool_or(r.side = 'a') AS has_a, bool_or(r.side = 'b') AS has_b
    FROM resolved r
    JOIN changes ch ON ch.unit_id = r.unit_id
    CROSS JOIN params
    LEFT JOIN since s ON s.pair_id = r.pair_id
    WHERE params.before_both OR s.since_ord IS NULL OR ch.ord >= s.since_ord
    GROUP BY ALL
),
counts AS (
    SELECT pair_id,
           count(*) FILTER (WHERE has_a AND NOT has_b) AS only_a,
           count(*) FILTER (WHERE has_b AND NOT has_a) AS only_b,
           count(*) FILTER (WHERE has_a AND has_b) AS both_sides
    FROM per_commit
    GROUP BY pair_id
)
SELECT cp.pair_id, cp.source,
       cp.file_a, cp.start_line_a, a.kind AS kind_a, a.qualified_name AS unit_a,
       cp.file_b, cp.start_line_b, b.kind AS kind_b, b.qualified_name AS unit_b,
       cp.tokens, cp.lines,
       CASE WHEN a.pair_id IS NULL THEN false
            WHEN b.pair_id IS NULL THEN NULL
            ELSE a.unit_ids = b.unit_ids END AS same_unit,
       coalesce(k.only_a, 0) AS only_a, coalesce(k.only_b, 0) AS only_b,
       coalesce(k.both_sides, 0) AS both_sides,
       coalesce(k.only_a, 0) + coalesce(k.only_b, 0) AS one_sided,
       o.sha AS since_sha, c.author_time AS since_time,
       coalesce(a.units, 0) AS units_a, coalesce(b.units, 0) AS units_b,
       coalesce(sh.shared_units, 0) AS shared_units,
       coalesce(a.side_units, []) AS side_units_a, coalesce(b.side_units, []) AS side_units_b
FROM clone_pairs cp
LEFT JOIN side_sets a ON a.pair_id = cp.pair_id AND a.side = 'a'
LEFT JOIN side_sets b ON b.pair_id = cp.pair_id AND b.side = 'b'
LEFT JOIN shared sh ON sh.pair_id = cp.pair_id
LEFT JOIN counts k ON k.pair_id = cp.pair_id
LEFT JOIN since s ON s.pair_id = cp.pair_id
LEFT JOIN commit_order o ON o.ord = s.since_ord
LEFT JOIN commits c ON c.sha = o.sha
CROSS JOIN params
WHERE params.clone_source IS NULL OR cp.source = params.clone_source
ORDER BY one_sided DESC, both_sides DESC, cp.pair_id;
