-- Question: in which kind of construct does each side of each clone pair sit?
-- A side carries every tag whose tagged node (in the same file) contains its
-- whole row range (match_mode = contain) or overlaps it (match_mode = overlap);
-- 'none' when no tag applies. Labels join sorted tags with '+'.
-- Evidence: in Rust, per-type copies of trait implementations are the most
-- common Type-1/2 clone kind, and inline tests contribute many more
-- (Pizzolotto et al. 2022, 15 crates, not peer reviewed). Whether separating
-- them improves review precision was not measured.
-- @param match_mode contain
-- @param source null
WITH tagged AS (
    SELECT t.file_id, t.tag, n.start_row, n.end_row
    FROM tags t JOIN nodes n USING (file_id, node_id)
),
sides AS (
    SELECT pair_id, 'a' AS side, file_a AS path, start_row_a AS s, end_row_a AS e FROM clone_pairs
    UNION ALL
    SELECT pair_id, 'b', file_b, start_row_b, end_row_b FROM clone_pairs
),
side_tags AS (
    SELECT s.pair_id, s.side,
           coalesce(nullif(array_to_string(list_sort(list_distinct(list(tg.tag))), '+'), ''), 'none') AS label
    FROM sides s
    JOIN files f ON f.path = s.path
    LEFT JOIN tagged tg
      ON tg.file_id = f.file_id
     AND (CASE WHEN $match_mode = 'overlap' THEN tg.start_row <= s.e AND tg.end_row >= s.s
               ELSE tg.start_row <= s.s AND tg.end_row >= s.e END)
    GROUP BY s.pair_id, s.side
)
SELECT cp.pair_id, cp.group_id, cp.source,
       cp.file_a, cp.start_row_a + 1 AS start_line_a, a.label AS tags_a,
       cp.file_b, cp.start_row_b + 1 AS start_line_b, b.label AS tags_b,
       cp.tokens, cp.lines
FROM clone_pairs cp
JOIN side_tags a ON a.pair_id = cp.pair_id AND a.side = 'a'
JOIN side_tags b ON b.pair_id = cp.pair_id AND b.side = 'b'
WHERE CAST($source AS VARCHAR) IS NULL OR cp.source = CAST($source AS VARCHAR)
ORDER BY cp.source, cp.pair_id
