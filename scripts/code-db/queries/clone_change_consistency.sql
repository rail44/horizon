-- Question: for each clone pair, how often did history change one side's unit
-- without the other, and how often both together?
-- Sides: the smallest current unit whose row range contains the whole fragment
-- (NULL when the fragment spans units or lies outside any unit; such sides get
-- no commits). Commits: non-merge commits in unit_changes for that unit's
-- (path, kind, qualified_name). same_unit marks pairs whose sides resolve to
-- one unit, where every commit counts as "both".
-- Evidence: inconsistent change of clones is the clone-harm signal with
-- published support (Juergens et al. ICSE 2009: about half of unintentional
-- inconsistencies were faults; Mondal et al. on late propagation); clone
-- length is not a supported predictor (Wagner et al. 2016). Not studied in Rust.
-- @param source null
-- @param include_merges 0
WITH sides AS (
    SELECT pair_id, 'a' AS side, file_a AS path, start_row_a AS s, end_row_a AS e FROM clone_pairs
    UNION ALL
    SELECT pair_id, 'b', file_b, start_row_b, end_row_b FROM clone_pairs
),
enclosing AS (
    SELECT s.pair_id, s.side, u.unit_id, u.kind, u.qualified_name,
           row_number() OVER (PARTITION BY s.pair_id, s.side ORDER BY u.end_row - u.start_row, u.unit_id DESC) AS rn
    FROM sides s
    JOIN files f ON f.path = s.path
    JOIN units u ON u.file_id = f.file_id AND u.start_row <= s.s AND u.end_row >= s.e
),
resolved AS (
    SELECT s.pair_id, s.side, s.path, e.unit_id, e.kind, e.qualified_name
    FROM sides s
    LEFT JOIN enclosing e ON e.pair_id = s.pair_id AND e.side = s.side AND e.rn = 1
),
changes AS (
    SELECT DISTINCT uc.sha, uc.path, uc.unit_kind, uc.unit_qualified_name
    FROM unit_changes uc
    JOIN commits c USING (sha)
    WHERE NOT c.is_merge OR $include_merges = 1
),
per_commit AS (
    SELECT r.pair_id, ch.sha, bool_or(r.side = 'a') AS has_a, bool_or(r.side = 'b') AS has_b
    FROM resolved r
    JOIN changes ch
      ON ch.path = r.path AND ch.unit_kind = r.kind AND ch.unit_qualified_name = r.qualified_name
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
SELECT cp.pair_id, cp.group_id, cp.source,
       cp.file_a, cp.start_row_a + 1 AS start_line_a, a.kind AS kind_a, a.qualified_name AS unit_a,
       cp.file_b, cp.start_row_b + 1 AS start_line_b, b.kind AS kind_b, b.qualified_name AS unit_b,
       cp.tokens, cp.lines,
       a.unit_id IS NOT NULL AND a.unit_id = b.unit_id AS same_unit,
       coalesce(k.only_a, 0) AS only_a, coalesce(k.only_b, 0) AS only_b,
       coalesce(k.both_sides, 0) AS both_sides,
       coalesce(k.only_a, 0) + coalesce(k.only_b, 0) AS one_sided
FROM clone_pairs cp
JOIN resolved a ON a.pair_id = cp.pair_id AND a.side = 'a'
JOIN resolved b ON b.pair_id = cp.pair_id AND b.side = 'b'
LEFT JOIN counts k ON k.pair_id = cp.pair_id
WHERE CAST($source AS VARCHAR) IS NULL OR cp.source = CAST($source AS VARCHAR)
ORDER BY one_sided DESC, both_sides DESC, cp.pair_id
